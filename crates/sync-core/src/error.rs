//! sync-core 错误类型（IPC 层映射 AppError，禁止裸 String）

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("本地 op_log 错误: {0}")]
    Db(String),
    #[error("同步网络错误: {0}")]
    Net(String),
    #[error("同步协议错误: {0}")]
    Proto(String),
    #[error("对端校验失败: {0}")]
    Peer(String),
    #[error("变更应用失败: {0}")]
    Apply(String),
    #[error("未就绪: {0}")]
    NotReady(String),
}

impl SyncError {
    /// SYNC_* 错误码（docs/impl/07 SYNC）
    pub fn code(&self) -> &'static str {
        match self {
            SyncError::Db(_) => "SYNC_DB_001",
            SyncError::Net(_) => "SYNC_NET_001",
            SyncError::Proto(_) => "SYNC_PROTO_001",
            SyncError::Peer(_) => "SYNC_PEER_001",
            SyncError::Apply(_) => "SYNC_APPLY_001",
            SyncError::NotReady(_) => "SYNC_STATE_001",
        }
    }
}

pub type Result<T> = std::result::Result<T, SyncError>;
