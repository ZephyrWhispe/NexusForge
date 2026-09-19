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
