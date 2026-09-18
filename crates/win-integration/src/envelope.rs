//! 剪贴板敏感条目信封加密（DECISIONS D-04，DESIGN §4.1/§8.5 安全红线）
//!
//! 正文用随机 DEK（AES-256-GCM）加密；DEK 由 KEK（DPAPI 当前用户作用域）包裹后随密文持久化。
//! DPAPI 只承担 KEK 保护，不再直接加密正文（旧口径），以满足 §8.5「标准加密原语」
//! 与 sync 端到端前提。
//!
//! 存储格式：`"NFX1" | u16le wrapped_len | wrapped_dek | nonce(12) | ct‖tag`，AAD 绑定魔数。
//! 读取兼容：无 `NFX1` 前缀的既有条目按遗留 DPAPI 直存密文原样解密（无发布版本，仅需一次读兼容）。

use std::sync::Arc;

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use host_core::error::AppError;
use host_core::ports::CryptoPort;
use rand::RngCore;
use zeroize::Zeroizing;

use crate::dpapi::Dpapi;

const MAGIC: &[u8; 4] = b"NFX1";
const DEK_LEN: usize = 32;
const NONCE_LEN: usize = 12;

/// DEK 包裹原语（KEK 层）。生产实现为 DPAPI；单测注入内存包裹以覆盖负例。
pub trait KeyWrap: Send + Sync {
    fn wrap(&self, key: &[u8]) -> Result<Vec<u8>, AppError>;
    fn unwrap(&self, wrapped: &[u8]) -> Result<Vec<u8>, AppError>;
}

/// DPAPI 实现的 KEK 包裹（当前用户作用域）
pub struct DpapiWrap;

impl KeyWrap for DpapiWrap {
    fn wrap(&self, key: &[u8]) -> Result<Vec<u8>, AppError> {
        Dpapi.protect(key)
    }
    fn unwrap(&self, wrapped: &[u8]) -> Result<Vec<u8>, AppError> {
        Dpapi.unprotect(wrapped)
    }
}

pub struct EnvelopeCrypto {
    wrap: Arc<dyn KeyWrap>,
}

impl EnvelopeCrypto {
    pub fn new(wrap: Arc<dyn KeyWrap>) -> Self {
        Self { wrap }
    }
    /// 生产装配：KEK = DPAPI 当前用户
    pub fn with_dpapi() -> Self {
        Self::new(Arc::new(DpapiWrap))
    }
}

impl CryptoPort for EnvelopeCrypto {
    fn protect(&self, plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
        let mut dek = Zeroizing::new([0u8; DEK_LEN]);
        rand::rngs::OsRng.fill_bytes(dek.as_mut());
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

        let cipher = Aes256Gcm::new_from_slice(dek.as_slice())
            .map_err(|e| AppError::module("CLIPBOARD_CRYPTO_001", e.to_string(), None))?;
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload { msg: plaintext, aad: MAGIC },
            )
            .map_err(|_| {
                AppError::module(
                    "CLIPBOARD_CRYPTO_001",
                    "AES-256-GCM 加密失败",
                    Some("内存压力或 provider 异常，可重试"),
                )
            })?;
        let wrapped = self.wrap.wrap(dek.as_slice())?;
        // dek 出作用域即被 Zeroizing 清零；返回值只含包裹后的密钥材料

