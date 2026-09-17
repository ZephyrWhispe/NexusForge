//! PR3 配置生成（docs/impl/05 PR3 后半）：节点 + 模式 → sing-box 配置 JSON。
//!
//! - 入站：mixed（HTTP+SOCKS，系统代理指向它）或 tun（PR5，需管理员 + wintun.dll）
//! - 出站：selector(proxy) + urltest(auto) + 节点 + direct
//! - 路由：私有地址恒直连；规则模式追加用户直连域名列表；global 模式 final=proxy

use serde::Serialize;

use crate::error::{ProxyError, Result};
use crate::sub::{Node, NodeKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RouteMode {
    Off,
    Global,
    Rule,
}

/// 生成参数
pub struct GenOptions<'a> {
    pub mixed_port: u16,
    pub mode: RouteMode,
    /// 规则模式的直连域名列表（用户配置；含泛域名如 `.corp.example.com`）
    pub direct_domains: &'a [String],
    /// true = TUN 入站（强制忽略 mixed_port 语义）
    pub tun: bool,
    pub nodes: &'a [Node],
}

/// 生成 sing-box 配置 JSON（写入 `{appData}/proxy/config.json` 由内核消费）
pub fn generate(opts: &GenOptions) -> Result<serde_json::Value> {
    if opts.nodes.is_empty() {
        return Err(ProxyError::Config(
            "无可用节点：请先添加并更新订阅".into(),
        ));
    }
    let node_tags: Vec<String> = opts.nodes.iter().map(|n| n.outbound_tag()).collect();
    let mut node_outbounds: Vec<serde_json::Value> =
        opts.nodes.iter().map(node_to_outbound).collect();

    let mut selector_outbounds = node_tags.clone();
    selector_outbounds.push("auto".into());
    let mut outbounds = vec![
        serde_json::json!({
            "type": "selector",
            "tag": "proxy",
            "outbounds": selector_outbounds,
            "default": node_tags[0],
        }),
        serde_json::json!({
            "type": "urltest",
            "tag": "auto",
            "outbounds": node_tags,
            "url": "https://www.gstatic.com/generate_204",
            "interval": "5m",
        }),
    ];
    outbounds.append(&mut node_outbounds);
    outbounds.push(serde_json::json!({ "type": "direct", "tag": "direct" }));

    // 路由规则：私有地址恒直连（回环/内网走直连避免自旋）
    let mut rules = vec![serde_json::json!({ "ip_is_private": true, "outbound": "direct" })];
    if opts.mode == RouteMode::Rule && !opts.direct_domains.is_empty() {
        rules.push(serde_json::json!({
            "domain_suffix": opts.direct_domains,
            "outbound": "direct",
        }));
    }

    let inbounds = if opts.tun {
        vec![serde_json::json!({
            "type": "tun",
            "tag": "tun-in",
            "interface_name": "NexusForge0",
            "address": ["172.19.0.1/30"],
            "auto_route": true,
            "strict_route": true,
            "stack": "mixed",
        })]
    } else {
        vec![serde_json::json!({
            "type": "mixed",
            "tag": "mixed-in",
            "listen": "127.0.0.1",
            "listen_port": opts.mixed_port,
        })]
    };

    Ok(serde_json::json!({
        "log": { "level": "info", "timestamp": true },
        "dns": {
            "servers": [
                { "tag": "remote", "address": "https://1.1.1.1/dns-query", "detour": "proxy" },
                { "tag": "local", "address": "local", "detour": "direct" }
            ],
            "rules": [
                { "outbound": "any", "server": "local" }
            ],
            "final": "remote",
            "strategy": "prefer_ipv4"
        },
        "inbounds": inbounds,
        "outbounds": outbounds,
        "route": {
            "rules": rules,
            "final": "proxy",
            "auto_detect_interface": true
        }
    }))
}

