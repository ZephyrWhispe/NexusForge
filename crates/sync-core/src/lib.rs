//! sync-core：跨设备同步（docs/impl/07 SYNC1–SYNC4）。
//!
//! - SYNC1 拓扑：局域网 P2P（独立 TCP 端口 49820；中继 v1 不做）
//! - SYNC2 变更流：op_log 追加表 + 设备游标（交换游标 → 拉缺失 → 应用）
//! - SYNC3 冲突：LWW(ts, device_id 字典序破平)；败方快照落 conflict_log（可查 ·可以本地副本重新生效），
//!   sync.conflict 事件仅作提示
//! - SYNC4 加密：复用 K2 配对信任根（同 identity/paired.json）双 DH → HKDF → ChaCha20-Poly1305
//! - 会话流水：每轮同步（含失败）落一行 sync_run（T-B5-3：摘要不再只是 tracing 里的一行字）
//! - 数据集注册表（T-B5-5）：`SYNC_ENTITIES` 编译期白名单 + 按 entity 分派的应用器映射；
//!   密码库条目**永不**自动同步（白名单无 vault 条目 + attach 运行期拒 + 分派口永久拒收）

pub mod engine;
pub mod error;
pub mod module;
pub mod oplog;
pub mod transport;

pub use engine::{ApplyOutcome, ChangeApplier, SyncEngine};
pub use error::{Result, SyncError};
pub use module::{
    is_sync_entity, record_change_event, record_change_with, restore_conflict_with, EntitySpec,
    PeerAddrResolver, PeerStatus, SyncConfig, SyncCtx, SyncModule, SyncStatus, SyncSummary,
    CHANGE_ACTIONS, DEFAULT_CONFLICT_KEEP_DAYS, DEFAULT_QUIET_PERIOD_MS, DEFAULT_SYNC_PORT,
    ENTITY_NOTE, QUIET_PERIOD_MIN_MS, SYNC_ENTITIES,
};
pub use oplog::{ConflictEntry, OpEntry, OpLog, SyncRun};
pub use transport::{SyncMsg, SyncSession};
