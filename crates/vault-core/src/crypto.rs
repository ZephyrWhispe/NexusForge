//! V1 加密核心（docs/impl/05 V1）：Argon2id KDF + AES-256-GCM 信封。
//!
//! 信封结构（规格）：
//! ```text
//! master_password --Argon2id(m=64MiB,t=3,p=4,salt)--> KEK
//! DEK（随机 32B）被 KEK 包裹存头部；改密码只重包 DEK，数据条目不动
//! 字段加密：AES-256-GCM(DEK)，nonce 随机 12B，aad 绑定条目 id（防密文换位）
//! ```
//! 内存纪律：密钥一律 [`SecretKey`]——zeroize on Drop、不派生 Debug/Display。
//! VirtualLock 锁页经 [`set_mem_lock`] 注入的 win-integration 端口接入（D-24）。

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::rngs::OsRng;
use rand::RngCore;
use std::sync::{Arc, OnceLock};
use zeroize::Zeroize;

use host_core::error::AppError;
use host_core::ports::MemLockPort;

/// 信封 AES-GCM nonce 长度
pub const NONCE_LEN: usize = 12;
/// DEK / KEK 长度
pub const KEY_LEN: usize = 32;
/// 默认 KDF 参数（规格：64MiB / t=3 / p=4）
pub const DEFAULT_M_COST_KIB: u32 = 64 * 1024;
pub const DEFAULT_T_COST: u32 = 3;
pub const DEFAULT_P_COST: u32 = 4;
/// 盐长度
pub const SALT_LEN: usize = 16;

// ---------------------------------------------------------------------------
// SecretKey：zeroize 内存纪律
// ---------------------------------------------------------------------------

/// 32B 对称密钥。**故意不实现 Debug/Display**（clippy 约束见 crate 根），
/// Drop 时内存清零；[`wipe`](Self::wipe) 供锁定流程显式调用。
///
/// COR-03：内层数组用 `Box` 承载——内联数组**移动即复制**，`VirtualLock` 锁的
/// 地址会随移动失效；堆分配使地址跨移动稳定，锁页/解锁始终作用于同一块内存。
pub struct SecretKey(Box<[u8; KEY_LEN]>);

impl SecretKey {
    pub fn new(bytes: [u8; KEY_LEN]) -> Self {
        Self(Box::new(bytes))
    }

    pub fn generate() -> Self {
        let mut k = Box::new([0u8; KEY_LEN]);
        OsRng.fill_bytes(k.as_mut());
        Self(k)
    }

    /// 仅限加解密调用点使用；调用方不得复制/打印
    pub fn expose(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// 显式清零（V3 锁定流程；Drop 是兜底）
    pub fn wipe(&mut self) {
        self.0.zeroize();
    }
}

impl Drop for SecretKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl SecretKey {
    /// 将密钥缓冲区锁定到物理内存（防换页泄露，D-24）。
    /// 端口未注册或锁定失败仅 warn——明文密钥短暂驻留页文件是降级而非致命错误，
    /// 绝不阻断解锁流程（D-24 验收⑥）。
    pub fn lock_in_memory(&self) {
        let Some(port) = mem_lock_port() else { return };
        let ptr = self.0.as_ptr() as usize;
        if !port.lock(ptr, KEY_LEN) {
            tracing::warn!("VirtualLock 锁页失败：DEK 可能驻留页文件（不阻断解锁）");
        }
    }

    /// 解锁缓冲区（锁定前调用；端口缺失时为 no-op）。
    /// COR-03：解锁失败必须可见——VirtualUnlock 失败通常意味着"从未锁过该地址"，
    /// 是内存锁状态不一致的信号，静默吞掉会掩盖锁错地址类缺陷。
    pub fn unlock_memory(&self) {
        if let Some(port) = mem_lock_port() {
            if !port.unlock(self.0.as_ptr() as usize, KEY_LEN) {
                tracing::warn!("VirtualUnlock 失败：地址可能从未锁定（内存锁状态不一致）");
            }
        }
    }
}

static MEM_LOCK: OnceLock<Arc<dyn MemLockPort>> = OnceLock::new();

/// 注册虚拟内存锁页端口（host 启动时经 win-integration 注入，D-24）
pub fn set_mem_lock(port: Arc<dyn MemLockPort>) {
    // 重复注册（测试/热重启）静默忽略首个之外的注入
    let _ = MEM_LOCK.set(port);
}

fn mem_lock_port() -> Option<&'static Arc<dyn MemLockPort>> {
    MEM_LOCK.get()
}

