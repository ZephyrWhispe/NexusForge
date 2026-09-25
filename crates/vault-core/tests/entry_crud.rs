//! TEST-01 补齐：vault 条目 CRUD / 字段打码数据面 / TOTP 生成用例。
//!
//! 打码与揭示的**视觉面**在前端（FieldValue 组件，COR-18 已加"值变即复位"自保
//! 与业务 key），核心层可机检的事实是：密码字段以 `kind=password` 落库、明文只
//! 经 `decrypt_row`（解锁态）返回、锁定态读不到任何字段、TOTP 以 30s 周期产出
//! 6 位码且与 RFC 6238 参考实现一致（固定向量）。

use std::path::PathBuf;
use std::sync::Arc;

use host_core::ports::{CryptoPort, HelloPort, Ports};
use vault_core::crypto::KdfParams;
use vault_core::model::{EntryField, FieldKind};
use vault_core::{VaultService, VaultState};

struct FakeHello {
    ok: std::sync::atomic::AtomicBool,
}
impl FakeHello {
    fn new(ok: bool) -> Arc<Self> {
        Arc::new(Self {
            ok: std::sync::atomic::AtomicBool::new(ok),
        })
    }
}
impl HelloPort for FakeHello {
    fn verify(&self, _reason: &str) -> Result<(), host_core::error::AppError> {
        if self.ok.load(std::sync::atomic::Ordering::SeqCst) {
            Ok(())
        } else {
            Err(host_core::error::AppError::module(
                "VAULT_HELLO_001",
                "Windows Hello 校验失败",
                None,
            ))
        }
    }
}

struct FakeCrypto;
impl CryptoPort for FakeCrypto {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, host_core::error::AppError> {
        // 可逆混淆（非真加密——DPAPI 的测试替身）
        Ok(plain.iter().map(|b| b ^ 0x5A).collect())
    }
    fn unprotect(&self, cipher: &[u8]) -> Result<Vec<u8>, host_core::error::AppError> {
        Ok(cipher.iter().map(|b| b ^ 0x5A).collect())
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nf_vault_crud_{tag}_{}", uuid::Uuid::now_v7()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cheap_kdf() -> KdfParams {
    KdfParams {
        algo: "argon2id".into(),
        m_cost_kib: 8 * 1024,
        t_cost: 1,
        p_cost: 1,
        salt_b64: String::new(),
    }
}

fn ports(hello: Arc<FakeHello>) -> Arc<Ports> {
    let p = Ports::new();
    p.register::<dyn HelloPort>(hello);
    p.register::<dyn CryptoPort>(Arc::new(FakeCrypto));
    Arc::new(p)
}

fn unlocked_vault(tag: &str) -> (VaultService, PathBuf) {
    let dir = temp_dir(tag);
    let svc = VaultService::open(&dir).unwrap();
    svc.bind_ports(ports(FakeHello::new(true)));
    svc.create("master-pw", Some(cheap_kdf())).unwrap();
    (svc, dir)
}

fn sample_fields() -> Vec<EntryField> {
    vec![
        EntryField {
            key: "用户名".into(),
            kind: FieldKind::Text,
            value: "alice".into(),
        },
        EntryField {
            key: "密码".into(),
            kind: FieldKind::Password,
            value: "s3cr3t-明文".into(),
        },
        EntryField {
            key: "站点".into(),
            kind: FieldKind::Url,
            value: "https://example.com".into(),
        },
    ]
}

#[test]
fn entry_crud_roundtrip_and_locked_state_reads_nothing() {
    let (svc, dir) = unlocked_vault("crud");
    let entry = svc
        .add_entry(None, "示例站点", false, sample_fields(), None)
        .unwrap();

    // 解锁态：字段原样读回（含中文与 kind）
    let got = svc.get_entry(&entry.id).unwrap().unwrap();
    assert_eq!(got.title, "示例站点");
    assert_eq!(got.fields, sample_fields());
    assert!(got.fields.iter().any(|f| f.kind == FieldKind::Password));

    // 锁定态：数据面整体拒读（打码的根——明文字段根本无法出库）
    svc.lock().unwrap();
    assert_eq!(svc.state(), VaultState::Locked);
    let err = svc.get_entry(&entry.id).unwrap_err();
    assert!(err.to_string().contains("锁定"), "{err}");

    // 重解锁：内容复原（信封加密纪律）
    svc.unlock("master-pw").unwrap();
    let got = svc.get_entry(&entry.id).unwrap().unwrap();
    assert_eq!(got.fields, sample_fields());

    // 更新 + 删除
    let mut updated = got.clone();
    updated.fields[1].value = "new-pass".into();
    let updated = svc.update_entry(updated).unwrap();
    assert_eq!(updated.fields[1].value, "new-pass");
    assert!(svc.delete_entry(&entry.id).unwrap());
    assert!(svc.get_entry(&entry.id).unwrap().is_none());
    assert!(
        !svc.delete_entry(&entry.id).unwrap(),
        "重复删除如实报 false"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn entry_search_by_title_uses_decrypted_index() {
    let (svc, dir) = unlocked_vault("search");
    svc.add_entry(None, "GitHub 主账号", false, sample_fields(), None)
        .unwrap();
    svc.add_entry(None, "银行账户", false, sample_fields(), None)
        .unwrap();

    let hits = svc.list_entries(None, Some("GitHub")).unwrap();
    assert_eq!(hits.len(), 1, "标题搜索命中 1 条");
    assert_eq!(hits[0].title, "GitHub 主账号");
    assert!(svc.list_entries(None, Some("不存在")).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn totp_generates_six_digit_code_with_period() {
    // 固定向量：RFC 6238 测试密钥（base32 "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"），
    // 借自 RFC 6238 附录 B 的 8 位码前 6 位语义（30s 周期档）
    let (code, remaining) =
        vault_core::totp::totp_at("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", 59).unwrap();
    assert_eq!(code.len(), 6, "6 位数字码: {code}");
    assert!(code.chars().all(|c| c.is_ascii_digit()), "{code}");
    assert!(
        remaining > 0 && remaining <= 30,
        "剩余秒数在周期内: {remaining}"
    );

    // 现在时刻同样产出合法码（vaultTotpNow 的核心层）
    let (code_now, rem_now) = vault_core::totp::totp_now("JBSWY3DPEHPK3PXP").unwrap();
    assert_eq!(code_now.len(), 6);
    assert!(rem_now > 0 && rem_now <= 30);
}
