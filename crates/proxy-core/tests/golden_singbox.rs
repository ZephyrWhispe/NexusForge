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

fn golden_domains() -> Vec<String> {
    vec![
        ".corp.example.com".to_string(),
        "internal.local".to_string(),
    ]
}

/// T-B2-7 扩臂输入（行字面：夹具 singbox_rule_mixed 扩含新协议节点臂）：
/// hy2/tuic/wg 三渲染臂 + 一枚带 REALITY/XHTTP 透传键的 vless。
/// ssr 刻意不在内——sing-box 官方不支持（⑭），其负面在 singbox.rs 单元测钉。
fn golden_nodes_newkinds() -> Vec<Node> {
    vec![
        Node {
            tag: "hy-seoul".into(),
            kind: NodeKind::Hysteria2,
            server: "3.3.3.3".into(),
            port: 8443,
            sub_id: "sub0aaaabbbbcccc".into(),
            groups: Vec::new(),
            extra: serde_json::json!({
                "password": "hy-pw",
                "sni": "hy.example.com",
                "insecure": false,
                "obfs": "salamander",
                "obfs_password": "obfspw",
            }),
        },
        Node {
            tag: "tuic-oslo".into(),
            kind: NodeKind::Tuic5,
            server: "4.4.4.4".into(),
            port: 6000,
            sub_id: "sub1bbbbccccdddd".into(),
            groups: Vec::new(),
            extra: serde_json::json!({
                "uuid": "9c0dbc5a-1d9f-4f6d-b5e1-0e9b48f2a1cd",
                "password": "tuic-pw",
                "sni": "tuic.example.com",
                "alpn": "h3",
            }),
        },
        Node {
            tag: "wg-paris".into(),
            kind: NodeKind::WireGuard,
            server: "5.5.5.5".into(),
            port: 51820,
            sub_id: "sub2ccccddddeeee".into(),
            groups: Vec::new(),
            extra: serde_json::json!({
                "private_key": "YHPe0P3bQxUqUvVXrLp0lP1dT1mT0d0d0d0d0d0d0d0=",
                "public_key": "Iz1N0T9KZ3mM0lL9pP2oO8nN7bB6vV5cC4xX3zZ2yY1=",
                "preshared_key": "qM7sK1dF0hG2jL5kP9oI8uY7tR6eW3qA1sD2fG3hJ4k=",
                "address": "10.0.0.2/32,fd00::2/128",
            }),
        },
        Node {
            tag: "reality-xhttp".into(),
            kind: NodeKind::Vless,
            server: "6.6.6.6".into(),
            port: 443,
            sub_id: "sub2ccccddddeeee".into(),
            groups: Vec::new(),
            extra: serde_json::json!({
                "uuid": "b2f2b2c9-1f4d-4d9d-9b6a-3a5c7e8f9a0b",
                "tls": true,
                "sni": "www.microsoft.com",
                "network": "xhttp",
                "host": "www.microsoft.com",
                "reality": { "public_key": "SbFKmQ6VN0zX9lJ3rV7cP0dT5mY2wH8kN4bQ1sD6fG0", "short_id": "0123" },
                "xhttp": { "mode": "auto" },
            }),
        },
    ]
}

fn golden_nodes_v2() -> Vec<Node> {
    let mut v = golden_nodes();
    v.extend(golden_nodes_newkinds());
    v
}

