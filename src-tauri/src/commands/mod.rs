//! 宿主级 IPC 命令 + 按模块域拆分（S6：原单文件 2809 行 / 172 命令）
//!
//! 命名规范：`{module}_{action}` snake_case；
//! 错误统一 `Result<T, AppError>`（S2 序列化契约）。
//! 每个领域一个子模块（与 docs/impl 各模块 IPC 节一一对应），
//! `pub use` 聚合后 lib.rs 的 `commands::xxx` 路径保持不变。

use host_core::error::AppError;
use tauri::State;

use crate::state::{HostState, ModuleStatusDto};

mod automation;
mod clipboard;
mod desktop;
mod editor;
mod file;
mod kvm;
mod notes;
mod ocr;
mod proxy;
mod screenshot;
mod sync;
mod sys;
mod term;
mod vault;
mod winops;

pub use automation::*;
pub use clipboard::*;
pub use desktop::*;
pub use editor::*;
pub use file::*;
pub use kvm::*;
pub use notes::*;
pub use ocr::*;
pub use proxy::*;
pub use screenshot::*;
pub use sync::*;
pub use sys::*;
pub use term::*;
pub use vault::*;
pub use winops::*;

/// 读取 Windows 系统强调色（docs/UI-PLAN.md U1-3）。
/// 失败时前端回退默认 Windows 蓝渐变。
#[tauri::command]
pub fn host_system_accent() -> Result<String, AppError> {
    win_integration::accent::system_accent_color().map(|c| c.to_hex())
}

/// 前端 OS 能力探测（COR-26）：Mica 仅 Win11 非远程会话可用。
/// 前端据此决定是否铺透明基底（否则渐变回退，防 chrome 透出壁纸）。
#[tauri::command]
pub fn host_capabilities() -> serde_json::Value {
    serde_json::json!({ "mica": win_integration::dwm::mica_supported() })
}

/// 全部模块元信息与状态（docs/impl/01 S7）
#[tauri::command]
pub fn host_modules_status(state: State<'_, HostState>) -> Vec<ModuleStatusDto> {
    build_modules_status(&state.registry)
}

/// infos × status_all 装配 DTO（纯函数，S6 IPC 契约回归入口）
pub fn build_modules_status(
    registry: &host_core::registry::ModuleRegistry,
) -> Vec<ModuleStatusDto> {
    let infos = registry.infos();
    let status: std::collections::HashMap<String, host_core::module::ModuleState> =
        registry.status_all().into_iter().collect();
    infos
        .into_iter()
        .map(|info| ModuleStatusDto {
            state: status
                .get(info.id)
                .copied()
                .unwrap_or(host_core::module::ModuleState::Uninitialized),
            id: info.id.to_owned(),
            name: info.name.to_owned(),
            version: info.version.to_owned(),
            priority: info.priority,
        })
        .collect()
}

/// 重启模块（stop → init → start；panic 后恢复入口）
#[tauri::command]
pub async fn host_module_restart(id: String, state: State<'_, HostState>) -> Result<(), AppError> {
    state.registry.restart(&id).await
}

/// 读模块配置
#[tauri::command]
pub fn host_config_get(
    module: String,
    state: State<'_, HostState>,
) -> Result<serde_json::Value, AppError> {
    state.config.get_module(&module)
}

/// 读模块配置 schema（设置中心自动渲染）
#[tauri::command]
pub fn host_config_schema(
    module: String,
    state: State<'_, HostState>,
) -> Result<serde_json::Value, AppError> {
    state
        .config
        .schema_of(&module)
        .ok_or_else(|| AppError::module("HOST_CONFIG_003", "模块未注册 schema", None))
}

/// 写模块配置（schema 校验 → 备份 → 原子写 → host.config_changed 事件）
#[tauri::command]
pub fn host_config_set(
    module: String,
    values: serde_json::Value,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state.config.set_module(&module, values)
}

/// 前端日志上报（webview console 外部不可见；关键异步失败经此进入宿主日志）。
/// COR-25：长度截断 + 换行清洗——`\n` 伪造日志行、超长消息写满日志文件都是
/// 从 webview 可达的放大面（allow-host-log 授予全部窗口）。
#[tauri::command]
pub fn host_log(level: String, message: String) {
    const MAX_LOG_LEN: usize = 2_000;
    let sanitized: String = message
        .chars()
        .take(MAX_LOG_LEN)
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    match level.as_str() {
        "error" => tracing::error!(target: "webview", "{sanitized}"),
        "warn" => tracing::warn!(target: "webview", "{sanitized}"),
        _ => tracing::info!(target: "webview", "{sanitized}"),
    }
}

/// 列表命令的 limit 统一夹取（COR-25）：默认 100、上限 500。
/// 与 sync 域既有的夹 200 对齐为同一纪律，防前端传巨值触发大查询。
pub fn clamp_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(100).clamp(1, 500)
}