/// 单节点 → sing-box 出站（PR3 协议投影；v1 覆盖 4 类常见协议）
fn node_to_outbound(node: &Node) -> serde_json::Value {
    let tag = node.outbound_tag();
    match node.kind {
        NodeKind::Shadowsocks => serde_json::json!({
            "type": "shadowsocks",
            "tag": tag,
            "server": node.server,
            "server_port": node.port,
            "method": node.extra["method"],
            "password": node.extra["password"],
        }),
        NodeKind::Vmess => {
            let mut o = serde_json::json!({
                "type": "vmess",
                "tag": tag,
                "server": node.server,
                "server_port": node.port,
                "uuid": node.extra["uuid"],
                "security": node.extra["security"],
                "alter_id": node.extra["alter_id"],
            });
            apply_transport_tls(&mut o, node);
            o
        }
        NodeKind::Trojan => {
            let mut o = serde_json::json!({
                "type": "trojan",
                "tag": tag,
                "server": node.server,
                "server_port": node.port,
                "password": node.extra["password"],
            });
            apply_transport_tls(&mut o, node);
            o
        }
        NodeKind::Vless => {
            let mut o = serde_json::json!({
                "type": "vless",
                "tag": tag,
                "server": node.server,
                "server_port": node.port,
                "uuid": node.extra["uuid"],
            });
            if let Some(flow) = node.extra["flow"].as_str() {
                if !flow.is_empty() {
                    o["flow"] = serde_json::json!(flow);
                }
            }
            apply_transport_tls(&mut o, node);
            o
        }
    }
}

/// network/tls/sni 通用投影（vmess/vless/trojan 共用）
fn apply_transport_tls(outbound: &mut serde_json::Value, node: &Node) {
    if node.extra["tls"] == serde_json::json!(true) {
        let sni = node.extra["sni"].as_str().unwrap_or_default();
        let mut tls = serde_json::json!({ "enabled": true });
        if !sni.is_empty() {
            tls["server_name"] = serde_json::json!(sni);
        }
        outbound["tls"] = tls;
    }
    if let Some(network) = node.extra["network"].as_str() {
        if network == "ws" {
            outbound["transport"] = serde_json::json!({
                "type": "ws",
                "path": node.extra["path"].as_str().unwrap_or("/")
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes() -> Vec<Node> {
        vec![
            Node {
                tag: "a".into(),
                kind: NodeKind::Shadowsocks,
                server: "1.2.3.4".into(),
                port: 8388,
                sub_id: "sub1".into(),
                extra: serde_json::json!({"method": "aes-256-gcm", "password": "p1"}),
            },
            Node {
                tag: "b".into(),
                kind: NodeKind::Trojan,
                server: "example.com".into(),
                port: 443,
                sub_id: "sub1".into(),
                extra: serde_json::json!({"password": "p2", "sni": "example.com", "tls": true}),
            },
        ]
    }

    #[test]
    fn generates_mixed_global_config() {
        let opts = GenOptions {
            mixed_port: 7890,
            mode: RouteMode::Global,
            direct_domains: &[],
            tun: false,
            nodes: &nodes(),
        };
        let cfg = generate(&opts).unwrap();
        let inbound = &cfg["inbounds"][0];
        assert_eq!(inbound["type"], "mixed");
        assert_eq!(inbound["listen_port"], 7890);
        let outbounds = cfg["outbounds"].as_array().unwrap();
        assert_eq!(outbounds[0]["type"], "selector");
        assert_eq!(outbounds[0]["outbounds"].as_array().unwrap().len(), 3, "2 节点 + auto");
        assert_eq!(outbounds[2]["type"], "shadowsocks");
        assert_eq!(outbounds[3]["type"], "trojan");
        assert_eq!(outbounds[3]["tls"]["server_name"], "example.com");
        assert_eq!(cfg["route"]["final"], "proxy");
    }

    #[test]
    fn generates_tun_config() {
        let opts = GenOptions {
            mixed_port: 7890,
            mode: RouteMode::Global,
            direct_domains: &[],
            tun: true,
            nodes: &nodes(),
        };
        let cfg = generate(&opts).unwrap();
        assert_eq!(cfg["inbounds"][0]["type"], "tun");
        assert_eq!(cfg["inbounds"][0]["auto_route"], true);
    }

    #[test]
    fn rule_mode_includes_direct_domains() {
        let domains = vec![".corp.example.com".to_string(), "internal.local".to_string()];
        let opts = GenOptions {
            mixed_port: 7890,
            mode: RouteMode::Rule,
            direct_domains: &domains,
            tun: false,
            nodes: &nodes(),
        };
        let cfg = generate(&opts).unwrap();
        let rules = cfg["route"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["domain_suffix"].as_array().map(|a| a.len()) == Some(2)));
    }

    #[test]
    fn empty_nodes_rejected() {
        let opts = GenOptions {
            mixed_port: 7890,
            mode: RouteMode::Global,
            direct_domains: &[],
            tun: false,
            nodes: &[],
        };
        assert!(matches!(generate(&opts), Err(ProxyError::Config(_))));
    }
}
