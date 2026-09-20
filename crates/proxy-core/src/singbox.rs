//! sing-box 方言渲染器（B2 T-B2-1）：[`IrConfig`] → sing-box 配置 JSON。
//!
//! 逐字节等价红线：输出与 B2 重构前的 `config::generate` 完全一致，由
//! `tests/golden/*.json`（重构前代码生成）+ `irGolden_*_byteEqual` 锁死。
//! 本仓库 serde_json 未开 preserve_order，Object 为 BTreeMap 规范序，
//! 因此等价判定=序列化字符串全等（与 service.rs 落盘 `to_vec_pretty` 同形）。

use serde_json::{json, Value};

use crate::ir::{IrConfig, IrInbound, IrOutbound, IrRule, IrRuleField, TAG_BLOCK, TAG_DIRECT};
use crate::sub::{Node, NodeKind};

/// IR → sing-box JSON（渲染即校验：无法投影的节点在 build 前的方言入口拒绝）
pub fn render(ir: &IrConfig) -> Value {
    json!({
        "log": { "level": "info", "timestamp": true },
        "dns": render_dns(ir),
        "inbounds": render_inbound(&ir.inbound),
        "outbounds": ir.outbounds.iter().map(render_outbound).collect::<Vec<Value>>(),
        "route": {
            "rules": ir.route.rules.iter().map(render_rule).collect::<Vec<Value>>(),
            "final": ir.route.final_target,
            "auto_detect_interface": true,
        },
    })
}

/// 方言能力入口（T-B2-2 trait config_render 消费）：sing-box 全量支持 v1 IR，
/// 仅拒绝方言无法表达的构造（为 xray/mihomo 的降级拒留有统一形状）。
pub fn validate(_ir: &IrConfig) -> crate::error::Result<()> {
    Ok(())
}

fn render_inbound(inbound: &IrInbound) -> Vec<Value> {
    match inbound {
        IrInbound::Mixed { listen, port } => vec![json!({
            "type": "mixed",
            "tag": "mixed-in",
            "listen": listen,
            "listen_port": port,
        })],
        IrInbound::Tun {
            interface_name,
            address,
            strict_route,
        } => vec![json!({
            "type": "tun",
            "tag": "tun-in",
            "interface_name": interface_name,
            "address": [address],
            "auto_route": true,
            "strict_route": strict_route,
            "stack": "mixed",
        })],
    }
}

fn render_outbound(outbound: &IrOutbound) -> Value {
    match outbound {
        IrOutbound::Node(node) => node_to_outbound(node),
        IrOutbound::Direct => json!({ "type": "direct", "tag": TAG_DIRECT }),
        IrOutbound::Block => json!({ "type": "block", "tag": TAG_BLOCK }),
        IrOutbound::Selector {
            tag,
            outbounds,
            default_tag,
        } => json!({
            "type": "selector",
            "tag": tag,
            "outbounds": outbounds,
            "default": default_tag
                .clone()
                .or_else(|| outbounds.first().cloned())
                .unwrap_or_else(|| TAG_DIRECT.to_string()),
        }),
        IrOutbound::Urltest {
            tag,
            outbounds,
            url,
            interval_secs,
        } => json!({
            "type": "urltest",
            "tag": tag,
            "outbounds": outbounds,
            "url": url,
            "interval": format!("{}m", interval_secs / 60),
        }),
    }
}

fn render_rule(rule: &IrRule) -> Value {
    let mut r = match rule.field {
        IrRuleField::IpIsPrivate => json!({ "ip_is_private": true }),
        IrRuleField::Domain => json!({ "domain": rule.patterns }),
        IrRuleField::DomainSuffix => json!({ "domain_suffix": rule.patterns }),
        IrRuleField::Keyword => json!({ "keyword": rule.patterns }),
        IrRuleField::IpCidr => json!({ "ip_cidr": rule.patterns }),
    };
    r["outbound"] = Value::String(rule.target.clone());
    r
}

fn render_dns(ir: &IrConfig) -> Value {
    json!({
        "servers": [
            { "tag": "remote", "address": ir.dns.remote, "detour": "proxy" },
            { "tag": "local", "address": ir.dns.local, "detour": "direct" }
        ],
        "rules": [
            { "outbound": "any", "server": "local" }
        ],
        "final": "remote",
        "strategy": ir.dns.strategy,
    })
}

/// 单节点 → sing-box 出站（PR3 协议投影；v1 覆盖 4 类常见协议）
fn node_to_outbound(node: &Node) -> Value {
    let tag = node.outbound_tag();
    match node.kind {
        NodeKind::Shadowsocks => json!({
            "type": "shadowsocks",
            "tag": tag,
            "server": node.server,
            "server_port": node.port,
            "method": node.extra["method"],
            "password": node.extra["password"],
        }),
        NodeKind::Vmess => {
            let mut o = json!({
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
            let mut o = json!({
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
            let mut o = json!({
                "type": "vless",
                "tag": tag,
                "server": node.server,
                "server_port": node.port,
                "uuid": node.extra["uuid"],
            });
            if let Some(flow) = node.extra["flow"].as_str() {
                if !flow.is_empty() {
                    o["flow"] = json!(flow);
                }
            }
            apply_transport_tls(&mut o, node);
            o
        }
    }
}

/// network/tls/sni 通用投影（vmess/vless/trojan 共用）
fn apply_transport_tls(outbound: &mut Value, node: &Node) {
    if node.extra["tls"] == json!(true) {
        let sni = node.extra["sni"].as_str().unwrap_or_default();
        let mut tls = json!({ "enabled": true });
        if !sni.is_empty() {
            tls["server_name"] = json!(sni);
        }
        outbound["tls"] = tls;
    }
    if let Some(network) = node.extra["network"].as_str() {
        if network == "ws" {
            outbound["transport"] = json!({
                "type": "ws",
                "path": node.extra["path"].as_str().unwrap_or("/")
            });
        }
    }
}
