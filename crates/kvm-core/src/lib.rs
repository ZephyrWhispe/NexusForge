//! kvm-core 键鼠共享（docs/impl/05 K1–K7）。
//!
//! 分层：discovery（K1 UDP 组播心跳）→ pairing（K2 一次性码 + X25519 指纹）
//! → session（K3 TCP 帧协议 + ChaCha20-Poly1305）→ transfer（K6 剪贴板/文件
//! 通道）→ 输入捕获/注入经 host-core 的 InputHookPort / InputInjectPort
//! （win-integration 实现）。模块本体见 module.rs（实现 host-core::Module）。

pub mod config;
pub mod discovery;
pub mod edge;
pub mod module;
pub mod pairing;
pub mod session;
pub mod transfer;

pub use config::{EdgeMapConfig, KvmConfig};
pub use discovery::{DiscoveryService, PeerEvent, PeerInfo};
pub use edge::{
    corner_edge, ControlReleasePayload, ControlTakePayload, Decision, Edge, EdgeSwitch,
};
pub use host_core::device::{DeviceIdentity, PairStore, PairedPeer};
pub use module::KvmModule;
pub use pairing::PairCodeManager;
pub use session::{SessionEvent, SessionHandle, SessionManager};
pub use transfer::{
    send_clip, send_file, AckPayload, ChunkOutcome, FileMetaPayload, FileProgress, MetaOutcome,
    TransferManager,
};
