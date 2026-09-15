//! 宿主核心错误码（docs/impl/01 S2）
//!
//! 规范：`{MODULE}_{CATEGORY}_{NNN}`，模块标识大写，全仓库唯一。
//! CI 通过脚本扫描 codes.rs 文件防止重复码。

/// 模块错误统一前缀（由 `ModuleError` 自动映射，见 [`crate::error::AppError`]）
pub const HOST_MODULE_PREFIX: &str = "HOST_MODULE";

/// 宿主自身错误码
pub mod host {
    /// 模块未注册（按 id 查询失败）
    pub const HOST_REGISTRY_001: &str = "HOST_REGISTRY_001";
    /// 模块重复注册
    pub const HOST_REGISTRY_002: &str = "HOST_REGISTRY_002";
    /// 快捷键冲突（模块间优先级仲裁失败）
    pub const HOST_HOTKEY_001: &str = "HOST_HOTKEY_001";
    /// 快捷键被系统占用（RegisterHotKey 失败）
    pub const HOST_HOTKEY_002: &str = "HOST_HOTKEY_002";
    /// 配置 schema 版本不兼容且迁移失败
    pub const HOST_CONFIG_001: &str = "HOST_CONFIG_001";
    /// 配置写入失败（schema 校验未通过 / 落盘 IO）
    pub const HOST_CONFIG_002: &str = "HOST_CONFIG_002";
    /// 未注册的事件主题（publish/subscribe 被拒绝）
    pub const HOST_EVENT_001: &str = "HOST_EVENT_001";
    /// 事件通道满（背压丢弃，宿主内部记录，通常不上抛 UI）
    pub const HOST_EVENT_002: &str = "HOST_EVENT_002";
}
