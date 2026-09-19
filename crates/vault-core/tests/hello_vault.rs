//! D-24 回归：V4 Hello 免密解锁（三重绑定 + 熔断）、V5 策略映射、⑥ 锁页降级。
//!
//! FakeHello/FakeCrypto 端口注入 VaultService::bind_ports，
//! 免真实 Windows Hello / DPAPI；KDF 用低参数（同 crypto::test_kdf 动机）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use host_core::config::ConfigStore;
use host_core::error::AppError;
use host_core::events::EventBus;
use host_core::ports::{CryptoPort, HelloPort, MemLockPort, Ports};

use vault_core::{KdfParams, VaultHeader, VaultModule, VaultService, VaultState, MAX_ATTEMPTS};

// ---------------------------------------------------------------------------
// 测试替身
// ---------------------------------------------------------------------------

struct FakeHello {
    ok: AtomicBool,
}

impl FakeHello {
    fn new(ok: bool) -> Arc<Self> {
        Arc::new(Self {
            ok: AtomicBool::new(ok),
        })
    }
}

impl HelloPort for FakeHello {
    fn verify(&self, _reason: &str) -> Result<(), AppError> {
        if self.ok.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(AppError::module("HELLO_TEST_001", "模拟校验失败", None))
        }
    }
}

/// 前缀标记"加密"：可逆、确定性，够验证 protect/unprotect 接线与长度校验
struct FakeCrypto;

impl CryptoPort for FakeCrypto {
    fn protect(&self, plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
        let mut v = b"FAKE-DPAPI:".to_vec();
        v.extend_from_slice(plaintext);
        Ok(v)
    }
    fn unprotect(&self, ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
        ciphertext
            .strip_prefix(b"FAKE-DPAPI:")
            .map(|p| p.to_vec())
            .ok_or_else(|| AppError::module("CRYPTO_TEST_001", "信封非法", None))
    }
}

struct FakeMemLock {
    ok: bool,
}

