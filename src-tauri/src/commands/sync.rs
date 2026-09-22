use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

// ======================== 跨设备同步（M15 SYNC，docs/impl/07） ========================

fn sync_err(e: sync_core::SyncError) -> AppError {
    AppError::module(e.code(), e.to_string(), None)
}

/// 配对设备列表（信任根复用 KVM 配对；UI 选择同步目标）
#[tauri::command]
pub async fn sync_peers(
    state: State<'_, HostState>,
) -> Result<Vec<kvm_core::PairedPeer>, AppError> {
    Ok(state.sync.peers())
}

/// op_log 状态（计数/监听端口）
#[tauri::command]
pub async fn sync_status(state: State<'_, HostState>) -> Result<serde_json::Value, AppError> {
    Ok(state.sync.status())
}

/// 立即与指定设备同步（addr 如 "192.168.1.10:49820"；端口默认 DEFAULT_SYNC_PORT）
#[tauri::command]
pub async fn sync_now(
    device_id: String,
    addr: String,
    state: State<'_, HostState>,
) -> Result<sync_core::SyncSummary, AppError> {
    state
        .sync
        .sync_with(&device_id, &addr)
        .await
        .map_err(sync_err)
}

/// 冲突行 DTO（09 §10.2 T-B5-2）：直接透出 sync-core 结构 = 单一真源，
/// 不另立字段副本（承重⑭ 的教训：两处声明同形靠人记，一处声明靠编译器记）。
pub type SyncConflictDto = sync_core::ConflictEntry;

/// 恢复结果最小形状：只说"这条快照已作为本机新变更入流"，**不含**任何"对端已回滚"承诺
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRestoreDto {
    pub conflict_id: String,
    pub entity: String,
    pub entity_id: String,
    /// 新入流的 op（可据此在变更流里对上号）
    pub op_id: String,
    /// 新 op 的时间戳（大于当时胜者 ts ⇒ 下轮 LWW 由它胜出）
    pub ts: i64,
}

/// 冲突历史分页（读侧 LIMIT 化：表可增长，视图不跟着无界）
#[tauri::command]
pub async fn sync_conflicts_get(
    limit: u32,
    offset: u32,
    state: State<'_, HostState>,
) -> Result<Vec<SyncConflictDto>, AppError> {
    let sync = state.sync.clone();
    let limit = (limit.min(200)) as i64;
    tauri::async_runtime::spawn_blocking(move || sync.conflicts(limit, offset as i64))
        .await
        .map_err(|e| sync_err(sync_core::SyncError::Db(format!("冲突读任务失败：{e}"))))?
        .map_err(sync_err)
}

/// 以本地留存的败方快照重新生效并推送（文案红线：不称"撤销对端/强制回滚"）
#[tauri::command]
pub async fn sync_conflict_restore(
    conflict_id: String,
    state: State<'_, HostState>,
) -> Result<SyncRestoreDto, AppError> {
    let sync = state.sync.clone();
    let id = conflict_id.clone();
    let op = tauri::async_runtime::spawn_blocking(move || sync.restore_conflict(&id))
        .await
        .map_err(|e| sync_err(sync_core::SyncError::Db(format!("冲突恢复任务失败：{e}"))))?
        .map_err(sync_err)?;
    Ok(SyncRestoreDto {
        conflict_id,
        entity: op.entity,
        entity_id: op.entity_id,
        op_id: op.op_id,
        ts: op.ts,
    })
}

/// 流水行 DTO（09 §10.2 T-B5-3）：同 `SyncConflictDto` 的取舍——直接透出 sync-core 结构。
pub type SyncRunDto = sync_core::SyncRun;

/// 同步活动流水（新行在前）。失败行照实返回（`error` 非空），面板不得只渲染成功行。
#[tauri::command]
pub async fn sync_runs_get(
    limit: u32,
    state: State<'_, HostState>,
) -> Result<Vec<SyncRunDto>, AppError> {
    let sync = state.sync.clone();
    let limit = (limit.min(200)) as i64;
    tauri::async_runtime::spawn_blocking(move || sync.runs(limit))
        .await
        .map_err(|e| sync_err(sync_core::SyncError::Db(format!("同步流水读任务失败：{e}"))))?
        .map_err(sync_err)
}
