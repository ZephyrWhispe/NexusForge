use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

// ---------------- KVM 键鼠共享命令（docs/impl/05 K8）----------------

/// ModuleError → AppError（IPC 层统一错误壳）
fn kvm_err(e: host_core::error::ModuleError) -> AppError {
    AppError::module("KVM_IPC_001", e.to_string(), None)
}

/// 签发一次性配对码（6 位，2 分钟有效）
#[tauri::command]
pub fn kvm_issue_pair_code(state: State<'_, HostState>) -> Result<(String, u64), AppError> {
    state.kvm.issue_pair_code().map_err(kvm_err)
}

/// 向已发现设备发起配对（阻塞握手 ≤10s → spawn_blocking）
#[tauri::command]
pub async fn kvm_pair_with(
    addr: String,
    code: String,
    state: State<'_, HostState>,
) -> Result<kvm_core::PairedPeer, AppError> {
    let addr: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| AppError::module("KVM_IPC_002", format!("地址非法: {e}"), None))?;
    let kvm = state.kvm.clone();
    tauri::async_runtime::spawn_blocking(move || kvm.pair_with(addr, &code))
        .await
        .map_err(|e| AppError::module("KVM_IPC_003", e.to_string(), None))?
        .map_err(kvm_err)
}

/// 解除配对
#[tauri::command]
pub fn kvm_unpair(device_id: String, state: State<'_, HostState>) -> Result<bool, AppError> {
    state.kvm.unpair(&device_id).map_err(kvm_err)
}

/// 已配对设备列表
#[tauri::command]
pub fn kvm_paired_peers(
    state: State<'_, HostState>,
) -> Result<Vec<kvm_core::PairedPeer>, AppError> {
    state.kvm.paired_peers().map_err(kvm_err)
}

/// 已发现邻居列表（心跳快照）
#[tauri::command]
pub fn kvm_discovered_peers(
    state: State<'_, HostState>,
) -> Result<Vec<kvm_core::PeerInfo>, AppError> {
    state.kvm.discovered_peers().map_err(kvm_err)
}

/// 向已配对设备发起会话（客户端角色；阻塞握手 ≤10s → spawn_blocking）
#[tauri::command]
pub async fn kvm_connect_to(addr: String, state: State<'_, HostState>) -> Result<String, AppError> {
    let addr: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| AppError::module("KVM_IPC_002", format!("地址非法: {e}"), None))?;
    let kvm = state.kvm.clone();
    tauri::async_runtime::spawn_blocking(move || kvm.connect_to(addr))
        .await
        .map_err(|e| AppError::module("KVM_IPC_003", e.to_string(), None))?
        .map_err(kvm_err)
}

/// 发送剪贴板内容到对端（Text/Image 单帧；Files 逐文件走 send_file）
#[tauri::command]
pub async fn kvm_send_clip(
    device_id: String,
    content: host_core::ports::ClipContent,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let kvm = state.kvm.clone();
    tauri::async_runtime::spawn_blocking(move || kvm.send_clip(&device_id, content))
        .await
        .map_err(|e| AppError::module("KVM_IPC_003", e.to_string(), None))?
        .map_err(kvm_err)
}

/// 发送本地文件到对端（后台传输；进度/回执走事件）
#[tauri::command]
pub fn kvm_send_file(
    device_id: String,
    path: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state.kvm.send_file(&device_id, path).map_err(kvm_err)
}

/// 活跃会话列表（服务端接入 + 本端发起）
#[tauri::command]
pub fn kvm_session_list(state: State<'_, HostState>) -> Vec<serde_json::Value> {
    state.kvm.session_list()
}

/// 设置 [设备→共享边] 映射（值 "left" | "right"；即时生效）
#[tauri::command]
pub fn kvm_set_edge_map(
    map: std::collections::HashMap<String, String>,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state.kvm.set_edge_map(map).map_err(kvm_err)
}

/// 当前边缘映射
#[tauri::command]
pub fn kvm_edge_map(state: State<'_, HostState>) -> std::collections::HashMap<String, String> {
    state.kvm.edge_map()
}

/// 控制状态（role: idle/controlling/controlled）
#[tauri::command]
pub fn kvm_control_state(state: State<'_, HostState>) -> serde_json::Value {
    state.kvm.control_state()
}

/// 手动释放控制权（UI 切回按钮）
#[tauri::command]
pub fn kvm_release_control(state: State<'_, HostState>) -> Result<(), AppError> {
    state.kvm.release_control().map_err(kvm_err)
}
