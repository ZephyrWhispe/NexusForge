//! 设备信任根（D-02 上移自 kvm-core）：本机 X25519 身份 + 已配对白名单。
//!
//! 信任根是宿主级状态（{appData}/kvm/identity.json / paired.json）：KVM 与 SYNC
//! 两模块在同机共享同一身份与配对表（docs/impl/05 K2 / impl/07 SYNC1），故定义在
//! host-core，模块 crate 一律经此消费，杜绝模块间直接依赖（DESIGN O1）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::error::{AppError, ModuleError};
use crate::ports::CryptoPort;

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
    pub fn load_or_create(
        dir: &PathBuf,
        crypto: Option<Arc<dyn CryptoPort>>,
    ) -> Result<Self, ModuleError> {
        let path = dir.join("identity.json");
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(id) = deserialize_identity(&bytes, crypto.clone()) {
                tracing::info!(device_id = %id.device_id, "设备身份已加载");
                return Ok(id);
            }
            tracing::warn!("身份文件损坏，重新生成");
        }
        let device_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "NexusForge".into());
        let id = Self::generate(device_name);
        let bytes = serialize_identity(&id, crypto)?;
        std::fs::create_dir_all(dir).map_err(|e| ModuleError::Init(e.to_string()))?;
        std::fs::write(&path, bytes).map_err(|e| ModuleError::Init(e.to_string()))?;
        tracing::info!(device_id = %id.device_id, "设备身份已创建");
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

fn serialize_identity(
    id: &DeviceIdentity,
    crypto: Option<Arc<dyn CryptoPort>>,
) -> Result<Vec<u8>, ModuleError> {
    let secret_bytes = id.secret.to_bytes();
    let (secret_b64, protected) = match &crypto {
        Some(port) => {
            let sealed = port
                .protect(&secret_bytes)
                .map_err(|e| ModuleError::Init(format!("DPAPI 保护失败: {e}")))?;
            (b64_encode(&sealed), true)
        }
        None => (b64_encode(&secret_bytes), false),
    };
    let persisted = PersistedIdentity {
        device_id: id.device_id.clone(),
        device_name: id.device_name.clone(),
        secret_b64,
        protected,
    };
    serde_json::to_vec_pretty(&persisted).map_err(|e| ModuleError::Init(e.to_string()))
}

