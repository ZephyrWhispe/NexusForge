//! term-core 错误类型（IPC 层映射 AppError，禁止裸 String）

#[derive(Debug, thiserror::Error)]
pub enum TermError {
    #[error("会话不存在: {0}")]
    NoSuchSession(String),
    #[error("会话已结束: {0}")]
    DeadSession(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("参数不合法: {0}")]
    BadParam(String),
    #[error("状态不允许该操作: {0}")]
    BadState(String),
    #[error("ConPTY 错误: {0}")]
    Pty(String),
    #[error("SSH 错误: {0}")]
    Ssh(String),
    #[error("SFTP 错误: {0}")]
    Sftp(String),
    #[error("主机密钥校验失败（TOFU）: {0}")]
    HostKey(String),
    #[error("认证失败: {0}")]
    Auth(String),
    #[error("WSL 错误: {0}")]
    Wsl(String),
    #[error("Docker 错误: {0}")]
    Docker(String),
}

impl From<russh::Error> for TermError {
    fn from(e: russh::Error) -> Self {
        TermError::Ssh(e.to_string())
    }
}

impl TermError {
    /// 映射 TERM_* 错误码（docs/impl/06 T 各步骤）
    pub fn code(&self) -> &'static str {
        match self {
            TermError::NoSuchSession(_) => "TERM_SESSION_001",
            TermError::DeadSession(_) => "TERM_SESSION_002",
            TermError::Io(_) => "TERM_SESSION_003",
            TermError::BadParam(_) => "TERM_SESSION_004",
            TermError::BadState(_) => "TERM_SESSION_005",
            TermError::Pty(_) => "TERM_PTY_001",
            TermError::Ssh(_) => "TERM_SSH_001",
            TermError::Sftp(_) => "TERM_SFTP_001",
            TermError::HostKey(_) => "TERM_SSH_002",
            TermError::Auth(_) => "TERM_SSH_003",
            TermError::Wsl(_) => "TERM_WSL_001",
            TermError::Docker(_) => "TERM_DOCKER_001",
        }
    }
}

pub type Result<T> = std::result::Result<T, TermError>;
