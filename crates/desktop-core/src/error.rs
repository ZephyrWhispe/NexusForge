//! desktop-core 错误（DESIGN 统一 AppError 错误码体系）

use host_core::error::AppError;

#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("索引未构建")]
    NotIndexed,
    #[error("条目不存在: {0}")]
    NotFound(String),
    #[error("启动失败: {0}")]
    Launch(String),
    #[error("桌面整理失败: {0}")]
    Tidy(String),
    #[error("数据库错误: {0}")]
    Db(String),
    #[error("无效状态: {0}")]
    BadState(String),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, DesktopError>;

impl From<DesktopError> for AppError {
    fn from(e: DesktopError) -> Self {
        let (code, message, hint) = match &e {
            DesktopError::Io(err) => ("DESKTOP_IO_001", err.to_string(), None),
            DesktopError::NotIndexed => (
                "DESKTOP_STATE_001",
                "启动器索引尚未构建完成".into(),
                Some("等待索引构建完成后重试".into()),
            ),
            DesktopError::NotFound(id) => (
                "DESKTOP_QUERY_001",
                format!("条目 {id} 不存在"),
                None,
            ),
            DesktopError::Launch(m) => (
                "DESKTOP_LAUNCH_001",
                format!("启动失败: {m}"),
                Some("检查目标程序是否存在；管理员目标需在 UAC 弹窗确认".into()),
            ),
            DesktopError::Tidy(m) => ("DESKTOP_TIDY_001", m.clone(), None),
            DesktopError::Db(m) => ("DESKTOP_DB_001", m.clone(), None),
            DesktopError::BadState(m) => ("DESKTOP_STATE_002", m.clone(), None),
            DesktopError::Json(err) => ("DESKTOP_IO_002", err.to_string(), None),
        };
        AppError::module(code, message, hint)
    }
}
