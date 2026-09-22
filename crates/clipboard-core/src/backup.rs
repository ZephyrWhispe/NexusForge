//! 加密备份导出/导入（09 §8.2 T-B3-9 / 细案 01§8-5）
//!
//! 红线是**明文导出禁**：落盘文件只含 AES-256-GCM 密文，敏感条目的明文既不以原文、
//! 也不以其 base64 变体出现在文件字节里。口令门只在「包含敏感条目」时立起——
//! 拒（`CLIPBOARD_EXPORT_001`）而不是静默丢掉敏感行，静默丢等于用户以为备份是全的。
//!
//! KDF/AEAD 档位刻意与 vault-core 同值（见 [`EXPORT_M_KIB`] 三常量），两模块各留一份
//! 常量早晚分叉；真正的钉在 src-tauri 的 `derive_key_matchesVaultCoreKdfTier`
//! （那是唯一同时看得见两个 crate 的层，clipboard-core 不该依赖 vault-core）。

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use host_core::error::AppError;
use host_core::util::{b64_decode, b64_encode};

pub const EXPORT_SCHEMA: u32 = 1;
/// 密文外的魔数：篡改/换格式即 AEAD 失败，导出文件不会被人改头换面后还被解成"成功"
pub const EXPORT_AAD: &[u8] = b"NFX-CLIP-EXPORT-1";
pub const MIN_PASSPHRASE: usize = 8;
pub const KEY_LEN: usize = 32;
pub const SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;
/// argon2id 档位：与 `vault_core::crypto::DEFAULT_M_COST_KIB`/`_T_COST`/`_P_COST` 同值
pub const EXPORT_M_KIB: u32 = 64 * 1024;
pub const EXPORT_T_COST: u32 = 3;
pub const EXPORT_P_COST: u32 = 4;

fn err(code: &str, msg: impl Into<String>) -> AppError {
    AppError::module(code, msg, None)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KdfParams {
    pub algo: String,
    pub m_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt_b64: String,
}

/// 明文可见的摘要（在密文之外）：不填口令也能知道这份文件大致装了什么。
/// 因此只放计数，永不放内容或组名；导入侧用 [`ImportReport`] 与本节的 `entries` 互校。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExportMeta {
    pub entries: u32,
    pub secrets: u32,
    pub images_skipped: u32,
    pub exported_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportEnvelopeV1 {
    pub schema: u32,
    pub kdf: KdfParams,
    pub nonce_b64: String,
    pub ct_b64: String,
    pub meta: ExportMeta,
}

/// 导出文件里的一行（内层 JSON 的元素，本身已在密文之内）。
///
/// 敏感行的明文放在 [`BackupRow::secret_b64`] 而不是 [`BackupRow::text`]：
/// `text` 是"列表可见的正文"槽位，敏感行在库内从来就没有可见正文，
/// 让两者各走各的槽位，导入侧就不可能把敏感行误当普通文本落库（那会当场取消信封加密）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupRow {
    pub content_type: String,
    pub text: String,
    #[serde(default)]
    pub html: Option<String>,
    #[serde(default)]
    pub group_name: Option<String>,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub secret_b64: Option<String>,
    #[serde(default)]
    pub source_app: Option<String>,
    pub created_at: i64,
    #[serde(default)]
    pub pinned: bool,
}

/// argon2id 派生 32B 密钥（默认档位）
pub fn derive_key(passphrase: &str, salt: &[u8]) -> [u8; KEY_LEN] {
    derive_key_with(passphrase, salt, EXPORT_M_KIB, EXPORT_T_COST, EXPORT_P_COST)
}

pub fn derive_key_with(
    passphrase: &str,
    salt: &[u8],
    m_kib: u32,
    t_cost: u32,
    p_cost: u32,
) -> [u8; KEY_LEN] {
    let params = Params::new(m_kib, t_cost, p_cost, Some(KEY_LEN))
        .expect("EXPORT_* 常量组合必须是 argon2 合法参数");
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut okm = [0u8; KEY_LEN];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut okm)
        .expect("输出缓冲长度由 Params::new 指定，不会失配");
    okm
}

/// 「包含敏感条目」时的口令门：短口令等于把口令这道门让出去，故拒而非降级导出。
pub fn check_export_passphrase(passphrase: &str, include_secrets: bool) -> Result<(), AppError> {
    if !include_secrets {
        return Ok(());
    }
    if passphrase.chars().count() >= MIN_PASSPHRASE {
        return Ok(());
    }
    Err(err(
        "CLIPBOARD_EXPORT_001",
        format!("包含敏感条目时口令至少 {MIN_PASSPHRASE} 字符"),
    ))
}

