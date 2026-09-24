use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

// ======================== 系统管理（M12 SY，docs/impl/06） ========================

pub(crate) fn sys_err(e: sys_core::SysError) -> AppError {
    AppError::module(e.code(), e.to_string(), None)
}

/// 包管理器源信息（SY1 探测）
#[derive(serde::Serialize)]
pub struct PkgSourceDto {
    pub id: String,
    pub label: String,
    pub available: bool,
}

/// 可用包管理器列表
#[tauri::command]
pub async fn sys_pkg_sources(state: State<'_, HostState>) -> Result<Vec<PkgSourceDto>, AppError> {
    Ok(state
        .sys
        .managers()
        .iter()
        .map(|m| PkgSourceDto {
            id: m.id().to_string(),
            label: m.label().to_string(),
            available: m.available(),
        })
        .collect())
}

/// 已装清单合并视图（SY2：多源去重，winget 优先）
#[tauri::command]
pub async fn sys_pkg_list(
    state: State<'_, HostState>,
) -> Result<Vec<sys_core::PkgEntry>, AppError> {
    let m = state.sys.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut sources = Vec::new();
        for mgr in m.managers() {
            if mgr.available() {
                match mgr.list() {
                    Ok(pkgs) => sources.push((mgr.id(), pkgs)),
                    Err(e) => tracing::warn!(source = mgr.id(), error = %e, "包清单获取失败"),
                }
            }
        }
        Ok(sys_core::pkg::merge_installed(sources))
    })
    .await
    .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))?
    .map_err(sys_err)
}

/// 变更命令行预览（UI 确认展示——docs/impl/06 SY1：列出将执行的确切命令行）
#[tauri::command]
pub async fn sys_pkg_cmd_preview(
    source: String,
    action: String,
    package_id: String,
    state: State<'_, HostState>,
) -> Result<String, AppError> {
    let mgr = state
        .sys
        .manager(&source)
        .ok_or_else(|| AppError::module("SYS_IPC_002", format!("未知包管理器: {source}"), None))?;
    mgr.cmd_preview(&action, &package_id).map_err(sys_err)
}

/// 执行包变更（install/uninstall/upgrade_all；输出逐行发 sys.pkg_line 事件）
#[tauri::command]
pub async fn sys_pkg_action(
    source: String,
    action: String,
    package_id: String,
    state: State<'_, HostState>,
) -> Result<Vec<String>, AppError> {
    // 预检（在 move 之前校验管理器存在）
    state
        .sys
        .manager(&source)
        .ok_or_else(|| AppError::module("SYS_IPC_002", format!("未知包管理器: {source}"), None))?;
    let m = state.sys.clone();
    let bus = state.bus.clone();
    let src_for_emit = source.clone();
    let action_for_emit = action.clone();
    let lines = tauri::async_runtime::spawn_blocking(move || {
        let mgr = m
            .manager(&source)
            .ok_or_else(|| AppError::module("SYS_IPC_002", format!("未知包管理器: {source}"), None))?;
        let mut emit = |line: String| {
            bus.publish(host_core::events::Event::new(
                "sys.pkg_line",
                "sys",
                serde_json::json!({ "source": src_for_emit, "action": action_for_emit, "line": line }),
            ))
            .ok();
        };
        mgr.run_action(&action, &package_id, &mut emit)
            .map_err(|e| AppError::module(e.code(), e.to_string(), None))
    })
    .await
    .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))??;
    Ok(lines)
}

/// 清理目标清单（SY3）
#[tauri::command]
pub async fn sys_clean_targets(
    state: State<'_, HostState>,
) -> Result<Vec<sys_core::clean::CleanTarget>, AppError> {
    Ok(state.sys.targets().to_vec())
}

/// 清理扫描（SY3：汇总可回收量）
#[tauri::command]
pub async fn sys_clean_scan(
    state: State<'_, HostState>,
) -> Result<Vec<sys_core::clean::CleanScanItem>, AppError> {
    let m = state.sys.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let mut out = Vec::new();
        for t in m.targets() {
            match sys_core::clean::scan_target(t, now) {
                Ok(item) => out.push(item),
                Err(e) => tracing::warn!(target = t.id, error = %e, "清理扫描失败"),
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))?
}

/// 执行清理（selected_ids 勾选目标；recycle=true 走回收站可恢复）
#[tauri::command]
pub async fn sys_clean_execute(
    selected_ids: Vec<String>,
    recycle: bool,
    state: State<'_, HostState>,
) -> Result<u64, AppError> {
    let m = state.sys.clone();
    let recycle_port = state.sys.recycle();
    tauri::async_runtime::spawn_blocking(move || {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let mut total_files = 0u64;
        for id in &selected_ids {
            let Some(t) = m.target(id) else { continue };
            let recycle_fn = |paths: &[std::path::PathBuf]| -> sys_core::Result<u32> {
                let Some(rp) = recycle_port.as_ref() else {
                    return Err(sys_core::SysError::CleanTarget("回收站端口未注册".into()));
                };
                rp.delete(paths)
                    .map_err(|e| sys_core::SysError::CleanTarget(e.to_string()))
            };
            match sys_core::clean::execute_target(t, now, recycle, &recycle_fn) {
                Ok((files, _)) => total_files += files,
                Err(e) => tracing::warn!(target = id, error = %e, "清理执行失败"),
            }
        }
        Ok(total_files)
    })
    .await
    .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))?
}

/// 监控历史（SY4：环形缓冲快照）
#[tauri::command]
pub async fn sys_metrics_history(
    state: State<'_, HostState>,
) -> Result<Vec<sys_core::MetricsPoint>, AppError> {
    Ok(state.sys.metrics().history())
}

/// 进程页 Top-N（T-B7-10：两拍差值；sort=name|cpu|mem|disk，query 大小写不敏感）
#[tauri::command]
pub async fn sys_processes(
    sort: String,
    query: Option<String>,
    state: State<'_, HostState>,
) -> Result<Vec<sys_core::ProcessRow>, AppError> {
    let m = state.sys.clone();
    tauri::async_runtime::spawn_blocking(move || {
        Ok(m.processes(&sort, PROCESS_TOP_N, &query.unwrap_or_default()))
    })
    .await
    .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))?
}

/// 结束进程（T-B7-10 红线：复述名逐字确认 + 保护名单 + 坏盘总拒，闸在 sys-core；
/// 成功/被拒均落 `{appData}/sys/process_audit.jsonl`，返回实际进程名回执）
#[tauri::command]
pub async fn sys_kill(
    pid: u32,
    confirm_name: String,
    state: State<'_, HostState>,
) -> Result<String, AppError> {
    let m = state.sys.clone();
    tauri::async_runtime::spawn_blocking(move || m.kill_process(pid, &confirm_name))
        .await
        .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))?
        .map_err(sys_err)
}

/// 进程页行数上限（UI 滚动面；差值计算在 sys-core 两拍内完成）
const PROCESS_TOP_N: usize = 60;
