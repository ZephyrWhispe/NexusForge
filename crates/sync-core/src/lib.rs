//! sync-core：跨设备同步（docs/impl/07 SYNC1–SYNC4）。
//!
//! - SYNC1 拓扑：局域网 P2P（独立 TCP 端口 49820；中继 v1 不做）
//! - SYNC2 变更流：op_log 追加表 + 设备游标（交换游标 → 拉缺失 → 应用）
//! - SYNC3 冲突：LWW(ts, device_id 字典序破平)；sync.conflict 事件通知
//! - SYNC4 加密：复用 K2 配对信任根（同 identity/paired.json）双 DH → HKDF → ChaCha20-Poly1305
//! - 数据集 v1 = note；密码库条目**永不**自动同步（白名单硬编码）

pub mod engine;
pub mod error;
pub mod module;
pub mod oplog;
pub mod transport;

pub use engine::{ApplyOutcome, ChangeApplier, SyncEngine};
pub use error::{Result, SyncError};
pub use module::{record_change_with, SyncCtx, SyncModule, SyncSummary, DEFAULT_SYNC_PORT};
pub use oplog::{OpEntry, OpLog};
pub use transport::{SyncMsg, SyncSession};