/// 加密内层 JSON；返回的信封 `meta` 为零值，由调用方按实际导出行数填（计数在密文之外）。
pub fn encrypt_backup(
    json: &[u8],
    passphrase: &str,
    exported_at_ms: u64,
) -> Result<ExportEnvelopeV1, AppError> {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    seal_with(
        json,
        passphrase,
        &salt,
        &nonce,
        EXPORT_M_KIB,
        EXPORT_T_COST,
        EXPORT_P_COST,
        exported_at_ms,
    )
}

/// 测试档：低 KDF 参数走同一套代码（正式档位由 [`encrypt_backup`] 固定）。
/// 与 vault-core 的 `create_vault` / `create_vault_with` 同一纪律：正式参数只在专测里跑一次。
#[cfg(test)]
pub(crate) fn encrypt_backup_dev(
    json: &[u8],
    passphrase: &str,
    exported_at_ms: u64,
) -> Result<ExportEnvelopeV1, AppError> {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    seal_with(
        json,
        passphrase,
        &salt,
        &nonce,
        8 * 1024,
        1,
        1,
        exported_at_ms,
    )
}

#[allow(clippy::too_many_arguments)]
fn seal_with(
    json: &[u8],
    passphrase: &str,
    salt: &[u8],
    nonce: &[u8],
    m_kib: u32,
    t_cost: u32,
    p_cost: u32,
    exported_at_ms: u64,
) -> Result<ExportEnvelopeV1, AppError> {
    let key = derive_key_with(passphrase, salt, m_kib, t_cost, p_cost);
    let cipher = Aes256Gcm::new(aes_gcm::Key::<Aes256Gcm>::from_slice(&key));
    let ct = cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: json,
                aad: EXPORT_AAD,
            },
        )
        .map_err(|_| err("CLIPBOARD_EXPORT_003", "备份加密失败"))?;
    Ok(ExportEnvelopeV1 {
        schema: EXPORT_SCHEMA,
        kdf: KdfParams {
            algo: "argon2id".into(),
            m_kib,
            t_cost,
            p_cost,
            salt_b64: b64_encode(salt),
        },
        nonce_b64: b64_encode(nonce),
        ct_b64: b64_encode(&ct),
        meta: ExportMeta {
            entries: 0,
            secrets: 0,
            images_skipped: 0,
            exported_at_ms,
        },
    })
}

/// 解出信封外壳：非本格式 / schema 版本不符 / 未知 KDF 算法一律 `CLIPBOARD_IMPORT_003`
pub fn decode_envelope(raw: &[u8]) -> Result<ExportEnvelopeV1, AppError> {
    let env: ExportEnvelopeV1 = serde_json::from_slice(raw).map_err(|e| {
        err(
            "CLIPBOARD_IMPORT_003",
            format!("不是本应用的备份文件格式: {e}"),
        )
    })?;
    if env.schema != EXPORT_SCHEMA {
        return Err(err(
            "CLIPBOARD_IMPORT_003",
            format!("备份 schema 版本 {} 不受本版本支持", env.schema),
        ));
    }
    if env.kdf.algo != "argon2id" {
        return Err(err(
            "CLIPBOARD_IMPORT_003",
            format!("未知 KDF 算法「{}」", env.kdf.algo),
        ));
    }
    Ok(env)
}

pub fn decrypt_backup(env: &ExportEnvelopeV1, passphrase: &str) -> Result<Vec<u8>, AppError> {
    let salt = b64_decode(&env.kdf.salt_b64)
        .ok_or_else(|| err("CLIPBOARD_IMPORT_003", "kdf.salt_b64 不是合法 base64"))?;
    let nonce = b64_decode(&env.nonce_b64)
        .ok_or_else(|| err("CLIPBOARD_IMPORT_003", "nonce_b64 不是合法 base64"))?;
    let ct = b64_decode(&env.ct_b64)
        .ok_or_else(|| err("CLIPBOARD_IMPORT_003", "ct_b64 不是合法 base64"))?;
    if nonce.len() != NONCE_LEN {
        return Err(err("CLIPBOARD_IMPORT_003", "nonce 长度非法"));
    }
    let key = derive_key_with(
        passphrase,
        &salt,
        env.kdf.m_kib,
        env.kdf.t_cost,
        env.kdf.p_cost,
    );
    let cipher = Aes256Gcm::new(aes_gcm::Key::<Aes256Gcm>::from_slice(&key));
    cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ct,
                aad: EXPORT_AAD,
            },
        )
        // AEAD 失败有两种成因（口令错 / 文件被篡改），从外部无法也不该分辨——
        // 分辨它等于给攻击者一个 oracle。两种都报同一句，且绝不退化解明文。
        .map_err(|_| {
            err(
                "CLIPBOARD_IMPORT_001",
                "口令错误或文件被篡改（认证解密失败）",
            )
        })
}

