//! notes-core 错误类型（IPC 层映射 AppError，禁止裸 String）

#[derive(Debug, thiserror::Error)]
pub enum NoteError {
    #[error("笔记不存在: {0}")]
    NotFound(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("路径不合法: {0}")]
    BadPath(String),
    #[error("状态不允许该操作: {0}")]
    BadState(String),
    #[error("数据库错误: {0}")]
    Db(String),
    #[error("画布数据错误: {0}")]
    Canvas(String),
    #[error("复习参数错误: {0}")]
    Review(String),
    #[error("驱动错误: {0}")]
    Driver(String),
}

impl NoteError {
    /// 映射 NOTE_* 错误码（docs/impl/06 N 各步骤）
    pub fn code(&self) -> &'static str {
        match self {
            NoteError::NotFound(_) => "NOTE_INDEX_001",
            NoteError::Io(_) => "NOTE_STORE_001",
            NoteError::BadPath(_) => "NOTE_STORE_002",
            NoteError::BadState(_) => "NOTE_STORE_003",
            NoteError::Db(_) => "NOTE_INDEX_002",
            NoteError::Canvas(_) => "NOTE_CANVAS_001",
            NoteError::Review(_) => "NOTE_REVIEW_001",
            NoteError::Driver(_) => "NOTE_STORE_004",
        }
    }
}

pub type Result<T> = std::result::Result<T, NoteError>;
