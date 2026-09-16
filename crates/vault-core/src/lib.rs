//! vault-core 密码库（docs/impl/05 V1–V6）。
//!
//! 分层：crypto（V1 信封 + 字段加密）→ model/store（V2 SQLite 持久化）
//! → vault（V3 解锁/锁定状态机）→ generator（V6 生成器）/ totp（RFC 6238）。
//! 内存纪律：密钥一律 [`crypto::SecretKey`]（zeroize Drop，无 Debug/Display）。

pub mod crypto;
pub mod generator;
pub mod model;
pub mod module;
pub mod totp;
pub mod vault;

pub use crypto::{
    change_master_password, create_vault, create_vault_with, open_field, seal_field, unwrap_dek,
    KdfParams, SecretKey, VaultHeader, WrappedKey, KEY_LEN,
};
pub use generator::{generate_password, PasswordPolicy};
pub use model::{Entry, EntryField, Folder, VaultStore};
pub use module::VaultModule;
pub use totp::totp_now;
pub use vault::{VaultService, VaultState, MAX_ATTEMPTS, LOCKOUT};
