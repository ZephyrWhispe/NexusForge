//! file-core 错误类型（docs/impl/01 S2 契约：IPC 层映射 AppError，禁止裸 String）

/// B6 固定码表（09 §6.2 T-B6-1）：远端面错误码恒出自这五枚，消息只由纯函数产出。
/// T-B6-6/8 追加的 006/007/008 属运行期门禁码，随各自批次行入表。
pub const FILE_REMOTE_MISSING: &str = "FILE_REMOTE_001";
pub const FILE_REMOTE_ID: &str = "FILE_REMOTE_002";
pub const FILE_REMOTE_MIXED: &str = "FILE_REMOTE_003";
pub const FILE_REMOTE_NOTIMPL: &str = "FILE_REMOTE_004";
pub const FILE_REMOTE_FIELD: &str = "FILE_REMOTE_005";

/// 本批远端面固定码表（判据：新码须先入表再使用，禁散落字面量）
pub const FILE_REMOTE_CODES: &[&str] = &[
    FILE_REMOTE_MISSING,
    FILE_REMOTE_ID,
    FILE_REMOTE_MIXED,
    FILE_REMOTE_NOTIMPL,
    FILE_REMOTE_FIELD,
];

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
    #[error("该驱动不支持此操作: {0}")]
    Unsupported(String),
    #[error("{msg}")]
    Remote { code: &'static str, msg: String },
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
            FileError::Unsupported(_) => "FILE_OPS_005",
            FileError::Remote { code, .. } => code,
        }
    }
}

/// D-02：跨模块存储契约统一以 AppError 传播（错误码沿用 FILE_* 系）
impl From<FileError> for host_core::error::AppError {
    fn from(e: FileError) -> Self {
        host_core::error::AppError::module(e.code(), e.to_string(), None)
    }
}
