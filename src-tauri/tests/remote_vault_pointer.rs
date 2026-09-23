//! B6 T-B6-5（09 §6.2）：VaultEntry 指针的端口缝合面——三态归因在**真
//! VaultService** 上跑（file-core 不 import vault-core，装配臂 src-tauri 侧）。
//! 本文件同时是 T-B6-8 正式装配的形态预演：适配器只经 `get_entry` 取
//! `kind==password` 字段值，包进 `Zeroizing` 即退役——密文不过第二条边。

use std::path::PathBuf;
use std::sync::Arc;

use file_core::{vault_secret_via_ports, FileError, VaultSecretPort};
use host_core::ports::Ports;
use vault_core::model::{EntryField, FieldKind};
use vault_core::vault::{VaultService, VaultState};
use zeroize::Zeroizing;

/// 生产形态的窄适配器（装配注册归 T-B6-8；本行以测试实例走通三态）
struct VaultSecretAdapter {
    svc: Arc<VaultService>,
}

impl VaultSecretPort for VaultSecretAdapter {
    fn read_secret(&self, entry_id: &str) -> Result<Zeroizing<String>, FileError> {
        match self.svc.state() {
            VaultState::Unlocked => {}
            // 未解锁态：点名状态本身，禁回落成"提示输口令"（口令不向协议腿伸手）
            VaultState::Locked => {
                return Err(FileError::BadState(format!(
                    "vault 处于未解锁状态：条目 {entry_id} 无法经指针解析（未解锁态——与缺端口/条目不存在两态分列）"
                )))
            }
            VaultState::Uninitialized => {
                return Err(FileError::BadState(format!(
                    "vault 尚未初始化：条目 {entry_id} 无法经指针解析（未初始化态）"
                )))
            }
        }
        let entry = self
            .svc
            .get_entry(entry_id)
            .map_err(|e| FileError::BadState(format!("vault 读取失败: {e}")))?
            .ok_or_else(|| {
                FileError::NotFound(format!(
                    "vault 条目 {entry_id} 不存在（条目缺失态——与未解锁/缺端口两态分列）"
                ))
            })?;
        entry
            .fields
            .iter()
            .find(|f| f.kind == FieldKind::Password)
            .map(|f| Zeroizing::new(f.value.clone()))
            .ok_or_else(|| FileError::NotFound(format!("vault 条目 {entry_id} 没有 password 字段")))
    }
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nf_vault_pointer_{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-5）字面测试名优先于 rustc 命名惯例
fn vaultEntryPointer_resolvesViaPortsAndErrsNamedByState() {
    let d = tmpdir("triage");
    let svc = Arc::new(VaultService::open(&d).unwrap());
    svc.create("master-Pw-9527", None).unwrap(); // create 即解锁
    let entry = svc
        .add_entry(
            None,
            "SFTP 口令",
            false,
            vec![EntryField {
                key: "password".into(),
                kind: FieldKind::Password,
                value: "vault-SESAME-42".into(),
            }],
            None,
        )
        .unwrap();

    // 成功态：指针 → 端口 → 字段值逐字
    let ports = Ports::new();
    ports.register::<dyn VaultSecretPort>(Arc::new(VaultSecretAdapter { svc: svc.clone() }));
    let got = vault_secret_via_ports(&ports, &entry.id).unwrap();
    assert_eq!(&*got, "vault-SESAME-42");

    // 条目不存在态
    let missing = vault_secret_via_ports(&ports, "ghost-entry-404")
        .unwrap_err()
        .to_string();
    assert!(
        missing.contains("ghost-entry-404") && missing.contains("不存在"),
        "须点名条目并归因缺失，实得 {missing}"
    );

    // 未解锁态：同目录新实例（open 后即 Locked）
    let svc2 = Arc::new(VaultService::open(&d).unwrap());
    assert_eq!(svc2.state(), VaultState::Locked, "夹具前提：重开即锁");
    let ports2 = Ports::new();
    ports2.register::<dyn VaultSecretPort>(Arc::new(VaultSecretAdapter { svc: svc2 }));
    let locked = vault_secret_via_ports(&ports2, &entry.id)
        .unwrap_err()
        .to_string();
    assert!(locked.contains("未解锁"), "实得 {locked}");

    // 缺端口态：空注册表
    let noport = vault_secret_via_ports(&Ports::new(), &entry.id)
        .unwrap_err()
        .to_string();
    assert!(
        noport.contains("VaultSecretPort"),
        "须点名缺的端口，实得 {noport}"
    );

    // 三态两两不同串 + 任何一态都不回落"提示输口令"（承重⑪ 裁决面）
    assert_ne!(locked, missing);
    assert_ne!(locked, noport);
    assert_ne!(missing, noport);
    for m in [&locked, &missing, &noport] {
        assert!(
            !m.contains("请输入") && !m.contains("请输入口令"),
            "禁回落索取: {m}"
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}
