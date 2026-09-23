//! B6 T-B6-8（09 §6.2）：凭据解析装配面——三态归因在**真 VaultService** 上跑。
//! file-core 不 import vault-core；`resolve_auth` 住在 src-tauri（唯一同时看得见
//! 两者的层），解出的值只过协议腿一口。四态消息两两不同串、任何一态都不回落
//! "请输入口令"（承重⑪ 裁决面，承 `remote_vault_pointer.rs` 的预演形状）。

use std::path::PathBuf;

use file_core::{AuthKind, RemoteProfile, RemoteProtocol};
use nexusforge_lib::commands::resolve_auth;
use vault_core::model::{EntryField, FieldKind};
use vault_core::vault::{VaultService, VaultState};

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nf_resolve_auth_{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn profile_with(id: &str, auth: AuthKind) -> RemoteProfile {
    RemoteProfile {
        id: id.into(),
        label: "装配夹具站".into(),
        protocol: RemoteProtocol::Sftp,
        host: "sftp.example.invalid".into(),
        port: 22,
        user: "me".into(),
        base_path: "/srv".into(),
        auth,
        preset_id: None,
        last_used_ms: 0,
    }
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-8）字面测试名优先于 rustc 命名惯例
fn resolveAuth_vaultUnlocked_returnsEntrySecretAndNeverLogsIt() {
    let d = tmpdir("unlocked");
    let svc = VaultService::open(&d).unwrap();
    svc.create("master-Pw-9527", None).unwrap(); // create 即解锁
    let entry = svc
        .add_entry(
            None,
            "SFTP 口令",
            false,
            vec![EntryField {
                key: "password".into(),
                kind: FieldKind::Password,
                value: "vault-SESAME-88".into(),
            }],
            None,
        )
        .unwrap();
    let p = profile_with(
        "remote:asm",
        AuthKind::VaultEntry {
            entry_id: entry.id.clone(),
        },
    );
    let resolved = resolve_auth(&p, Some(&svc), None).unwrap();
    assert_eq!(resolved.user, "me");
    let pw = resolved
        .secret
        .as_ref()
        .expect("已解锁档案必须解出口令")
        .password
        .as_ref()
        .expect("password 字段值");
    assert_eq!(&**pw, "vault-SESAME-88", "指针→字段值逐字");
    // 错误文本/stdout 不含口令：Debug 面脱敏（值只在内存区活到协议腿）
    let dbg = format!("{resolved:?}");
    assert!(!dbg.contains("vault-SESAME-88"), "Debug 泄露口令: {dbg}");
    assert!(dbg.contains("redacted"), "正对照：Debug 报在场");
    // typed 不覆写指针：VaultEntry 档案即便被塞了逐次口令，解出的仍是 vault 值
    let typed = Some(file_core::AuthSecret {
        header: None,
        password: Some(zeroize::Zeroizing::new("typed-OVERRIDE-x".into())),
    });
    let r2 = resolve_auth(&p, Some(&svc), typed).unwrap();
    assert_eq!(&*r2.secret.unwrap().password.unwrap(), "vault-SESAME-88");
    // 正对照二：非指针臂 typed 直通（PromptEachTime），Anonymous 就地退役
    let q = profile_with("remote:asm2", AuthKind::PromptEachTime);
    let r3 = resolve_auth(
        &q,
        Some(&svc),
        Some(file_core::AuthSecret {
            header: None,
            password: Some(zeroize::Zeroizing::new("typed-PASS".into())),
        }),
    )
    .unwrap();
    assert_eq!(&*r3.secret.unwrap().password.unwrap(), "typed-PASS");
    let a = profile_with("remote:asm3", AuthKind::Anonymous);
    let r4 = resolve_auth(&a, None, None).unwrap();
    assert!(r4.secret.is_none(), "匿名档恒 None");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-8）字面测试名优先于 rustc 命名惯例
fn resolveAuth_lockedVault_errsNamingState() {
    let d = tmpdir("locked");
    {
        let svc = VaultService::open(&d).unwrap();
        svc.create("master-Pw-9527", None).unwrap();
    }
    // 重开即锁（夹具前提与 remote_vault_pointer.rs 同谱）
    let svc2 = VaultService::open(&d).unwrap();
    assert_eq!(svc2.state(), VaultState::Locked);
    let p = profile_with(
        "remote:asm",
        AuthKind::VaultEntry {
            entry_id: "0198f2c7-3a4e-7a10-9b6a-2f1c8d5e4b3a".into(),
        },
    );
    let e = resolve_auth(&p, Some(&svc2), None).unwrap_err().to_string();
    assert!(
        e.contains("未解锁") && e.contains("0198f2c7"),
        "未解锁态须点名状态与条目，实得 {e}"
    );
    assert!(!e.contains("请输入"), "禁回落索取口令: {e}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-8）字面测试名优先于 rustc 命名惯例
fn resolveAuth_missingEntry_errsNamingIdNotFallbackPrompt() {
    let d = tmpdir("missing");
    let svc = VaultService::open(&d).unwrap();
    svc.create("master-Pw-9527", None).unwrap();
    let ghost = profile_with(
        "remote:asm",
        AuthKind::VaultEntry {
            entry_id: "ghost-entry-404".into(),
        },
    );
    let missing = resolve_auth(&ghost, Some(&svc), None)
        .unwrap_err()
        .to_string();
    assert!(
        missing.contains("ghost-entry-404") && missing.contains("不存在"),
        "条目缺失须点名 id 并归因缺失，实得 {missing}"
    );
    // 悬空第四态：条目在但没有 password 字段——三态之外单列，同样不回落索取
    let note = svc
        .add_entry(
            None,
            "只有备注",
            false,
            vec![EntryField {
                key: "note".into(),
                kind: FieldKind::Note,
                value: "无口令".into(),
            }],
            None,
        )
        .unwrap();
    let dangling = profile_with(
        "remote:asm2",
        AuthKind::VaultEntry {
            entry_id: note.id.clone(),
        },
    );
    let nofield = resolve_auth(&dangling, Some(&svc), None)
        .unwrap_err()
        .to_string();
    assert!(nofield.contains("password 字段"), "实得 {nofield}");
    // 缺装配态 + 未解锁态消息（构造见上测）逐档分列：这里以四串两两不同收口
    let noport = resolve_auth(&ghost, None, None).unwrap_err().to_string();
    assert!(
        noport.contains("未装配"),
        "缺端口态须点名装配，实得 {noport}"
    );
    let locked = "vault 处于未解锁状态：条目 ghost-entry-404 无法经指针解析".to_string();
    let mut msgs = vec![missing, nofield, noport, locked];
    msgs.sort();
    msgs.dedup();
    assert_eq!(msgs.len(), 4, "四态消息两两不同串");
    for m in &msgs {
        assert!(!m.contains("请输入"), "任何一态都禁回落索取口令: {m}");
    }
    let _ = std::fs::remove_dir_all(&d);
}