// ---------------------------------------------------------------------------
// Windows Hello 免密解锁信封（D-24 V4）
// ---------------------------------------------------------------------------

/// verifier 明文的域分隔常量（AES-GCM aad=vault_id 之外的第二重绑定）
pub const HELLO_VERIFIER: &[u8] = b"nf-vault-hello-v1";

/// Hello 路径信封：DEK 副本经 CryptoPort（DPAPI）包裹 + DEK 自证 verifier。
/// DPAPI 无法"由 Hello 派生"（D-24 决策①偏差记录）：Hello 校验作为解锁
/// 前置门（verify 通过才允许读取），DPAPI 提供账户域绑定，verifier 提供
/// 本库绑定——三者缺一即拒绝。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HelloEnvelope {
    /// CryptoPort.protect(DEK) 的 base64 密文
    pub wrapped_dek_b64: String,
    /// AES-GCM(DEK, aad=vault_id) 的固定明文密文，解密须还原 HELLO_VERIFIER
    pub verifier: WrappedKey,
}

/// 用 DEK 生成自证信封（启用 Hello 时调用）
pub fn seal_hello_verifier(dek: &SecretKey, vault_id: &str) -> Result<WrappedKey, AppError> {
    let (nonce, ct) = seal_raw(dek, HELLO_VERIFIER, vault_id.as_bytes())?;
    Ok(WrappedKey {
        nonce_b64: b64(&nonce),
        ct_b64: b64(&ct),
    })
}