        let mut out = Vec::with_capacity(4 + 2 + wrapped.len() + NONCE_LEN + ct.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(wrapped.len() as u16).to_le_bytes());
        out.extend_from_slice(&wrapped);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    fn unprotect(&self, ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
        if !ciphertext.starts_with(MAGIC) {
            // 遗留条目：DPAPI 直接加密正文（D-04 改造前写入），保持可读
            return self.wrap.unwrap(ciphertext);
        }
        let bad = || {
            AppError::module(
                "CLIPBOARD_CRYPTO_002",
                "信封密文格式损坏",
                Some("删除该条目后重新复制"),
            )
        };
        if ciphertext.len() < 4 + 2 + NONCE_LEN {
            return Err(bad());
        }
        let wrapped_len = u16::from_le_bytes([ciphertext[4], ciphertext[5]]) as usize;
        let wrapped_end = 6usize.checked_add(wrapped_len).ok_or_else(bad)?;
        let nonce_end = wrapped_end.checked_add(NONCE_LEN).ok_or_else(bad)?;
        if nonce_end > ciphertext.len() {
            return Err(bad());
        }
        let wrapped = &ciphertext[wrapped_end - wrapped_len..wrapped_end];
        let nonce = &ciphertext[wrapped_end..nonce_end];
        let ct = &ciphertext[nonce_end..];

        let dek = Zeroizing::new(self.wrap.unwrap(wrapped)?);
        let cipher = Aes256Gcm::new_from_slice(&dek)
            .map_err(|_| AppError::module("CLIPBOARD_CRYPTO_002", "DEK 长度非法", None))?;
        cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload { msg: ct, aad: MAGIC },
            )
            .map_err(|_| {
                AppError::module(
                    "CLIPBOARD_CRYPTO_002",
                    "AES-256-GCM 解密失败（密钥不符或密文被篡改）",
                    Some("该条目无法解密；确认本机用户未变更"),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内存 KEK 包裹：key[i] ^ kek —— 可注入不同 kek 模拟"错误密钥"
    struct MemWrap {
        kek: Vec<u8>,
    }
    impl KeyWrap for MemWrap {
        fn wrap(&self, key: &[u8]) -> Result<Vec<u8>, AppError> {
            Ok(key.iter().enumerate().map(|(i, b)| b ^ self.kek[i % self.kek.len()]).collect())
        }
        fn unwrap(&self, wrapped: &[u8]) -> Result<Vec<u8>, AppError> {
            Ok(wrapped.iter().enumerate().map(|(i, b)| b ^ self.kek[i % self.kek.len()]).collect())
        }
    }

    fn mem(kek: &[u8]) -> EnvelopeCrypto {
        EnvelopeCrypto::new(Arc::new(MemWrap { kek: kek.to_vec() }))
    }

    #[test]
    fn envelope_roundtrip_and_randomness() {
        let c = mem(b"kek-A");
        let pt = b"password=hunter2".to_vec();
        let c1 = c.protect(&pt).unwrap();
        let c2 = c.protect(&pt).unwrap();
        assert!(c1.starts_with(MAGIC));
        assert_ne!(c1, c2, "DEK/nonce 必须每次随机");
        assert_eq!(c.unprotect(&c1).unwrap(), pt);
        assert_eq!(c.unprotect(&c2).unwrap(), pt);
    }

    #[test]
    fn wrong_kek_fails_authentication() {
        let c = mem(b"kek-A");
        let sealed = c.protect(b"top-secret".as_slice()).unwrap();
        let attacker = mem(b"kek-B");
        // KEK 不符 → 解出错误 DEK → GCM 认证失败（不得返回任何明文）
        assert!(attacker.unprotect(&sealed).is_err());
    }

    #[test]
    fn tampered_ciphertext_fails_authentication() {
        let c = mem(b"kek-A");
        let mut sealed = c.protect(b"api-key-123".as_slice()).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01; // 篡改 ct/tag 末字节
        assert!(c.unprotect(&sealed).is_err());

        let mut sealed2 = c.protect(b"api-key-123".as_slice()).unwrap();
        sealed2[6] ^= 0x01; // 篡改 wrapped_dek 首字节
        assert!(c.unprotect(&sealed2).is_err());
    }

    #[test]
    fn truncated_or_corrupt_envelope_rejected() {
        let c = mem(b"kek-A");
        assert!(c.unprotect(MAGIC).is_err(), "仅魔数");
        let sealed = c.protect(b"x".as_slice()).unwrap();
        assert!(c.unprotect(&sealed[..10]).is_err(), "截断");
        // wrapped_len 谎报超出总长
        let mut lie = sealed.clone();
        let lie_len = (sealed.len() as u16) + 100;
        lie[4..6].copy_from_slice(&lie_len.to_le_bytes());
        assert!(c.unprotect(&lie).is_err());
    }

    #[test]
    fn legacy_dpapi_blob_passes_through_untouched() {
        // 无 NFX1 前缀 = 遗留直存密文：原样交给 KEK 层 unwrap（DPAPI 语义）
        struct Echo;
        impl KeyWrap for Echo {
            fn wrap(&self, key: &[u8]) -> Result<Vec<u8>, AppError> {
                Ok(format!("WRAPPED({})", String::from_utf8_lossy(key)).into_bytes())
            }
            fn unwrap(&self, wrapped: &[u8]) -> Result<Vec<u8>, AppError> {
                Ok(wrapped.to_vec())
            }
        }
        let c = EnvelopeCrypto::new(Arc::new(Echo));
        let legacy = vec![0x01, 0x00, 0x00, 0xFF, 0xAA];
        assert_eq!(c.unprotect(&legacy).unwrap(), legacy);
    }

    #[test]
    fn aad_mismatch_rejected() {
        // 手工构造：用不同 AAD 加密的包必须被本实现拒绝
        let dek = [7u8; DEK_LEN];
        let nonce = [3u8; NONCE_LEN];
        let cipher = Aes256Gcm::new_from_slice(&dek).unwrap();
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: b"hi".as_slice(), aad: b"EVIL" })
            .unwrap();
        let wrapped = dek.iter().enumerate().map(|(i, b)| b ^ b"kek-A"[i % 5]).collect::<Vec<_>>();
        let mut blob = Vec::new();
        blob.extend_from_slice(MAGIC);
        blob.extend_from_slice(&(wrapped.len() as u16).to_le_bytes());
        blob.extend_from_slice(&wrapped);
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ct);
        assert!(mem(b"kek-A").unprotect(&blob).is_err());
    }

    /// 真机 DPAPI 往返（KEK=当前用户），验证生产装配路径
    #[test]
    fn dpapi_kek_roundtrip_real() {
        let c = EnvelopeCrypto::with_dpapi();
        let pt = b"13800000000-verification".to_vec();
        let sealed = c.protect(&pt).unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert_eq!(c.unprotect(&sealed).unwrap(), pt);
    }
}
