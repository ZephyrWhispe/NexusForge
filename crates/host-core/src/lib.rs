//! NexusForge 宿主核心（host-core）
//!
//! 按 docs/impl/01-host-core.md 实施：
//! - S2 错误体系 → S3 Module trait / Ports → S4 事件总线 → S5 注册表与生命周期
//! - S6 配置中心 / 快捷键 / 托盘 / 日志 / 崩溃恢复 → S7 Tauri Plugin 集成
//!
//! 当前状态：S1–S6 完成（S6：配置中心/快捷键/托盘聚合/日志/崩溃恢复）。

pub mod capability;
pub mod codes;
pub mod config;
pub mod crash;
pub mod error;
pub mod events;
pub mod hotkey;
pub mod logging;
pub mod module;
pub mod ports;
pub mod registry;

/// 宿主核心版本，与 workspace 版本保持一致
pub const HOST_CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_available() {
        assert!(!super::HOST_CORE_VERSION.is_empty());
    }
}
