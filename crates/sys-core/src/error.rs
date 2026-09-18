//! sys-core 错误类型（IPC 层映射 AppError，禁止裸 String）

#[derive(Debug, thiserror::Error)]
pub enum SysError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("参数不合法: {0}")]
    BadParam(String),
    #[error("包管理器不可用: {0}")]
    PkgUnavailable(String),
    #[error("包管理器命令失败: {0}")]
    PkgCmd(String),
    #[error("清理目标不存在: {0}")]
    CleanTarget(String),
    #[error("WinOps 目录错误: {0}")]
    Catalog(String),
    #[error("WinOps 注册表错误: {0}")]
    Registry(String),
    #[error("WinOps 应用失败: {0}")]
    Apply(String),
}

impl SysError {
    /// 映射 SYS_* 错误码（docs/impl/06 SY 各步骤 + docs/impl/08 WinOps）
    pub fn code(&self) -> &'static str {
        match self {
            SysError::Io(_) => "SYS_PKG_001",
            SysError::BadParam(_) => "SYS_PKG_002",
            SysError::PkgUnavailable(_) => "SYS_PKG_003",
            SysError::PkgCmd(_) => "SYS_PKG_004",
            SysError::CleanTarget(_) => "SYS_CLEAN_001",
            SysError::Catalog(_) => "SYS_WINOPS_001",
            SysError::Registry(_) => "SYS_WINOPS_002",
            SysError::Apply(_) => "SYS_WINOPS_003",
        }
    }
}

pub type Result<T> = std::result::Result<T, SysError>;
