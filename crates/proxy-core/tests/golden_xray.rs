//! T-B2-5 golden 回归（09 §5.2）：IR→xray 渲染输出逐字节锁死。
//! 夹具 `tests/golden/xray_rule_mixed.json` 由一次性导出测（同 T-B2-1 纪律，
//! 用后即删）对固定 4 节点输入生成并评审入库：http 7890 + socks 7891 双入站、
//! 兜底出口钉 outbounds[0]（首节点 hk-1）、domainStrategy=AsIs（核证修正，
//! 任务书字面 AsOrigin 为不存在的值）、ip_is_private 显式 CIDR 展开（零资产
//! 依赖，geoip.dat 归 T-B2-10）、selector/urltest 零漏出。
//! xray 无"重构前"（本方言为新建），锁的是评审后的第一版正确输出。
#![allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例

use proxy_core::ir::{self, IrRule};
use proxy_core::sub::{Node, NodeKind};

fn golden_nodes() -> Vec<Node> {
    vec![
        Node {
            tag: "hk-1".into(),
            kind: NodeKind::Shadowsocks,
            server: "1.2.3.4".into(),
            port: 8388,
            sub_id: "sub0aaaabbbbcccc".into(),
            groups: Vec::new(),
            extra: serde_json::json!({ "method": "aes-256-gcm", "password": "p1" }),
        },
        Node {
            tag: "tokyo-2".into(),
            kind: NodeKind::Trojan,
            server: "example.com".into(),
            port: 443,
            sub_id: "sub0aaaabbbbcccc".into(),
            groups: Vec::new(),
            extra: serde_json::json!({ "password": "p2", "sni": "example.com", "tls": true }),
        },
        Node {
            tag: "lg-vless".into(),
            kind: NodeKind::Vless,
            server: "5.6.7.8".into(),
            port: 8443,
            sub_id: "sub1bbbbccccdddd".into(),
            groups: Vec::new(),
            extra: serde_json::json!({
                "uuid": "c0ee752e-de1a-4fde-a6bf-0f04b1c9d6e3",
                "tls": true,
                "sni": "cdn.example.net",
                "network": "ws",
                "path": "/rain",
                "flow": "xtls-rprx-vision",
            }),
        },
        Node {
            tag: "fr-vmess".into(),
            kind: NodeKind::Vmess,
            server: "9.9.9.9".into(),
            port: 2053,
            sub_id: "sub1bbbbccccdddd".into(),
            groups: Vec::new(),
            extra: serde_json::json!({
                "uuid": "8a4c2b1e-3f60-4a2d-9e5b-7c1d0f2a3b4c",
                "security": "auto",
                "alter_id": 0,
                "network": "ws",
            }),
        },
    ]
}

#[test]
fn xrayGolden_mixedByteEqual() {
    let nodes = golden_nodes();
    let domains = vec![
        ".corp.example.com".to_string(),
        "internal.local".to_string(),
    ];
    let ir = ir::build(
        7890,
        false,
        &nodes,
        &[] as &[IrRule],
        ir::TAG_PROXY,
        &domains,
    )
    .unwrap();
    let want = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join("xray_rule_mixed.json"),
    )
    .expect("夹具 tests/golden/xray_rule_mixed.json 必须存在");
    let got = serde_json::to_string_pretty(&proxy_core::xray::render(&ir).unwrap()).unwrap();
    assert_eq!(got, want, "xray 渲染输出与评审入库的夹具不再逐字节等价");
}
