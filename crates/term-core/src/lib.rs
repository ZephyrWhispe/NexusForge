//! term-core：终端与运维（docs/impl/06 T1–T6）。
//!
//! - T1 ConPTY：win-integration 封装（CreatePseudoConsole + 双管道，写/resize 串行化）
//! - T2 数据管道：8ms 批处理（单批 ≤ 64KB）+ 前端 ack 背压（落后 > 4MB 暂停拉取）
//! - T3 SSH/SFTP：russh + TOFU known_hosts（指纹变更强拒绝）
//! - T5 WSL：wsl.exe 分发探测（UTF-16LE 解析）+ ConPTY spawn
//! - T6 Docker：Engine API over named pipe（容器列表/启停/日志 tail）

pub mod docker;
pub mod error;
pub mod module;
pub mod session;
pub mod ssh;
pub mod wsl;

pub use error::{Result, TermError};
pub use module::TermModule;
pub use session::{SessionInfo, SessionState, TermKind, TermSessions};
pub use ssh::{SftpEntry, SshAuth, SshService, SshTarget};
