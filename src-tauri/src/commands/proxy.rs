use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

// ======================== 代理（M7 PR，docs/impl/05） ========================

fn proxy_service(state: &HostState) -> Result<std::sync::Arc<proxy_core::ProxyService>, AppError> {
    state.proxy.service().ok_or_else(|| {
        AppError::module(
            "PROXY_STATE_001",
            "代理模块尚未初始化",
            Some("请等待模块启动完成后重试"),
        )
    })
}

fn proxy_err(e: proxy_core::ProxyError) -> AppError {
    AppError::from(e)
}

/// 代理总状态（模式/内核运行/安装清单/管理员/TUN 前置/备份标记）
#[tauri::command]
pub async fn proxy_status(state: State<'_, HostState>) -> Result<proxy_core::StatusDto, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.status()))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
}

/// 安装/更新 sing-box 内核（官方 Release 直链；version 空则用默认版本）
#[tauri::command]
pub async fn proxy_kernel_install(
    version: Option<String>,
    state: State<'_, HostState>,
) -> Result<proxy_core::Manifest, AppError> {
    let svc = proxy_service(&state)?;
    svc.kernel_install(version).await.map_err(proxy_err)
}

/// 安装 wintun.dll（TUN 模式前置）
#[tauri::command]
pub async fn proxy_wintun_install(state: State<'_, HostState>) -> Result<(), AppError> {
    let svc = proxy_service(&state)?;
    svc.wintun_install().await.map_err(proxy_err)
}

/// 订阅列表
#[tauri::command]
pub async fn proxy_subs(state: State<'_, HostState>) -> Result<Vec<proxy_core::Sub>, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.subs()))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
}

/// 添加订阅（不自动拉取，随后调用 proxy_sub_update）
#[tauri::command]
pub async fn proxy_sub_add(
    name: String,
    url: String,
    state: State<'_, HostState>,
) -> Result<proxy_core::Sub, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.sub_add(&name, &url))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
        .map_err(proxy_err)
}

/// 删除订阅（连其节点一并清除）
#[tauri::command]
pub async fn proxy_sub_remove(id: String, state: State<'_, HostState>) -> Result<bool, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.sub_remove(&id))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
        .map_err(proxy_err)
}

/// 拉取并解析订阅（内容持久化，不进日志）
#[tauri::command]
pub async fn proxy_sub_update(
    id: String,
    state: State<'_, HostState>,
) -> Result<proxy_core::Sub, AppError> {
    let svc = proxy_service(&state)?;
    svc.sub_update(&id).await.map_err(proxy_err)
}

/// 节点列表（全部订阅聚合）
#[tauri::command]
pub async fn proxy_nodes(
    state: State<'_, HostState>,
) -> Result<Vec<proxy_core::NodeDto>, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.nodes()))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
}

/// 直连域名规则
#[tauri::command]
pub async fn proxy_direct_rules(state: State<'_, HostState>) -> Result<Vec<String>, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.direct_rules()))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
}

/// 设置直连域名规则（trim + 去空 + 去重；模式重新切换后生效）
#[tauri::command]
pub async fn proxy_set_direct_rules(
    rules: Vec<String>,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.set_direct_rules(rules))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
        .map_err(proxy_err)
}

/// 切换模式：off（停内核+还原）/ system（内核+系统代理）/ tun（内核+TUN，需管理员）
#[tauri::command]
pub async fn proxy_set_mode(mode: String, state: State<'_, HostState>) -> Result<(), AppError> {
    let svc = proxy_service(&state)?;
    let mode = proxy_core::Mode::parse(&mode).map_err(proxy_err)?;
    tauri::async_runtime::spawn_blocking(move || svc.set_mode(mode))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
        .map_err(proxy_err)
}

/// 节点 TCP 连通性测试（3s 超时，并发）
#[tauri::command]
pub async fn proxy_delay_test(
    state: State<'_, HostState>,
) -> Result<Vec<proxy_core::NodeDelayDto>, AppError> {
    let svc = proxy_service(&state)?;
    Ok(svc.delay_test().await)
}

/// 内核日志（环形缓冲快照，limit 条）
#[tauri::command]
pub async fn proxy_logs(
    limit: Option<usize>,
    state: State<'_, HostState>,
) -> Result<Vec<proxy_core::LogLine>, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.logs(limit.unwrap_or(100))))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
}
