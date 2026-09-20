//! T-B2-1 golden 逐字节回归（09 §5.2）：IR→sing-box 渲染输出必须与**重构前**
//! `config::generate` 落盘的 pretty JSON 全等。夹具由重构前代码一次性导出生成
//! （to_string_pretty，与 service.rs 写 config.json 的 to_vec_pretty 同形，无尾换行）。
#![allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例

use proxy_core::ir::{self, IrRule};
use proxy_core::singbox;
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

fn golden_domains() -> Vec<String> {
    vec![
        ".corp.example.com".to_string(),
        "internal.local".to_string(),
    ]
}

fn assert_byte_equal(fixture: &str, ir: &ir::IrConfig) {
    let want = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(fixture),
    )
    .unwrap_or_else(|e| panic!("夹具 {fixture} 读取失败：{e}"));
    let got = serde_json::to_string_pretty(&singbox::render(ir)).unwrap();
    assert_eq!(got, want, "渲染输出与重构前夹具不再逐字节等价：{fixture}");
}

#[test]
fn irGolden_singboxRuleMixed_byteEqual() {
    let nodes = golden_nodes();
    let ir = ir::build(
        7890,
        false,
        &nodes,
        &[] as &[IrRule],
        ir::TAG_PROXY,
        &golden_domains(),
    )
    .unwrap();
    assert_byte_equal("singbox_rule_mixed.json", &ir);
}

#[test]
fn irGolden_singboxGlobalMixed_byteEqual() {
    let nodes = golden_nodes();
    let ir = ir::build(7890, false, &nodes, &[] as &[IrRule], ir::TAG_PROXY, &[]).unwrap();
    assert_byte_equal("singbox_global_mixed.json", &ir);
}

#[test]
fn irGolden_singboxTunByteEqual() {
    let nodes = golden_nodes();
    let ir = ir::build(
        7890,
        true,
        &nodes,
        &[] as &[IrRule],
        ir::TAG_PROXY,
        &golden_domains(),
    )
    .unwrap();
    assert_byte_equal("singbox_rule_tun.json", &ir);
}
