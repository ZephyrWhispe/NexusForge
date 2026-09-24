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
    #[error("主机密钥不在记录（TOFU 首见拒连）: {note}{host}:{port}。指纹（逐字）：{descriptor}。请经带外渠道核对后走指纹确认命令")]
    HostKeyUnknown {
        /// ProxyJump 臂的展示侧前缀（"第 N 跳 "，T-B7-4 落位）；直连为空串。
        /// host/port/descriptor 三字段恒原样——跳数信息永不混进逐字回传面
        /// （指纹确认命令吃的还是那三枚原字段）
        note: String,
        host: String,
        port: u16,
        descriptor: String,
    },
    #[error("认证失败: {0}")]
    Auth(String),
    #[error("转发错误: {0}")]
    Forward(String),
    #[error("端口暴露需显式指定: {0}")]
    ForwardBind(String),
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

impl From<host_core::ssh_trust::TrustError> for TermError {
    fn from(e: host_core::ssh_trust::TrustError) -> Self {
        // 信任文件 IO/损坏消息已自带点名（路径 + fail-closed 语义），
        // 裹进 TERM_SSH_001 臂——不为存储面另立错误码家族
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
            TermError::HostKeyUnknown { .. } => "TERM_SSH_004",
            TermError::Forward(_) => "TERM_FWD_001",
            TermError::ForwardBind(_) => "TERM_FWD_002",
            TermError::Wsl(_) => "TERM_WSL_001",
            TermError::Docker(_) => "TERM_DOCKER_001",
        }
    }
}

pub type Result<T> = std::result::Result<T, TermError>;
