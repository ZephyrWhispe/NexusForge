//! 统一错误体系（docs/impl/01 S2，依据 docs/DESIGN.md §8.1）
//!
//! 两层结构：
//! - [`ModuleError`]：模块内部错误，供 `Module` trait 的 `Result` 使用；
//! - [`AppError`]：IPC / 宿主级错误，序列化为 `{kind, data:{code, message, hint?}}`
//!   交给前端，**禁止**在 IPC 层出现裸 `String` 错误。

use serde::Serialize;

/// 模块级错误（Module trait 各生命周期方法返回）
#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("模块初始化失败: {0}")]
    Init(String),
    #[error("模块启动失败: {0}")]
    Start(String),
    #[error("模块停止失败: {0}")]
    Stop(String),
    #[error("配置错误: {0}")]
    Config(String),
    #[error("存储错误: {0}")]
    Storage(String),
    #[error("能力不支持: {0}")]
    Unsupported(String),
    #[error("模块未就绪")]
    NotReady,
    #[error("模块已 panic: {0}")]
    Panicked(String),
}

impl ModuleError {
    /// 映射到宿主错误码（`HOST_MODULE_{CATEGORY}`）
    pub fn code(&self) -> &'static str {
        match self {
            ModuleError::Init(_) => "HOST_MODULE_INIT",
            ModuleError::Start(_) => "HOST_MODULE_START",
            ModuleError::Stop(_) => "HOST_MODULE_STOP",
            ModuleError::Config(_) => "HOST_MODULE_CONFIG",
            ModuleError::Storage(_) => "HOST_MODULE_STORAGE",
            ModuleError::Unsupported(_) => "HOST_MODULE_UNSUPPORTED",
            ModuleError::NotReady => "HOST_MODULE_NOTREADY",
            ModuleError::Panicked(_) => "HOST_MODULE_PANIC",
        }
    }
}

/// IPC / 宿主级统一错误。
///
/// Tauri 命令签名统一为 `Result<T, AppError>`；
/// 序列化形状：`{"kind":"Module","data":{"code":"...","message":"...","hint":"..."}}`。
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "data")]
pub enum AppError {
    /// 模块错误（来自 ModuleError 或模块经宿主上抛）
    #[error("{message}")]
    Module {
        code: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
    },
    /// 存储错误（SQLite / blob / 文件 IO）
    #[error("{message}")]
    Storage { code: String, message: String },
    /// 网络错误（Sidecar 通信 / 同步 / 订阅拉取）
    #[error("{message}")]
    Network {
        code: String,
        message: String,
        /// 前端据此决定是否展示"重试"按钮
        retryable: bool,
    },
    /// 权限不足（管理员操作 / 受保护资源），hint 必填
    #[error("{message}")]
    Permission {
        code: String,
        message: String,
        hint: String,
    },
    /// 配置错误（schema 校验 / 迁移）
    #[error("{message}")]
    Config { code: String, message: String },
}

impl AppError {
    /// 模块侧快捷构造。
    ///
    /// # 示例
    /// ```
    /// use host_core::error::AppError;
    /// let e = AppError::module("CLIPBOARD_STORAGE_001", "写入历史失败", Some("检查磁盘空间"));
    /// assert_eq!(e.code(), "CLIPBOARD_STORAGE_001");
    /// ```
    pub fn module(code: &str, message: impl Into<String>, hint: Option<&str>) -> Self {
        AppError::Module {
            code: code.to_owned(),
            message: message.into(),
            hint: hint.map(str::to_owned),
        }
    }

    /// 取错误码（供日志与前端埋点）。
    pub fn code(&self) -> &str {
        match self {
            AppError::Module { code, .. }
            | AppError::Storage { code, .. }
            | AppError::Network { code, .. }
            | AppError::Permission { code, .. }
            | AppError::Config { code, .. } => code,
        }
    }
}

impl From<ModuleError> for AppError {
    fn from(e: ModuleError) -> Self {
        AppError::Module {
            code: e.code().to_owned(),
            message: e.to_string(),
            hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codes;
    use serde_json::Value;

    #[test]
    fn module_error_maps_to_app_error_with_code() {
        let e: AppError = ModuleError::Storage("clipboard.db 锁定".into()).into();
        assert_eq!(e.code(), "HOST_MODULE_STORAGE");
        match e {
            AppError::Module { message, hint, .. } => {
                assert!(message.contains("存储错误"));
                assert!(hint.is_none());
            }
            other => panic!("期望 Module 变体，实际: {other:?}"),
        }
    }

    #[test]
    fn serialization_shape_matches_ipc_contract() {
        // 契约（docs/UI-PLAN.md §8，M1 前冻结）：{kind, data:{code,message,hint?}}
        let e = AppError::module(
            "CLIPBOARD_STORAGE_001",
            "写入历史失败",
            Some("检查磁盘空间"),
        );
        let v: Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "Module");
        assert_eq!(v["data"]["code"], "CLIPBOARD_STORAGE_001");
        assert_eq!(v["data"]["message"], "写入历史失败");
        assert_eq!(v["data"]["hint"], "检查磁盘空间");

        // hint 为 None 时字段必须省略（省带宽 + 前端判空简化）
        let e2 = AppError::module("OCR_ENGINE_001", "无可用 OCR 引擎", None);
        let v2: Value = serde_json::to_value(&e2).unwrap();
        assert!(v2["data"].get("hint").is_none());
    }

    #[test]
    fn error_codes_follow_naming_convention() {
        // 全部宿主码必须匹配 {HOST}_{CATEGORY}_{NNN}
        for code in [
            codes::host::HOST_REGISTRY_001,
            codes::host::HOST_REGISTRY_002,
            codes::host::HOST_HOTKEY_001,
            codes::host::HOST_HOTKEY_002,
            codes::host::HOST_CONFIG_001,
            codes::host::HOST_CONFIG_002,
            codes::host::HOST_EVENT_001,
            codes::host::HOST_EVENT_002,
        ] {
            assert!(
                code.starts_with("HOST_") && code.len() >= "HOST_X_000".len(),
                "错误码不合规: {code}"
            );
        }
        // ModuleError 派生码以 HOST_MODULE_ 为前缀
        assert!(ModuleError::NotReady.code().starts_with(codes::HOST_MODULE_PREFIX));
    }

    #[test]
    fn network_error_carries_retryable_flag() {
        let e = AppError::Network {
            code: "SYNC_NET_001".into(),
            message: "订阅拉取超时".into(),
            retryable: true,
        };
        let v: Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["data"]["retryable"], true);
        assert_eq!(e.code(), "SYNC_NET_001");
    }
}
