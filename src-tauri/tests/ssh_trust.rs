//! B7 T-B7-1（09 §7.2）：SSH 主机信任**单一事实源**跨域端到端正证。
//! 两表互不共享的 B6 债务（09 §6.3）在此清偿——term 写入即 file 所见。

use std::path::PathBuf;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nf_ssh_trust_e2e_{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-1）字面测试名优先于 rustc 命名惯例
fn fileAndTerm_shareSingleStore() {
    let d = tmpdir("share");
    // ① 路径恒一：两域薄门面都必须委托 host-core 同一构造函数
    assert_eq!(
        term_core::ssh::KnownHosts::shared_path(&d),
        file_core::shared_known_hosts_path(&d),
        "单一事实源路径在两域门面必须恒一"
    );
    // ② term 明示 accept → file 域查同键得 Trusted（整键逐字）
    let kh = term_core::ssh::KnownHosts::open(&d).unwrap();
    let whole = "ssh-ed25519 SHA256:SHAREDasinglestore";
    kh.accept("shared.example", 2222, whole).unwrap();
    let dec = file_core::check_shared_host_key(&d, "shared.example", 2222, whole).unwrap();
    assert!(
        matches!(&dec, file_core::HostKeyDecision::Trusted { fingerprint } if fingerprint == whole),
        "term 写入的信任必须被 file 直接读到，实得 {dec:?}"
    );
    // ③ 同一枚键换算法名（整键口径）：file 侧同样落 Changed——两域同一比较口径
    let dec = file_core::check_shared_host_key(
        &d,
        "shared.example",
        2222,
        "ssh-rsa SHA256:SHAREDasinglestore",
    )
    .unwrap();
    assert!(
        matches!(&dec, file_core::HostKeyDecision::Changed { .. }),
        "整键口径必须两域一致，实得 {dec:?}"
    );
    // ④ 正对照防空洞：两域都不认识的主机在 file 侧裁 Unknown（不是读空回落）
    let dec =
        file_core::check_shared_host_key(&d, "never.seen", 22, "ssh-ed25519 SHA256:x").unwrap();
    assert!(matches!(&dec, file_core::HostKeyDecision::Unknown { .. }));
    let _ = std::fs::remove_dir_all(&d);
}
