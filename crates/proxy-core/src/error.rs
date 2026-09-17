//! 错误体系（docs/impl/05 PR）：ProxyError → PROXY_* 错误码（与 AppError::module 对接）

use host_core::error::AppError;

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("资源不存在: {0}")]
    NotFound(String),
    #[error("路径非法: {0}")]
    BadPath(String),
    #[error("状态不允许: {0}")]
    BadState(String),
    #[error("内核错误: {0}")]
    Kernel(String),
    #[error("配置生成失败: {0}")]
    Config(String),
    #[error("订阅失败: {0}")]
    Subscription(String),
    #[error("下载/校验失败: {0}")]
    Download(String),
    #[error("系统代理错误: {0}")]
    SysProxy(String),
    #[error("权限不足: {0}")]
    Permission(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("序列化错误: {0}")]
    Json(#[from] serde_json::Error),
}

impl ProxyError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "PROXY_QUERY_001",
            Self::BadPath(_) => "PROXY_QUERY_002",
            Self::BadState(_) => "PROXY_STATE_001",
            Self::Kernel(_) => "PROXY_KERNEL_001",
            Self::Config(_) => "PROXY_CONFIG_001",
            Self::Subscription(_) => "PROXY_SUB_001",
            Self::Download(_) => "PROXY_DOWNLOAD_001",
            Self::SysProxy(_) => "PROXY_SYS_001",
            Self::Permission(_) => "PROXY_TUN_002",
            Self::Io(_) => "PROXY_IO_001",
            Self::Json(_) => "PROXY_IO_002",
        }
    }
}

impl From<ProxyError> for AppError {
    fn from(e: ProxyError) -> Self {
        let hint = match &e {
            ProxyError::Permission(_) => Some("TUN 模式需要以管理员身份运行 NexusForge"),
            ProxyError::Kernel(_) => Some("可查看日志页定位内核报错，或重新安装内核"),
            _ => None,
        };
        AppError::module(e.code(), e.to_string(), hint)
    }
}

pub type Result<T> = std::result::Result<T, ProxyError>;
