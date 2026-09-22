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
    /// 数据集白名单/注册表层面的拒绝（T-B5-5 承重⑧⑩⑪）：
    /// 未注册或白名单外的 entity、以及"快照读不到却被要求记删除"这类误删除前兆。
    /// 与 `Apply`（数据集写入本身失败）刻意分野：这两类是**结构**问题，重试无用。
    #[error("数据集拒绝: {0}")]
    Entity(String),
    /// 配置值被拒（T-B5-6）：坏值在**落运行态之前**弹回，经 `host.config_rejected`
    /// 回到设置界面。与 `Db`/`Apply` 分野：这不是执行失败，是"这个值本来就不该收"。
    #[error("配置拒绝: {0}")]
    Config(String),
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
            SyncError::Entity(_) => "SYNC_ENTITY_001",
            SyncError::Config(_) => "SYNC_CONFIG_001",
            SyncError::NotReady(_) => "SYNC_STATE_001",
        }
    }
}

pub type Result<T> = std::result::Result<T, SyncError>;
