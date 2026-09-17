//! file-core 错误类型（docs/impl/01 S2 契约：IPC 层映射 AppError，禁止裸 String）

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("路径不存在: {0}")]
    NotFound(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("路径不合法: {0}")]
    BadPath(String),
    #[error("操作不存在: {0}")]
    NoSuchOp(String),
    #[error("操作状态不允许该动作: {0}")]
    BadState(String),
    #[error("压缩/解压失败: {0}")]
    Zip(String),
    #[error("预览失败: {0}")]
    Preview(String),
    #[error("重命名规则错误: {0}")]
    Rule(String),
    #[error("USN 索引不可用: {0}")]
    Usn(String),
}

impl FileError {
    /// 映射 FILE_* 错误码（docs/impl/05 F 各步骤）
    pub fn code(&self) -> &'static str {
        match self {
            FileError::NotFound(_) => "FILE_BROWSE_001",
            FileError::Io(_) => "FILE_OPS_001",
            FileError::BadPath(_) => "FILE_BROWSE_002",
            FileError::NoSuchOp(_) => "FILE_OPS_002",
            FileError::BadState(_) => "FILE_OPS_003",
            FileError::Zip(_) => "FILE_OPS_004",
            FileError::Preview(_) => "FILE_PREVIEW_001",
            FileError::Rule(_) => "FILE_RENAME_001",
            FileError::Usn(_) => "FILE_SEARCH_001",
        }
    }
}
