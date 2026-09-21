//! T-B2-6 golden 回归（09 §5.2 `mihomoGolden_yamlByteEqual`）：IR→mihomo
//! config.yaml 渲染输出逐字节锁死。夹具 `tests/golden/mihomo_rule_mixed.yaml`
//! 由一次性导出测（同 T-B2-1/5 纪律，用后即删）对与 sing-box/xray golden
//! 完全同款的固定 4 节点输入生成并评审入库：mixed-port 7890 + 回环
//! bind-address、external-controller ""（地基禁用）、tun 块常驻 enable:false、
//! 每 proxy 显式 udp:true（对齐 sing-box mixed 入站 UDP 语义）、
//! AUTO url-test（gstatic/300s 消费 IR 真源）+ NexusForge select（AUTO 恒首项）、
//! ip_is_private 显式 CIDR 展开（v6 走 IP-CIDR6，零 geo 资产）、
//! 兜底 MATCH,NexusForge（policy_groups=true：指向组而非决议单点）。
//! 锁的是评审后的第一版正确输出。
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
            extra: serde_json::json!({ "method": "aes-256-gcm", "password": "p1" }),
        },
        Node {
            tag: "tokyo-2".into(),
            kind: NodeKind::Trojan,
            server: "example.com".into(),
            port: 443,
            sub_id: "sub0aaaabbbbcccc".into(),
            extra: serde_json::json!({ "password": "p2", "sni": "example.com", "tls": true }),
        },
        Node {
            tag: "lg-vless".into(),
            kind: NodeKind::Vless,
            server: "5.6.7.8".into(),
            port: 8443,
            sub_id: "sub1bbbbccccdddd".into(),
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
fn mihomoGolden_yamlByteEqual() {
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
            .join("mihomo_rule_mixed.yaml"),
    )
    .expect("夹具 tests/golden/mihomo_rule_mixed.yaml 必须存在");
    let got = proxy_core::mihomo::render(&ir).unwrap();
    assert_eq!(got, want, "mihomo 渲染输出与评审入库的夹具不再逐字节等价");
}
