//! WinOps e2e 验收（docs/impl/08 W7 步骤表：HKCU 沙箱键全链路）。
//!
//! 用真实 RegistryOpsWin（非 mock）跑 BAVR 全链路：scan → apply → verify →
//! 回归检测（自愈模拟）→ rollback → scan 复原；附审计 roundtrip。
//! 沙箱键 = HKCU\Software\NexusForgeWinOpsE2E\{pid}（测试并行隔离）。

use sys_core::winops::{
    self, load_catalog, scan, ApplyReport, AuditStore, BackupStore, BackupItem, RegistryBackup, ScanState, SysPorts,
    Tweak, TweakAction,
};

/// 真实注册表数据面（HKCU 沙箱键；Service/Task/Maintenance/Appx 端口缺省 None）
fn real_ports(reg: &win_integration::registry::RegistryOpsWin) -> SysPorts<'_> {
    SysPorts { registry: reg, tasks: None, services: None, maintenance: None, appx: None }
}

fn sandbox_key() -> String {
    format!(r"HKCU\Software\NexusForgeWinOpsE2E\{}", std::process::id())
}

/// 构造 HKCU registry 单值 tweak（e2e 专用，不进 catalog）
fn e2e_tweak(key: &str, val: u32) -> Tweak {
    serde_json::from_value(serde_json::json!({
        "id": "e2e_sandbox",
        "name": "e2e 沙箱",
        "category": "test",
        "actions": [{ "type": "registry", "key": key, "value_name": "v", "value_type": "dword", "data": { "dword": val } }]
    }))
    .unwrap()
}

#[test]
fn hksu_sandbox_full_chain() {
    let key = sandbox_key();
    let reg = win_integration::registry::RegistryOpsWin::new();
    let ports = real_ports(&reg);
    let dir = std::env::temp_dir().join(format!("nf_winops_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    // 预置原值 0（existed=true 备份路径）
    ports.registry.write_value(&key, "v", &host_core::ports::RegValue::Dword(0)).unwrap();

    // ---- scan：目标 1 未应用 ----
    let t = e2e_tweak(&key, 1);
    let tweaks = vec![t.clone()];
    let states = scan(&ports, &tweaks, true);
    assert_eq!(states[0].1, ScanState::NotApplied, "初始应为未应用");

    // ---- apply：BAVR 通过，值变 1 ----
    let report: ApplyReport = winops::apply(&ports, &t, true).unwrap();
    assert!(report.verified);
    let (v, existed) = ports.registry.read_value(&key, "v").unwrap();
    assert!(existed && v == host_core::ports::RegValue::Dword(1));

    // 备份落库 + 审计 apply
    let store = BackupStore::open(&dir);
    store.save(&report).unwrap();
    let audit = AuditStore::open(&dir);
    audit.record("ui", "e2e_sandbox", "apply", Some(true), "");

    // ---- scan：已应用 ----
    let states = scan(&ports, &tweaks, true);
    assert_eq!(states[0].1, ScanState::Applied);

    // ---- 回归检测：目标态 → 无回归；自愈回 0 → 检出 ----
    assert!(winops::regression_check(&ports, &store, &tweaks).is_empty());
    ports.registry.write_value(&key, "v", &host_core::ports::RegValue::Dword(0)).unwrap();
    assert_eq!(winops::regression_check(&ports, &store, &tweaks), vec!["e2e_sandbox".to_string()]);
    // 重新 apply 修复回归
    winops::apply(&ports, &t, true).unwrap();
    assert!(winops::regression_check(&ports, &store, &tweaks).is_empty());

    // ---- rollback：恢复原值 0，scan 回 NotApplied ----
    let backup = store.take("e2e_sandbox");
    assert!(!backup.is_empty());
    match &backup[0] {
        BackupItem::Registry(RegistryBackup { existed, old_value, .. }) => {
            assert!(existed, "原值 0 预置存在");
            assert_eq!(old_value, &Some(host_core::ports::RegValue::Dword(0)));
        }
        other => panic!("应为 Registry 备份: {other:?}"),
    }
    winops::restore_backup(&ports, &backup).unwrap();
    store.remove("e2e_sandbox");
    audit.record("ui", "e2e_sandbox", "rollback", None, "");
    let states = scan(&ports, &tweaks, true);
    assert_eq!(states[0].1, ScanState::NotApplied, "回滚后应复原为未应用");
    let (v, existed) = ports.registry.read_value(&key, "v").unwrap();
    assert!(existed && v == host_core::ports::RegValue::Dword(0));

    // ---- 审计 roundtrip + 导出 ----
    let entries = audit.entries();
    assert_eq!(entries.len(), 2, "apply + rollback 两条");
    assert_eq!(entries[0].action, "apply");
    assert_eq!(entries[1].action, "rollback");
    let exported = audit.export(&dir).unwrap();
    assert!(exported.exists(), "导出文件存在");
    let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&exported).unwrap()).unwrap();
    assert_eq!(doc["audit"].as_array().unwrap().len(), 2);

    // ---- 清理沙箱键 ----
    ports.registry.delete_value(&key, "v").unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 嵌入 catalog 含 W7 Defender 条目（高风险族入库且全 requires_admin）
#[test]
fn catalog_contains_defender_family() {
    let tweaks = load_catalog(None).unwrap();
    let off = tweaks.iter().find(|t| t.id == "defender_realtime_off").expect("defender_realtime_off 应在目录");
    assert!(off.requires_admin && off.maintenance);
    assert!(matches!(off.actions[0], TweakAction::DefenderRealtime { disable: true }));
    let on = tweaks.iter().find(|t| t.id == "defender_realtime_on").expect("defender_realtime_on 应在目录");
    assert!(matches!(on.actions[0], TweakAction::DefenderRealtime { disable: false }));
}
