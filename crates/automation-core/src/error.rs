//! automation-core 错误类型（IPC 层映射 AppError，禁止裸 String）

#[derive(Debug, thiserror::Error)]
pub enum AutomationError {
    #[error("规则不存在: {0}")]
    NoSuchRule(String),
    #[error("规则不合法: {0}")]
    BadRule(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("动作执行失败: {0}")]
    Action(String),
    #[error("已死信（重试耗尽）: {0}")]
    DeadLetter(String),
}

impl AutomationError {
    /// 映射 AUTO_* 错误码（docs/impl/07 A 各步骤）
    pub fn code(&self) -> &'static str {
        match self {
            AutomationError::NoSuchRule(_) => "AUTO_RULE_001",
            AutomationError::BadRule(_) => "AUTO_RULE_002",
            AutomationError::Io(_) => "AUTO_STORE_001",
            AutomationError::Action(_) => "AUTO_EXEC_001",
            AutomationError::DeadLetter(_) => "AUTO_EXEC_002",
        }
    }
}

pub type Result<T> = std::result::Result<T, AutomationError>;