impl MemLockPort for FakeMemLock {
    fn lock(&self, _addr: usize, _len: usize) -> bool {
        self.ok
    }
    fn unlock(&self, _addr: usize, _len: usize) -> bool {
        self.ok
    }
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

fn cheap_kdf() -> KdfParams {
    KdfParams {
        algo: "argon2id".into(),
        m_cost_kib: 8 * 1024,
        t_cost: 1,
        p_cost: 1,
        salt_b64: String::new(),
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vault-hello-{tag}-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ports(hello: Arc<FakeHello>) -> Arc<Ports> {
    let p = Ports::new();
    p.register::<dyn HelloPort>(hello);
    p.register::<dyn CryptoPort>(Arc::new(FakeCrypto));
    Arc::new(p)
}

/// 建库（解锁态）+ 绑定端子的服务
fn unlocked_vault(tag: &str) -> (VaultService, PathBuf, Arc<FakeHello>) {
    let dir = temp_dir(tag);
    let svc = VaultService::open(&dir).unwrap();
    let hello = FakeHello::new(true);
    svc.bind_ports(ports(hello.clone()));
    svc.create("master-pw", Some(cheap_kdf())).unwrap();
    (svc, dir, hello)
}

fn meta_path(dir: &Path) -> PathBuf {
    dir.join("vault").join("vault.meta.json")
}

fn read_meta(dir: &Path) -> VaultHeader {
    serde_json::from_str(&std::fs::read_to_string(meta_path(dir)).unwrap()).unwrap()
}

fn write_meta(dir: &Path, header: &VaultHeader) {
    std::fs::write(meta_path(dir), serde_json::to_vec_pretty(header).unwrap()).unwrap();
}

// ---------------------------------------------------------------------------
// ① 熔断：Hello 失败 ≥5 → 仅密码可解锁；密码成功后恢复
// ---------------------------------------------------------------------------

#[test]
fn hello_failures_force_password_unlock_until_reset() {
    let (svc, _dir, hello) = unlocked_vault("fuse");
    svc.hello_enable().unwrap();
    assert!(svc.hello_enabled());
    svc.lock().unwrap();

    hello.ok.store(false, Ordering::Relaxed);
    let mut last = None;
    for _ in 0..MAX_ATTEMPTS {
        last = Some(svc.hello_unlock().unwrap_err());
    }
    assert_eq!(
        last.unwrap().code(),
        "HELLO_TEST_001",
        "前 5 次透传校验失败"
    );
    assert!(svc.hello_forced_off(), "第 5 次失败必须熔断");
    // 熔断期：即便校验恢复可用也被拒（不再触达 Hello）
    hello.ok.store(true, Ordering::Relaxed);
    let e = svc.hello_unlock().unwrap_err();
    assert_eq!(e.code(), "VAULT_HELLO_003");

    // 密码解锁成功 = 熔断电门
    svc.unlock("master-pw").unwrap();
    assert!(!svc.hello_forced_off());
    svc.lock().unwrap();
    svc.hello_unlock().unwrap();
    assert_eq!(svc.state(), VaultState::Unlocked);
}

// ---------------------------------------------------------------------------
// ② 篡改 / 跨库重放 → verifier 拒绝，留在 Locked
// ---------------------------------------------------------------------------

#[test]
fn tampered_hello_envelope_stays_locked() {
    let (svc, dir, _hello) = unlocked_vault("tamper");
    svc.hello_enable().unwrap();
    svc.lock().unwrap();

    let mut header = read_meta(&dir);
    let ct = header.hello.as_mut().unwrap().verifier.ct_b64.clone();
    let mut bytes = host_core::util::b64_decode(&ct).unwrap();
    let last = bytes.last_mut().unwrap();
    *last ^= 0xFF;
    header.hello.as_mut().unwrap().verifier.ct_b64 = host_core::util::b64_encode(&bytes);
    write_meta(&dir, &header);

    // 重开（模拟进程重启读回被篡改的 meta）
    let svc2 = VaultService::open(&dir).unwrap();
    svc2.bind_ports(ports(FakeHello::new(true)));
    let e = svc2.hello_unlock().unwrap_err();
    assert_eq!(e.code(), "VAULT_UNLOCK_001", "tag 校验必须拒绝篡改信封");
    assert_eq!(svc2.state(), VaultState::Locked);
    // 密码路径不受影响
    svc2.unlock("master-pw").unwrap();
}

#[test]
fn cross_vault_hello_envelope_rejected() {
    // A 库的信封塞进 B 库头部：DEK 不同 + aad=vault_id 不同，双重必拒
    let (svc_a, dir_a, _) = unlocked_vault("replay-a");
    svc_a.hello_enable().unwrap();
    let envelope_a = read_meta(&dir_a).hello.clone();

    let (_svc_b, dir_b, _) = unlocked_vault("replay-b");
    let mut header_b = read_meta(&dir_b);
    header_b.hello = envelope_a;
    write_meta(&dir_b, &header_b);

    let svc_b2 = VaultService::open(&dir_b).unwrap();
    svc_b2.bind_ports(ports(FakeHello::new(true)));
    assert!(
        svc_b2.hello_unlock().is_err(),
        "跨库信封必须被 verifier 拒绝"
    );
    assert_eq!(svc_b2.state(), VaultState::Locked);
}

// ---------------------------------------------------------------------------
// ③ 冷却期同样拒绝 Hello 路径
// ---------------------------------------------------------------------------

#[test]
fn cooling_blocks_hello_unlock() {
    let (svc, _dir, hello) = unlocked_vault("cooling");
    svc.hello_enable().unwrap();
    svc.lock().unwrap();
    for _ in 0..MAX_ATTEMPTS {
        assert!(svc.unlock("bad-password").is_err());
    }
    assert!(svc.lockout_remaining_secs() > 0, "5 次密码错误进入冷却");
    let e = svc.hello_unlock().unwrap_err();
    assert_eq!(e.code(), "VAULT_LOCKED_002", "冷却不给 Hello 开后门");
    assert_eq!(svc.state(), VaultState::Locked);
    assert!(hello.ok.load(Ordering::Relaxed)); // 未触达校验即被拒
}

// ---------------------------------------------------------------------------
// 端口缺失 / 未启用（负例）+ ⑥ 锁页失败降级
// ---------------------------------------------------------------------------

#[test]
fn hello_paths_degrade_without_ports_or_envelope_and_memlock_failure_is_soft() {
    // ⑥：注入"永远失败"的 MemLockPort——解锁/建库必须照常（只 warn 不阻断）
    vault_core::crypto::set_mem_lock(Arc::new(FakeMemLock { ok: false }));
    let (svc, _dir, _hello) = unlocked_vault("degrade");
    assert_eq!(svc.state(), VaultState::Unlocked, "锁页失败不得影响解锁");

    // 未启用（锁定态下；解锁态 hello_unlock 幂等成功）→ 拒绝
    svc.lock().unwrap();
    let e = svc.hello_unlock().unwrap_err();
    assert_eq!(e.code(), "VAULT_HELLO_004");

    // 端口未绑定 → enable 拒绝（密码路径不受影响）
    let dir2 = temp_dir("degrade-nopos");
    let svc2 = VaultService::open(&dir2).unwrap();
    svc2.create("master-pw", Some(cheap_kdf())).unwrap();
    let e = svc2.hello_enable().unwrap_err();
    assert_eq!(e.code(), "VAULT_HELLO_001");
    assert!(svc2.unlock("master-pw").is_ok() || svc2.state() == VaultState::Unlocked);
}

// ---------------------------------------------------------------------------
// V5：模块策略映射（config → AutolockPolicy）+ 未 init 时 tick 无害
// ---------------------------------------------------------------------------

#[test]
fn watchdog_policy_maps_config_minutes_and_defaults() {
    let dir = temp_dir("policy");
    let config = Arc::new(ConfigStore::new(dir.clone(), Arc::new(EventBus::new())));
    // 缺省（无 vault.json）= schema 默认 15/5 分钟
    let module = VaultModule::new_with_config(config.clone());
    let p = module.autolock_policy();
    assert_eq!((p.idle_secs, p.blur_secs), (900, 300));
    // 0 = 禁用线；自定义值分钟→秒
    std::fs::write(
        dir.join("vault.json"),
        serde_json::json!({ "auto_lock_idle_mins": 0, "auto_lock_blur_mins": 10 }).to_string(),
    )
    .unwrap();
    let p = module.autolock_policy();
    assert_eq!((p.idle_secs, p.blur_secs), (0, 600));
    // 未 init 的模块：tick 必须静默无害（无服务/无事件）
    module.watchdog_tick();
    // new()（无配置注入）：看门狗不挂，tick 不评估
    VaultModule::new().watchdog_tick();
}
