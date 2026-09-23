//! file-core 文件与存储（docs/impl/05 F1–F7）。
//!
//! 分层：browse（F1）→ conflict（F3）→ ops（F2 队列/断点续传）→ preview（F4）
//! → search（F5 USN 优先/遍历降级）→ driver（F6 存储抽象）→ rename（F7 DSL）
//! → profile/remote（B6 档案与远端驱动，09 §6.2）→ service（门面）→ module（Module trait 壳）。

pub mod browse;
pub mod conflict;
pub mod driver;
pub mod error;
pub mod module;
pub mod ops;
pub mod preview;
pub mod profile;
pub mod remote;
pub mod rename;
pub mod search;
pub mod service;

pub use browse::{
    breadcrumbs, display_path, drives, list_dir, to_long_path, DriveInfo, FileEntry, SortKey,
};
pub use conflict::{
    conflict_pairs, scan_conflicts, unique_name, ConflictAction, ConflictItem, ConflictPolicy,
};
pub use driver::{DriverInfo, DriverRegistry, FileStoragePort, LocalDriver, StorageDriver};
pub use error::FileError;
pub use module::FileModule;
pub use ops::{
    direction_of, parent_key, Checkpoint, OpEndpoint, OpKind, OpProgress, OpQueue, OpSpec, OpState,
    PendingOp, TransferDirection, CHUNK,
};
pub use preview::Preview;
pub use profile::{
    profile_id_of, validate_profile, AuthKind, ProfileStore, RemoteProfile, RemoteProtocol,
};
pub use remote::{classify_resume, range_plan, remote_error_message, throttle_share_kbps};
pub use remote::{
    webdav::{join_remote_url, parse_propfind_responses, percent_decode, propfind_body},
    AuthSecret, DownloadOutcome, HttpsDriver, RemoteDriverInfo, RemoteEntry, Resumable,
    WebDavDriver,
};
pub use rename::{apply_plan, build_plan, CaseMode, RenamePlan, RenameRule};
pub use search::{SearchOpts, SearchResult};
pub use service::FileService;
