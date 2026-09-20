use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

// ======================== 桌面效率（M8 D，docs/impl/05） ========================

fn desktop_module(state: &HostState) -> std::sync::Arc<desktop_core::DesktopModule> {
    state.desktop.clone()
}

fn desktop_err(e: desktop_core::DesktopError) -> AppError {
    AppError::from(e)
}

/// 启动器搜索（D1+D2 打分排序）
#[tauri::command]
pub async fn desktop_launcher_search(
    query: String,
    state: State<'_, HostState>,
) -> Result<Vec<desktop_core::LauncherHit>, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || m.index().search(&query, 20))
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
        .map_err(desktop_err)
}

/// 启动条目（App → ShellExecuteW；Action → 发事件；记频次）
#[tauri::command]
pub async fn desktop_launcher_launch(
    id: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || m.launch(&id))
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
        .map_err(AppError::from)
}

/// 索引状态（是否就绪 + 条目数）
#[tauri::command]
pub async fn desktop_launcher_status(
    state: State<'_, HostState>,
) -> Result<(bool, usize), AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || Ok(m.index().status()))
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
}

/// 重建启动器索引（09 §4.2 T-B1-6：core 内 build 后重放内置动作）。返回 App 条目数。
#[tauri::command]
pub async fn desktop_launcher_reindex(state: State<'_, HostState>) -> Result<usize, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || Ok(m.reindex()))
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
}

/// 桌面整理预览（D3）
#[tauri::command]
pub async fn desktop_tidy_plan(
    state: State<'_, HostState>,
) -> Result<desktop_core::TidyPlan, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || m.tidy_planner().plan(m.desktop_dir()))
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
        .map_err(desktop_err)
}

/// 执行桌面整理（返回移动数/跳过数）
#[tauri::command]
pub async fn desktop_tidy_apply(state: State<'_, HostState>) -> Result<(usize, usize), AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || m.tidy_planner().apply(m.desktop_dir()))
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
        .map_err(desktop_err)
}

/// 还原上次整理
#[tauri::command]
pub async fn desktop_tidy_restore(state: State<'_, HostState>) -> Result<usize, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || m.tidy_planner().restore())
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
        .map_err(desktop_err)
}

/// 是否存在待还原的整理记录
#[tauri::command]
pub async fn desktop_tidy_status(state: State<'_, HostState>) -> Result<bool, AppError> {
    let m = desktop_module(&state);
    Ok(m.tidy_planner().has_manifest())
}

/// 新增随记（D4：#标签 + 提醒解析在 core 内）
#[tauri::command]
pub async fn desktop_note_add(
    content: String,
    state: State<'_, HostState>,
) -> Result<desktop_core::Note, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let store = m
            .note_store()
            .ok_or_else(|| desktop_core::DesktopError::BadState("随记库未初始化".into()))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        store.add(&content, now)
    })
    .await
    .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
    .map_err(desktop_err)
}

/// 随记列表
#[tauri::command]
pub async fn desktop_note_list(
    include_done: bool,
    state: State<'_, HostState>,
) -> Result<Vec<desktop_core::Note>, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let store = m
            .note_store()
            .ok_or_else(|| desktop_core::DesktopError::BadState("随记库未初始化".into()))?;
        store.list(include_done)
    })
    .await
    .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
    .map_err(desktop_err)
}

/// 随记完成/未完成
#[tauri::command]
pub async fn desktop_note_done(
    id: String,
    done: bool,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let store = m
            .note_store()
            .ok_or_else(|| desktop_core::DesktopError::BadState("随记库未初始化".into()))?;
        store.set_done(&id, done)
    })
    .await
    .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
    .map_err(desktop_err)
}

/// 删除随记
#[tauri::command]
pub async fn desktop_note_remove(
    id: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let store = m
            .note_store()
            .ok_or_else(|| desktop_core::DesktopError::BadState("随记库未初始化".into()))?;
        store.remove(&id)
    })
    .await
    .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
    .map_err(desktop_err)
}

/// 手动拉取到期提醒（后台轮询之外的补充路径）
#[tauri::command]
pub async fn desktop_notes_due(
    state: State<'_, HostState>,
) -> Result<Vec<desktop_core::Note>, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let store = m
            .note_store()
            .ok_or_else(|| desktop_core::DesktopError::BadState("随记库未初始化".into()))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        store.take_due(now)
    })
    .await
    .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
    .map_err(desktop_err)
}