pub fn parse_backup_rows(json: &[u8]) -> Result<Vec<BackupRow>, AppError> {
    serde_json::from_slice(json)
        .map_err(|e| err("CLIPBOARD_IMPORT_003", format!("备份内容解析失败: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试档密钥材料：只够证明"派生确定性 + 盐参与派生"，不代表正式强度
    const DEV_M_KIB: u32 = 8 * 1024;

    fn dev_derive(pass: &str, salt: &[u8]) -> [u8; KEY_LEN] {
        derive_key_with(pass, salt, DEV_M_KIB, 1, 1)
    }

    #[test]
    fn derive_key_is_deterministic_and_salt_sensitive() {
        let salt = [7u8; SALT_LEN];
        let a = dev_derive("correct horse", &salt);
        let b = dev_derive("correct horse", &salt);
        assert_eq!(a, b, "同口令同盐必须同密钥（否则备份文件解不开）");
        let c = dev_derive("correct horse", &[8u8; SALT_LEN]);
        assert_ne!(a, c, "盐必须进派生（同口令同盐才是一对）");
        let d = dev_derive("correct hprse", &salt);
        assert_ne!(a, d, "一字之差的口令必须派生出不同密钥");
    }

    #[test]
    fn export_envelope_roundtrip_and_aad_tamper_detected() {
        let json = r#"[{"content_type":"text","text":"一行备份正文"}]"#;
        let env = encrypt_backup_dev(json.as_bytes(), "passphrase!", 1).unwrap();
        assert_eq!(env.schema, EXPORT_SCHEMA);
        assert_eq!(env.kdf.algo, "argon2id");
        assert_eq!(
            String::from_utf8(decrypt_backup(&env, "passphrase!").unwrap()).unwrap(),
            json,
            "往返必须逐字节等值"
        );

        let mut tampered = env.clone();
        tampered.ct_b64 = {
            let mut bytes = b64_decode(&env.ct_b64).unwrap();
            let last = bytes.len() - 1;
            bytes[last] ^= 0x01;
            b64_encode(&bytes)
        };
        let e = decrypt_backup(&tampered, "passphrase!").unwrap_err();
        assert!(
            matches!(&e, AppError::Module { code, .. } if code == "CLIPBOARD_IMPORT_001"),
            "密文位翻转须以认证失败拒：{e:?}"
        );

        let mut renamed = env.clone();
        renamed.kdf.algo = "argon2i".into();
        assert!(matches!(
            decode_envelope(&serde_json::to_vec(&renamed).unwrap()),
            Err(AppError::Module { code, .. }) if code == "CLIPBOARD_IMPORT_003"
        ));

        let mut newer = env.clone();
        newer.schema = EXPORT_SCHEMA + 1;
        let e = decode_envelope(&serde_json::to_vec(&newer).unwrap()).unwrap_err();
        match e {
            AppError::Module { message, .. } => assert!(
                message.contains("2"),
                "版本不符须点名收到的版本号，不是只说「格式不对」：{message}"
            ),
            other => panic!("预期 Module 型错误，得到 {other:?}"),
        }
    }

    #[test]
    fn export_gate_requires_passphrase_only_when_secrets_included() {
        check_export_passphrase("", false).unwrap();
        assert!(matches!(
            check_export_passphrase("", true),
            Err(AppError::Module { code, .. }) if code == "CLIPBOARD_EXPORT_001"
        ));
        assert!(matches!(
            check_export_passphrase("1234567", true),
            Err(AppError::Module { .. })
        ));
        check_export_passphrase("12345678", true).unwrap();
        // 门按字符数而非字节数：中文口令不该被 UTF-8 长度虚高放行
        assert!(matches!(
            check_export_passphrase("口令口令", true),
            Err(AppError::Module { code, .. }) if code == "CLIPBOARD_EXPORT_001"
        ));
        check_export_passphrase("口令口令口令口令", true).unwrap();
    }
}
