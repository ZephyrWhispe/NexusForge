//! 设备身份（docs/impl/05 K1/K2）：X25519 静态密钥对 + 公钥指纹 + 持久化。
//!
//! 指纹 = SHA256(公钥) 十六进制前 32 位；配对时双方核对指纹防中间人。
//! 私钥落盘经 CryptoPort（DPAPI）保护，无端口时明文（仅开发态）。

use std::path::PathBuf;
use std::sync::Arc;

use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use host_core::error::ModuleError;
use host_core::ports::CryptoPort;

/// X25519 公钥字节数
const KEY_LEN: usize = 32;
/// 指纹展示长度（hex 字符数）
const FINGERPRINT_HEX: usize = 32;

#[derive(Serialize, Deserialize)]
struct PersistedIdentity {
    device_id: String,
    device_name: String,
    /// base64(X25519 私钥)；enable_protect=true 时为 DPAPI 密文
    secret_b64: String,
    protected: bool,
}

pub struct DeviceIdentity {
    pub device_id: String,
    pub device_name: String,
    pub pubkey_fingerprint: String,
    secret: StaticSecret,
}

impl DeviceIdentity {
    /// X25519 公钥原始字节
    pub fn public_key(&self) -> [u8; KEY_LEN] {
        PublicKey::from(&self.secret).to_bytes()
    }

    /// 计算本机与对端指纹是否一致（配对比对入口）
    pub fn fingerprint_of(pubkey: &[u8; KEY_LEN]) -> String {
        fingerprint(pubkey)
    }

    /// 静态 X25519 DH（K3 会话鉴权：配对双方各自持有对端静态公钥）。
    /// 共享密钥为全零（小群元素）时返回 None。
    pub fn diffie_hellman(&self, peer_pub: &[u8; KEY_LEN]) -> Option<[u8; KEY_LEN]> {
        let shared = self.secret.diffie_hellman(&PublicKey::from(*peer_pub));
        if shared.was_contributory() {
            Some(shared.to_bytes())
        } else {
            None
        }
    }

    /// 生成新身份（随机密钥 + 新 uuid）
    pub fn generate(device_name: String) -> Self {
        let secret = StaticSecret::random_from_rng(OsRng);
        Self::from_secret(secret, device_name)
    }

    fn from_secret(secret: StaticSecret, device_name: String) -> Self {
        let pubkey = PublicKey::from(&secret);
        Self {
            device_id: uuid::Uuid::now_v7().to_string(),
            device_name,
            pubkey_fingerprint: fingerprint(&pubkey.to_bytes()),
            secret,
        }
    }

    /// 加载或创建身份文件（{appData}/kvm/identity.json）
    pub fn load_or_create(dir: &PathBuf, crypto: Option<Arc<dyn CryptoPort>>) -> Result<Self, ModuleError> {
        let path = dir.join("identity.json");
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(id) = self::deserialize(&bytes, crypto.clone()) {
                tracing::info!(device_id = %id.device_id, "KVM 身份已加载");
                return Ok(id);
            }
            tracing::warn!("KVM 身份文件损坏，重新生成");
        }
        let device_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "NexusForge".into());
        let id = Self::generate(device_name);
        let bytes = self::serialize(&id, crypto)?;
        std::fs::create_dir_all(dir).map_err(|e| ModuleError::Init(e.to_string()))?;
        std::fs::write(&path, bytes).map_err(|e| ModuleError::Init(e.to_string()))?;
        tracing::info!(device_id = %id.device_id, "KVM 身份已创建");
        Ok(id)
    }
}

fn fingerprint(pubkey: &[u8; KEY_LEN]) -> String {
    let digest = Sha256::digest(pubkey);
    digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..FINGERPRINT_HEX]
        .to_string()
}

fn serialize(id: &DeviceIdentity, crypto: Option<Arc<dyn CryptoPort>>) -> Result<Vec<u8>, ModuleError> {
    let secret_bytes = id.secret.to_bytes();
    let (secret_b64, protected) = match &crypto {
        Some(port) => {
            let sealed = port
                .protect(&secret_bytes)
                .map_err(|e| ModuleError::Init(format!("DPAPI 保护失败: {e}")))?;
            (base64_encode(&sealed), true)
        }
        None => (base64_encode(&secret_bytes), false),
    };
    let persisted = PersistedIdentity {
        device_id: id.device_id.clone(),
        device_name: id.device_name.clone(),
        secret_b64,
        protected,
    };
    serde_json::to_vec_pretty(&persisted).map_err(|e| ModuleError::Init(e.to_string()))
}

fn deserialize(bytes: &[u8], crypto: Option<Arc<dyn CryptoPort>>) -> Result<DeviceIdentity, String> {
    let persisted: PersistedIdentity = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let raw = base64_decode(&persisted.secret_b64).ok_or("私钥 base64 非法")?;
    let secret_bytes: [u8; KEY_LEN] = if persisted.protected {
        let port = crypto.ok_or("身份受 DPAPI 保护但 CryptoPort 缺失")?;
        let opened = port.unprotect(&raw).map_err(|e| e.to_string())?;
        opened.try_into().map_err(|_| "DPAPI 明文长度非法")?
    } else {
        raw.try_into().map_err(|_| "私钥长度非法")?
    };
    let secret = StaticSecret::from(secret_bytes);
    let pubkey = PublicKey::from(&secret);
    Ok(DeviceIdentity {
        device_id: persisted.device_id,
        device_name: persisted.device_name,
        pubkey_fingerprint: fingerprint(&pubkey.to_bytes()),
        secret,
    })
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_deterministic_and_truncated() {
        let mut pk = [0u8; KEY_LEN];
        pk[0] = 7;
        let f1 = fingerprint(&pk);
        let f2 = DeviceIdentity::fingerprint_of(&pk);
        assert_eq!(f1, f2);
        assert_eq!(f1.len(), FINGERPRINT_HEX);
        // 不同公钥指纹必不同
        pk[0] = 8;
        assert_ne!(f1, fingerprint(&pk));
    }

    #[test]
    fn roundtrip_plain_identity() {
        let dir = std::env::temp_dir().join(format!("kvm-id-test-{}", uuid::Uuid::now_v7()));
        let id = DeviceIdentity::load_or_create(&dir, None).unwrap();
        let reloaded = DeviceIdentity::load_or_create(&dir, None).unwrap();
        assert_eq!(id.device_id, reloaded.device_id);
        assert_eq!(id.pubkey_fingerprint, reloaded.pubkey_fingerprint);
        assert_eq!(id.public_key(), reloaded.public_key());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