/// 出站点位序的节点 tag 序列（剔除 direct/selector/urltest 组与 block）
fn node_tag_order(cfg: &serde_json::Value) -> Vec<String> {
    cfg["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| {
            !matches!(
                o["type"].as_str().unwrap_or_default(),
                "direct" | "selector" | "urltest" | "block"
            )
        })
        .map(|o| o["tag"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn assert_byte_equal(fixture: &str, ir: &ir::IrConfig) {
    let want = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(fixture),
    )
    .unwrap_or_else(|e| panic!("夹具 {fixture} 读取失败：{e}"));
    // T-B2-7 起 render 可失败（SSR 等方言拒），golden 输入恒为可渲染协议 → unwrap 合法
    let got = serde_json::to_string_pretty(&singbox::render(ir).unwrap()).unwrap();
    assert_eq!(got, want, "渲染输出与重构前夹具不再逐字节等价：{fixture}");
}

#[test]
fn irGolden_singboxRuleMixed_byteEqual() {
    // T-B2-7 起本夹具输入=v1 四节点+新协议扩臂（夹具同批更新评审：
    // 旧四节点出站段逐字节不变，见 outboundTag_existingUuidTags_stable）
    let nodes = golden_nodes_v2();
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

#[test]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
fn singboxNewKindsGolden_beqFixture() {
    // 扩臂输入 → 更新后的 singbox_rule_mixed.json 逐字节钉（夹具同批更新评审面），
    // 叠加结构断言：三新 type 各就位、REALITY/XHTTP 投影形状正确、SSR 永不漏出。
    let ir = ir::build(
        7890,
        false,
        &golden_nodes_v2(),
        &[] as &[IrRule],
        ir::TAG_PROXY,
        &golden_domains(),
    )
    .unwrap();
    assert_byte_equal("singbox_rule_mixed.json", &ir);
    let cfg = singbox::render(&ir).unwrap();
    let text = serde_json::to_string(&cfg).unwrap();
    assert!(
        !text.contains("shadowsocksr"),
        "SSR 永不进 sing-box 渲染：{text}"
    );
    let find = |t: &str| -> serde_json::Value {
        cfg["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["type"] == t)
            .cloned()
            .unwrap_or_else(|| panic!("渲染输出缺 type={t} 出站"))
    };
    let hy = find("hysteria2");
    assert_eq!(hy["password"], "hy-pw");
    assert_eq!(hy["tls"]["server_name"], "hy.example.com");
    assert_eq!(hy["obfs"]["type"], "salamander");
    assert_eq!(hy["obfs"]["password"], "obfspw");
    let tuic = find("tuic");
    assert_eq!(tuic["uuid"], "9c0dbc5a-1d9f-4f6d-b5e1-0e9b48f2a1cd");
    assert_eq!(tuic["alpn"], serde_json::json!(["h3"]));
    let wg = find("wireguard");
    assert_eq!(
        wg["local_address"],
        serde_json::json!(["10.0.0.2/32", "fd00::2/128"])
    );
    assert_eq!(wg["peer_public_keys"].as_array().unwrap().len(), 1);
    assert_eq!(wg["preshared_keys"].as_array().unwrap().len(), 1);
    let real = find("vless");
    // 首枚 vless（lg-vless）无 reality；reality-xhttp 臂单独按 tag 取
    let reality_ob = cfg["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["tag"] == "sub2cccc:reality-xhttp")
        .cloned()
        .unwrap();
    assert_eq!(reality_ob["tls"]["reality"]["enabled"], true);
    assert_eq!(
        reality_ob["tls"]["reality"]["public_key"],
        "SbFKmQ6VN0zX9lJ3rV7cP0dT5mY2wH8kN4bQ1sD6fG0"
    );
    assert_eq!(reality_ob["tls"]["reality"]["short_id"], "0123");
    assert_eq!(reality_ob["transport"]["type"], "xhttp");
    assert_eq!(reality_ob["transport"]["mode"], "auto");
    assert_eq!(reality_ob["transport"]["host"], "www.microsoft.com");
    // 旧 vless（lg-vless）零污染：无 reality/无 transport.host
    assert!(real["tls"].get("reality").is_none());
}

#[test]
#[allow(non_snake_case)]
fn outboundTag_existingUuidTags_stable() {
    // 红线：扩臂不改存量——v1 四节点的出站 tag（含 8 字符 sub 前缀）序列
    // 在 v2 渲染中必须是严格前缀（前端/历史数据引用的 tag 永不漂移）
    let v1_ir = ir::build(
        7890,
        false,
        &golden_nodes(),
        &[] as &[IrRule],
        ir::TAG_PROXY,
        &golden_domains(),
    )
    .unwrap();
    let v2_ir = ir::build(
        7890,
        false,
        &golden_nodes_v2(),
        &[] as &[IrRule],
        ir::TAG_PROXY,
        &golden_domains(),
    )
    .unwrap();
    let v1_tags = node_tag_order(&singbox::render(&v1_ir).unwrap());
    let v2_tags = node_tag_order(&singbox::render(&v2_ir).unwrap());
    let want_old = [
        "sub0aaaa:hk-1",
        "sub0aaaa:tokyo-2",
        "sub1bbbb:lg-vless",
        "sub1bbbb:fr-vmess",
    ];
    assert_eq!(v1_tags, want_old.to_vec());
    let old_owned: Vec<String> = want_old.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        &v2_tags[..v1_tags.len()],
        &old_owned[..],
        "存量 tag 序列被扩臂扰动"
    );
    let want_new: Vec<String> = [
        "sub0aaaa:hy-seoul",
        "sub1bbbb:tuic-oslo",
        "sub2cccc:wg-paris",
        "sub2cccc:reality-xhttp",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(&v2_tags[v1_tags.len()..], &want_new[..]);
}
