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

/// 安装/更新内核（T-B2-3 参数化：kernel 缺省 sing-box=旧调用兼容，其余 id 触网前如实拒；version 空则用默认版本）
#[tauri::command]
pub async fn proxy_kernel_install(
    kernel: Option<String>,
    version: Option<String>,
    state: State<'_, HostState>,
) -> Result<proxy_core::Manifest, AppError> {
    let svc = proxy_service(&state)?;
    svc.kernel_install(kernel.as_deref(), version)
        .await
        .map_err(proxy_err)
}

/// 内核重启（T-B2-3）：仅运行中有效；停旧→以当前模式重生成配置→起新，mode/kernel 不变，
/// 起新失败后端按缺陷⑧纪律归零
#[tauri::command]
pub async fn proxy_kernel_restart(state: State<'_, HostState>) -> Result<(), AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.restart_kernel())
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
        .map_err(proxy_err)
}

/// 安装 wintun.dll（TUN 模式前置）
#[tauri::command]
pub async fn proxy_wintun_install(state: State<'_, HostState>) -> Result<(), AppError> {
    let svc = proxy_service(&state)?;
    svc.wintun_install().await.map_err(proxy_err)
}

/// 选定/切换代理内核（T-B2-2）：未运行 = 只落选择；运行中 = 新核起、失败自动回滚旧核
#[tauri::command]
pub async fn proxy_kernel_select(
    kernel: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.set_kernel(&kernel))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
        .map_err(proxy_err)
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

/// 直连域名规则（v2 投影适配器：读=suffix+direct 桶；写=全量替换该桶并双写旧文件）
#[tauri::command]
pub async fn proxy_direct_rules(state: State<'_, HostState>) -> Result<Vec<String>, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.direct_rules()))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
}

/// 设置直连域名规则（trim + 去空 + 保序去重；模式重新切换后生效）
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

/// 分流规则 v2 全表（T-B2-9：三目标规则 + 兜底 final + 全局模式）
#[tauri::command]
pub async fn proxy_rules_get(state: State<'_, HostState>) -> Result<proxy_core::RulesV2, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.rules_v2()))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
}

/// 全量写分流规则 v2（后端 sanitize 校验闸：CIDR/域字符集/进程名/枚举白名单，
/// 违规 Config 点名字段值；改后需重新切换模式生效——与旧直连规则同语义）
#[tauri::command]
pub async fn proxy_rules_set(
    rules: proxy_core::RulesV2,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.set_rules_v2(rules))
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
