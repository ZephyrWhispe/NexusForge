//! file-core 文件与存储（docs/impl/05 F1–F7）。
//!
//! 分层：browse（F1）→ conflict（F3）→ ops（F2 队列/断点续传）→ preview（F4）
//! → search（F5 USN 优先/遍历降级）→ driver（F6 存储抽象）→ rename（F7 DSL）
//! → service（门面）→ module（Module trait 壳）。

pub mod browse;
pub mod conflict;
pub mod driver;
pub mod error;
pub mod module;
pub mod ops;
pub mod preview;
pub mod rename;
pub mod search;
pub mod service;

pub use browse::{breadcrumbs, drives, list_dir, to_long_path, display_path, DriveInfo, FileEntry, SortKey};
pub use conflict::{scan_conflicts, ConflictAction, ConflictItem, ConflictPolicy};
pub use driver::{DriverInfo, DriverRegistry, LocalDriver, StorageDriver};
pub use error::FileError;
pub use module::FileModule;
pub use ops::{OpKind, OpProgress, OpQueue, OpSpec, OpState, PendingOp, Checkpoint, CHUNK, PROGRESS_INTERVAL};
pub use preview::Preview;
pub use rename::{apply_plan, build_plan, CaseMode, RenamePlan, RenameRule};
pub use search::{SearchOpts, SearchResult};
pub use service::FileService;
