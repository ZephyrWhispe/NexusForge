//! editor-core 错误（DESIGN 统一 AppError 错误码体系）

use host_core::error::AppError;

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("会话不存在: {0}")]
    NotFound(String),
    #[error("编码错误: {0}")]
    Encoding(String),
    #[error("PDF 处理失败: {0}")]
    Pdf(String),
    #[error("无效参数: {0}")]
    BadParam(String),
}

pub type Result<T> = std::result::Result<T, EditorError>;

impl From<EditorError> for AppError {
    fn from(e: EditorError) -> Self {
        let (code, message, hint) = match &e {
            EditorError::Io(err) => ("EDITOR_IO_001", err.to_string(), None),
            EditorError::NotFound(id) => (
                "EDITOR_SESSION_001",
                format!("会话 {id} 不存在（可能已关闭）"),
                None,
            ),
            EditorError::Encoding(m) => (
                "EDITOR_ENC_001",
                m.clone(),
                Some("可尝试以其他编码重新打开，或文件为二进制格式".into()),
            ),
            EditorError::Pdf(m) => (
                "EDITOR_PDF_001",
                m.clone(),
                Some("文件可能已损坏、加密或不是有效 PDF".into()),
            ),
            EditorError::BadParam(m) => ("EDITOR_PARAM_001", m.clone(), None),
        };
        AppError::module(code, message, hint)
    }
}