/// 解验自证信封；明文必须等于 HELLO_VERIFIER，否则视为跨库/篡改（D-24 验收②）
pub fn open_hello_verifier(
    dek: &SecretKey,
    vault_id: &str,
    env: &WrappedKey,
) -> Result<(), AppError> {
    let nonce_vec = unb64(&env.nonce_b64)?;
    let ct = unb64(&env.ct_b64)?;
    let nonce: [u8; NONCE_LEN] = nonce_vec
        .try_into()
        .map_err(|_| vault_err("VAULT_CRYPTO_010", "hello verifier nonce 长度非法"))?;
    let plain = open_raw(dek, &nonce, &ct, vault_id.as_bytes())?;
    if plain != HELLO_VERIFIER {
        return Err(vault_err("VAULT_CRYPTO_011", "hello verifier 校验失败"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 信封头部（持久化 vault.meta.json）
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct KdfParams {
    pub algo: String,
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    /// base64
    pub salt_b64: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WrappedKey {
    /// base64
    pub nonce_b64: String,
    /// base64
    pub ct_b64: String,
}

/// 保险库头部：明文可存盘（机密性全部在 wrapped_dek 与字段密文里）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VaultHeader {
    pub version: u32,
    pub vault_id: String,
    pub kdf: KdfParams,
    pub wrapped_dek: WrappedKey,
    /// Windows Hello 免密路径（D-24 V4），未启用 = None；旧文件缺字段兼容
    #[serde(default)]
    pub hello: Option<HelloEnvelope>,
}

impl VaultHeader {
    pub fn current_version() -> u32 {
        1
    }
}

fn b64(bytes: &[u8]) -> String {
    host_core::util::b64_encode(bytes)
}

fn unb64(s: &str) -> Result<Vec<u8>, AppError> {
    host_core::util::b64_decode(s)
        .ok_or_else(|| AppError::module("VAULT_CRYPTO_004", "base64 解码失败", None))
}

fn vault_err(code: &str, msg: impl Into<String>) -> AppError {
    AppError::module(code, msg, None)
}

// ---------------------------------------------------------------------------
// KDF 与信封
// ---------------------------------------------------------------------------

/// Argon2id 派生 32B KEK
pub fn derive_kek(password: &str, kdf: &KdfParams) -> Result<SecretKey, AppError> {
    if kdf.algo != "argon2id" {
        return Err(vault_err(
            "VAULT_CRYPTO_005",
            format!("未知 KDF 算法 {}", kdf.algo),
        ));
    }
    let salt = unb64(&kdf.salt_b64)?;
    let params = Params::new(kdf.m_cost_kib, kdf.t_cost, kdf.p_cost, Some(KEY_LEN))
        .map_err(|e| vault_err("VAULT_CRYPTO_006", format!("KDF 参数非法: {e}")))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut okm = [0u8; KEY_LEN];
    argon
        .hash_password_into(password.as_bytes(), &salt, &mut okm)
        .map_err(|e| vault_err("VAULT_CRYPTO_001", format!("Argon2id 派生失败: {e}")))?;
    Ok(SecretKey::new(okm))
}

/// AES-256-GCM 原语：随机 nonce 加密，返回 (nonce, ct)
fn seal_raw(
    key: &SecretKey,
    plaintext: &[u8],
    aad: &[u8],
) -> Result<([u8; NONCE_LEN], Vec<u8>), AppError> {
    let cipher = Aes256Gcm::new(aes_gcm::Key::<Aes256Gcm>::from_slice(key.expose()));
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            aes_gcm::aead::Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| vault_err("VAULT_CRYPTO_002", "字段加密失败"))?;
    Ok((nonce, ct))
}

/// AES-256-GCM 解密（tag 校验失败 = 密钥错/密文被篡改）
fn open_raw(
    key: &SecretKey,
    nonce: &[u8; NONCE_LEN],
    ct: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, AppError> {
    let cipher = Aes256Gcm::new(aes_gcm::Key::<Aes256Gcm>::from_slice(key.expose()));
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            aes_gcm::aead::Payload { msg: ct, aad },
        )
        .map_err(|_| vault_err("VAULT_UNLOCK_001", "解密失败：密码错误或数据被篡改"))
}

/// 新建保险库：随机 DEK + 随机盐 + 默认 KDF 参数；返回（头部， DEK）
pub fn create_vault(master_password: &str) -> Result<(VaultHeader, SecretKey), AppError> {
    create_vault_with(
        master_password,
        KdfParams {
            algo: "argon2id".into(),
            m_cost_kib: DEFAULT_M_COST_KIB,
            t_cost: DEFAULT_T_COST,
            p_cost: DEFAULT_P_COST,
            salt_b64: String::new(),
        },
    )
}

/// 指定 KDF 参数的新建（测试/未来策略调整入口）；salt_b64 留空则随机
pub fn create_vault_with(
    master_password: &str,
    mut kdf: KdfParams,
) -> Result<(VaultHeader, SecretKey), AppError> {
    if kdf.salt_b64.is_empty() {
        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        kdf.salt_b64 = b64(&salt);
    }
    let vault_id = uuid::Uuid::now_v7().to_string();
    let kek = derive_kek(master_password, &kdf)?;
    let dek = SecretKey::generate();
    let (nonce, ct) = seal_raw(&kek, dek.expose(), vault_id.as_bytes())?;
    let header = VaultHeader {
        version: VaultHeader::current_version(),
        vault_id,
        kdf,
        wrapped_dek: WrappedKey {
            nonce_b64: b64(&nonce),
            ct_b64: b64(&ct),
        },
        hello: None,
    };
    Ok((header, dek))
}

/// 用主密码解开信封取回 DEK；AEAD 校验失败即密码错误（不泄露差异信息）
pub fn unwrap_dek(header: &VaultHeader, master_password: &str) -> Result<SecretKey, AppError> {
    let kek = derive_kek(master_password, &header.kdf)?;
    let nonce_vec = unb64(&header.wrapped_dek.nonce_b64)?;
    let ct = unb64(&header.wrapped_dek.ct_b64)?;
    let nonce: [u8; NONCE_LEN] = nonce_vec
        .try_into()
        .map_err(|_| vault_err("VAULT_CRYPTO_007", "信封 nonce 长度非法"))?;
    let dek = open_raw(&kek, &nonce, &ct, header.vault_id.as_bytes())?;
    let dek: [u8; KEY_LEN] = dek
        .try_into()
        .map_err(|_| vault_err("VAULT_CRYPTO_008", "DEK 长度非法"))?;
    Ok(SecretKey::new(dek))
}

/// 改主密码：旧密码验证 → 新盐新 KEK 重包同一 DEK（数据条目零改动）
pub fn change_master_password(
    header: &VaultHeader,
    old_password: &str,
    new_password: &str,
) -> Result<VaultHeader, AppError> {
    let dek = unwrap_dek(header, old_password)?;
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let mut kdf = header.kdf.clone();
    kdf.salt_b64 = b64(&salt);
    let kek = derive_kek(new_password, &kdf)?;
    let (nonce, ct) = seal_raw(&kek, dek.expose(), header.vault_id.as_bytes())?;
    Ok(VaultHeader {
        version: header.version,
        vault_id: header.vault_id.clone(),
        kdf,
        wrapped_dek: WrappedKey {
            nonce_b64: b64(&nonce),
            ct_b64: b64(&ct),
        },
        // DEK 不变 → Hello 信封（DPAPI 包裹的 DEK 副本 + DEK 自证）原样保留
        hello: header.hello.clone(),
    })
}

// ---------------------------------------------------------------------------
// 字段加密（V2 条目 fields / totp_secret 共用）
// ---------------------------------------------------------------------------

/// 加密字段值：b64(nonce || ct)。aad 绑定条目 id，防止密文在条目间换位。
pub fn seal_field(dek: &SecretKey, entry_id: &str, plaintext: &[u8]) -> Result<String, AppError> {
    let (nonce, ct) = seal_raw(dek, plaintext, entry_id.as_bytes())?;
    let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ct);
    Ok(b64(&blob))
}

/// 解密字段值
pub fn open_field(dek: &SecretKey, entry_id: &str, blob_b64: &str) -> Result<Vec<u8>, AppError> {
    let blob = unb64(blob_b64)?;
    if blob.len() < NONCE_LEN {
        return Err(vault_err("VAULT_CRYPTO_009", "字段密文长度非法"));
    }
    let (nonce, ct) = blob.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().expect("nonce 长度已校验");
    open_raw(dek, &nonce, ct, entry_id.as_bytes())
}

// ---------------------------------------------------------------------------
// 测试：低 KDF 参数（dev profile 无优化下 Argon2 64MiB 太慢，正式参数仅冒烟一次）
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) fn test_kdf() -> KdfParams {
    KdfParams {
        algo: "argon2id".into(),
        m_cost_kib: 8 * 1024,
        t_cost: 1,
        p_cost: 1,
        salt_b64: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrip_and_wrong_password() {
        let (header, dek_direct) = create_vault_with("correct horse", test_kdf()).unwrap();
        let dek = unwrap_dek(&header, "correct horse").unwrap();
        assert_eq!(dek.expose(), dek_direct.expose(), "解出的 DEK 与创建时一致");
        assert!(unwrap_dek(&header, "wrong").is_err(), "错误密码必须失败");
    }

    #[test]
    fn change_password_rewraps_same_dek() {
        let (header, dek0) = create_vault_with("old-pass", test_kdf()).unwrap();
        let header2 = change_master_password(&header, "old-pass", "new-pass").unwrap();
        assert_ne!(header2.kdf.salt_b64, header.kdf.salt_b64, "改密码必须换盐");
        let dek = unwrap_dek(&header2, "new-pass").unwrap();
        assert_eq!(dek.expose(), dek0.expose(), "DEK 不变（只重包）");
        assert!(unwrap_dek(&header2, "old-pass").is_err(), "旧密码失效");
    }

    #[test]
    fn field_seal_open_and_aad_binding() {
        let (_, dek) = create_vault_with("pw", test_kdf()).unwrap();
        let blob = seal_field(&dek, "entry-1", b"secret-value").unwrap();
        assert_eq!(open_field(&dek, "entry-1", &blob).unwrap(), b"secret-value");
        // aad 绑定：换条目 id 解密必须失败（防密文换位）
        assert!(open_field(&dek, "entry-2", &blob).is_err());
    }

    #[test]
    fn secret_key_wipe_zeroes_buffer() {
        // 阶段二验收：锁定后内存密钥 zeroize（断言缓冲区全 0）
        let mut k = SecretKey::new([0xAB; KEY_LEN]);
        assert_eq!(k.expose(), &[0xABu8; KEY_LEN]);
        k.wipe();
        assert_eq!(k.expose(), &[0u8; KEY_LEN], "wipe 后必须全 0");
    }

    #[test]
    fn secret_key_lock_address_is_stable_across_moves() {
        // COR-03：VirtualLock 锁的是地址——内联数组移动即复制会使锁失效。
        // Box 化后移动只搬指针，堆地址必须不变。
        let k = SecretKey::new([7u8; KEY_LEN]);
        let p1 = k.expose().as_ptr() as usize;
        let moved = k; // 移动
        assert_eq!(
            p1,
            moved.expose().as_ptr() as usize,
            "移动后堆地址必须不变（否则 VirtualLock 失效）"
        );
    }

    #[test]
    fn tampered_ciphertext_rejected() {
        let (_, dek) = create_vault_with("pw", test_kdf()).unwrap();
        let mut blob = seal_field(&dek, "e", b"data").unwrap();
        // 篡改密文末字节（base64 尾部字符翻转）
        let last = blob.pop().unwrap();
        blob.push(if last == 'A' { 'B' } else { 'A' });
        assert!(open_field(&dek, "e", &blob).is_err(), "篡改必须被 tag 拒绝");
    }

    #[test]
    fn hello_verifier_binds_dek_and_vault() {
        // D-24 验收②：verifier 必须同时绑定 DEK 与 vault_id，否则信封可跨库重放
        let (header, dek) = create_vault_with("pw", test_kdf()).unwrap();
        let v = seal_hello_verifier(&dek, &header.vault_id).unwrap();
        open_hello_verifier(&dek, &header.vault_id, &v).unwrap();
        let other = SecretKey::generate();
        assert!(
            open_hello_verifier(&other, &header.vault_id, &v).is_err(),
            "异 DEK 必须解不开（跨库重放防御）"
        );
        assert!(
            open_hello_verifier(&dek, "another-vault-id", &v).is_err(),
            "aad 绑定：换 vault_id 必须失败"
        );
        let mut tampered = v.clone();
        tampered.ct_b64.pop().unwrap();
        assert!(
            open_hello_verifier(&dek, &header.vault_id, &tampered).is_err(),
            "截断密文必须被拒"
        );
    }

    #[test]
    fn change_password_preserves_hello_envelope() {
        // D-24：改主密码只重包 KEK 下的 DEK；DEK 不变 → Hello 信封（含 verifier）零改动
        let (mut header, dek) = create_vault_with("old-pass", test_kdf()).unwrap();
        header.hello = Some(HelloEnvelope {
            wrapped_dek_b64: "deadbeef".into(),
            verifier: seal_hello_verifier(&dek, &header.vault_id).unwrap(),
        });
        let before = header.hello.clone();
        let after = change_master_password(&header, "old-pass", "new-pass").unwrap();
        assert_eq!(
            after.hello.as_ref().unwrap().wrapped_dek_b64,
            before.unwrap().wrapped_dek_b64
        );
        assert!(
            after.hello.as_ref().unwrap().verifier.ct_b64
                == header.hello.as_ref().unwrap().verifier.ct_b64,
            "verifier 不随改密码变化（DEK 未变）"
        );
    }
}