fn deserialize_identity(
    bytes: &[u8],
    crypto: Option<Arc<dyn CryptoPort>>,
) -> Result<DeviceIdentity, String> {
    let persisted: PersistedIdentity = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let raw = b64_decode(&persisted.secret_b64).ok_or("私钥 base64 非法")?;
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

// ---------------------------------------------------------------------------
// 配对设备持久化（{appData}/kvm/paired.json）
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairedPeer {
    pub device_id: String,
    pub device_name: String,
    /// SHA256(公钥) hex 前 32 位——会话准入白名单依据
    pub fingerprint: String,
    /// base64(X25519 公钥)
    pub pubkey_b64: String,
    /// unix 毫秒
    pub paired_at: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct PersistedPeers {
    peers: Vec<PairedPeer>,
}

/// 已配对设备表（KVM/SYNC 共享白名单）
pub struct PairStore {
    path: PathBuf,
    peers: RwLock<HashMap<String, PairedPeer>>,
}

impl PairStore {
    pub fn load_or_default(dir: &PathBuf) -> Result<Self, AppError> {
        std::fs::create_dir_all(dir).map_err(|e| {
            AppError::module("KVM_PAIR_002", format!("创建 kvm 目录失败: {e}"), None)
        })?;
        let path = dir.join("paired.json");
        let mut map = HashMap::new();
        if let Ok(bytes) = std::fs::read(&path) {
            let persisted: PersistedPeers = serde_json::from_slice(&bytes).map_err(|e| {
                AppError::module("KVM_PAIR_001", format!("paired.json 损坏: {e}"), None)
            })?;
            for p in persisted.peers {
                map.insert(p.device_id.clone(), p);
            }
        }
        Ok(Self {
            path,
            peers: RwLock::new(map),
        })
    }

    pub fn is_paired(&self, device_id: &str) -> bool {
        self.peers.read().expect("peers 锁").contains_key(device_id)
    }

    pub fn get(&self, device_id: &str) -> Option<PairedPeer> {
        self.peers.read().expect("peers 锁").get(device_id).cloned()
    }

    /// 指纹白名单校验（K3 会话准入）
    pub fn verify_fingerprint(&self, device_id: &str, fingerprint: &str) -> bool {
        self.peers
            .read()
            .expect("peers 锁")
            .get(device_id)
            .map(|p| p.fingerprint == fingerprint)
            .unwrap_or(false)
    }

    pub fn all(&self) -> Vec<PairedPeer> {
        let mut list: Vec<PairedPeer> = self
            .peers
            .read()
            .expect("peers 锁")
            .values()
            .cloned()
            .collect();
        list.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        list
    }

    /// 登记配对记录（K2 配对流程写入；sync-core 测试/对账也复用）
    pub fn upsert(&self, peer: PairedPeer) -> Result<(), AppError> {
        self.peers
            .write()
            .expect("peers 锁")
            .insert(peer.device_id.clone(), peer);
        self.persist()
    }

    pub fn remove(&self, device_id: &str) -> Result<bool, AppError> {
        let removed = self
            .peers
            .write()
            .expect("peers 锁")
            .remove(device_id)
            .is_some();
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    fn persist(&self) -> Result<(), AppError> {
        let snapshot = PersistedPeers { peers: self.all() };
        let json = serde_json::to_vec_pretty(&snapshot)
            .map_err(|e| AppError::module("KVM_PAIR_002", e.to_string(), None))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)
            .map_err(|e| AppError::module("KVM_PAIR_002", e.to_string(), None))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| AppError::module("KVM_PAIR_002", e.to_string(), None))?;
        Ok(())
    }
}

/// base64 编解码（配对记录公钥/会话公钥传输；KVM 配对与 SYNC 握手复用）
pub fn b64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
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
        let dir = std::env::temp_dir().join(format!("nf-device-id-test-{}", uuid::Uuid::now_v7()));
        let id = DeviceIdentity::load_or_create(&dir, None).unwrap();
        let reloaded = DeviceIdentity::load_or_create(&dir, None).unwrap();
        assert_eq!(id.device_id, reloaded.device_id);
        assert_eq!(id.pubkey_fingerprint, reloaded.pubkey_fingerprint);
        assert_eq!(id.public_key(), reloaded.public_key());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pair_store_roundtrip_persists() {
        let dir =
            std::env::temp_dir().join(format!("nf-device-pair-test-{}", uuid::Uuid::now_v7()));
        let store = PairStore::load_or_default(&dir).unwrap();
        assert!(store.all().is_empty());
        let peer = PairedPeer {
            device_id: "dev-1".into(),
            device_name: "测试机".into(),
            fingerprint: "ab".repeat(16),
            pubkey_b64: b64_encode(&[1u8; 32]),
            paired_at: 0,
        };
        store.upsert(peer.clone()).unwrap();
        assert!(store.is_paired("dev-1"));
        assert!(store.verify_fingerprint("dev-1", &peer.fingerprint));
        assert!(!store.verify_fingerprint("dev-1", "wrong"));
        // 重新加载走磁盘（tmp+rename 持久化）
        let reloaded = PairStore::load_or_default(&dir).unwrap();
        assert_eq!(reloaded.get("dev-1").unwrap().device_name, "测试机");
        assert!(reloaded.remove("dev-1").unwrap());
        assert!(!reloaded.is_paired("dev-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D-02 负例：篡改 paired.json 中指纹与公钥的绑定不影响存储层语义，
    /// 但 b64 非法公钥必须在校验路径被拒（sync/kvm 握手共用此约定）
    #[test]
    fn b64_helpers_reject_garbage() {
        assert_eq!(b64_encode(&[0x00, 0xFF]), "AP8=");
        assert_eq!(b64_decode("AP8=").unwrap(), vec![0x00, 0xFF]);
        assert!(b64_decode("!!!not-base64!!!").is_none());
    }
}
