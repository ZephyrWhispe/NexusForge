//! sing-box 方言渲染器（B2 T-B2-1）：[`IrConfig`] → sing-box 配置 JSON。
//!
//! 逐字节等价红线：输出与 B2 重构前的 `config::generate` 完全一致，由
//! `tests/golden/*.json`（重构前代码生成）+ `irGolden_*_byteEqual` 锁死。
//! 本仓库 serde_json 未开 preserve_order，Object 为 BTreeMap 规范序，
//! 因此等价判定=序列化字符串全等（与 service.rs 落盘 `to_vec_pretty` 同形）。

use serde_json::{json, Value};

use crate::error::{ProxyError, Result};
use crate::ir::{IrConfig, IrInbound, IrOutbound, IrRule, IrRuleField, TAG_BLOCK, TAG_DIRECT};
use crate::sub::{Node, NodeKind};

/// IR → sing-box JSON（渲染即校验：方言无法表达的构造诚实 Err，如 SSR——
/// sing-box 官方主线不支持，09 §5.1-⑭；Err 形状与 xray/mihomo 方言统一）
pub fn render(ir: &IrConfig) -> Result<Value> {
    let outbounds = ir
        .outbounds
        .iter()
        .map(render_outbound)
        .collect::<Result<Vec<Value>>>()?;
    let mut cfg = json!({
        "log": { "level": "info", "timestamp": true },
        "dns": render_dns(ir),
        "inbounds": render_inbound(&ir.inbound),
        "outbounds": outbounds,
        "route": {
            "rules": ir.route.rules.iter().map(render_rule).collect::<Vec<Value>>(),
            "final": ir.route.final_target,
            "auto_detect_interface": true,
        },
    });
    // T-B2-10：geo 规则在场才注入 rule_set 数据面块——与 block 出口"仅被引用才
    // 注入"同款纪律：预声明不存在的本地文件会让内核在无 geo 规则时也起不来。
    if let Some(sets) = render_rule_sets(ir) {
        cfg["rule_set"] = Value::Array(sets);
    }
    Ok(cfg)
}

/// geo 规则引用的码表 → sing-box 本地 rule_set 条目。通道核证⑭（2026-09-21，
/// releases/expanded_assets）：SagerNet/sing-geoip·geosite 官方 release 只发布
/// `.db` 编译体（**无 .srs 资产**，任务书预测的 srs 通道不存在）→ 定档 geoip.db/
/// geosite.db，rule_set format=binary（.db 即 sing-box 二进制编译格式）。
/// 路径相对配置文件所在 proxy/ 目录解析。
fn render_rule_sets(ir: &IrConfig) -> Option<Vec<Value>> {
    let mut cats: Vec<&'static str> = Vec::new();
    for r in &ir.route.rules {
        if let Some(c) = r.field.geo_category() {
            if !cats.contains(&c) {
                cats.push(c);
            }
        }
    }
    (!cats.is_empty()).then(|| {
        cats.iter()
            .map(|c| {
                json!({
                    "type": "local",
                    "format": "binary",
                    "path": format!("geo/{c}.db"),
                    "tag": c,
                })
            })
            .collect()
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

fn render_outbound(outbound: &IrOutbound) -> Result<Value> {
    let v = match outbound {
        IrOutbound::Node(node) => node_to_outbound(node)?,
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
    };
    Ok(v)
}

fn render_rule(rule: &IrRule) -> Value {
    let mut r = match &rule.field {
        IrRuleField::IpIsPrivate => json!({ "ip_is_private": true }),
        IrRuleField::Domain => json!({ "domain": rule.patterns }),
        IrRuleField::DomainSuffix => json!({ "domain_suffix": rule.patterns }),
        IrRuleField::Keyword => json!({ "keyword": rule.patterns }),
        IrRuleField::IpCidr => json!({ "ip_cidr": rule.patterns }),
        IrRuleField::Process => json!({ "process_name": rule.patterns }),
        // T-B2-10（09 §5.2 行字面形）：rule_set 引用 + 单码 payload，
        // 数据面块由 render_rule_sets 在场注入（缺件预检在 service 层）
        IrRuleField::GeoSite(code) => json!({ "rule_set": ["geosite"], "payload": code }),
        IrRuleField::GeoIp(code) => json!({ "rule_set": ["geoip"], "payload": code }),
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

/// 单节点 → sing-box 出站（v1 四协议 + T-B2-7 四新协议；新 NodeKind 变体 =
/// 编译期强制同步本 match。SSR 如实 Err：sing-box 官方主线不支持（⑭），
/// service 层按 supported_kinds 过滤后理论上到不了这里，本 Err 是双保险红线）
fn node_to_outbound(node: &Node) -> Result<Value> {
    let tag = node.outbound_tag();
    let v = match node.kind {
        NodeKind::Shadowsocks => {
            let mut o = json!({
                "type": "shadowsocks",
                "tag": tag,
                "server": node.server,
                "server_port": node.port,
                "method": node.extra["method"],
                "password": node.extra["password"],
            });
            // ss plugin 透传（T-B2-7）：`obfs-local;obfs=tls;...` 按首个 ';' 拆
            // plugin/plugin_opts。参数体语义不解析=原样透传；若目标 sing-box 版本
            // 不认该插件，内核启动失败会走既有诚实 on_exit/stop_kernel_and_restore 路径。
            if let Some(plugin) = node.extra["plugin"].as_str() {
                if !plugin.is_empty() {
                    match plugin.split_once(';') {
                        Some((name, opts)) => {
                            o["plugin"] = json!(name);
                            o["plugin_opts"] = json!(opts);
                        }
                        None => o["plugin"] = json!(plugin),
                    }
                }
            }
            o
        }
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
        NodeKind::Hysteria2 => {
            let mut o = json!({
                "type": "hysteria2",
                "tag": tag,
                "server": node.server,
                "server_port": node.port,
                "password": node.extra["password"],
            });
            let sni = node.extra["sni"].as_str().unwrap_or_default();
            let insecure = node.extra["insecure"].as_bool().unwrap_or(false);
            if !sni.is_empty() || insecure {
                let mut tls = json!({ "enabled": true });
                if !sni.is_empty() {
                    tls["server_name"] = json!(sni);
                }
                if insecure {
                    tls["insecure"] = json!(true);
                }
                o["tls"] = tls;
            }
            if node.extra["obfs"].as_str().is_some() {
                o["obfs"] = json!({
                    "type": node.extra["obfs"],
                    "password": node.extra["obfs_password"],
                });
            }
            o
        }
        NodeKind::Tuic5 => {
            let mut o = json!({
                "type": "tuic",
                "tag": tag,
                "server": node.server,
                "server_port": node.port,
                "uuid": node.extra["uuid"],
                "password": node.extra["password"],
            });
            let sni = node.extra["sni"].as_str().unwrap_or_default();
            if !sni.is_empty() {
                o["tls"] = json!({ "enabled": true, "server_name": sni });
            }
            let alpn = node.extra["alpn"].as_str().unwrap_or_default();
            if !alpn.is_empty() {
                o["alpn"] = json!(alpn.split(',').collect::<Vec<_>>());
            }
            o
        }
        NodeKind::WireGuard => {
            let address = node.extra["address"].as_str().unwrap_or_default();
            let preshared = node.extra["preshared_key"].as_str().unwrap_or_default();
            let mut o = json!({
                "type": "wireguard",
                "tag": tag,
                "server": node.server,
                "server_port": node.port,
                "local_address": address
                    .split(',')
                    .filter(|a| !a.is_empty())
                    .collect::<Vec<_>>(),
                "private_key": node.extra["private_key"],
                "peer_public_keys": [node.extra["public_key"]],
            });
            if !preshared.is_empty() {
                o["preshared_keys"] = json!([preshared]);
            }
            o
        }
        NodeKind::ShadowsocksR => {
            return Err(ProxyError::Config(format!(
                "sing-box 官方主线不支持 SSR（ShadowsocksR）出口：节点 {} 仅作展示保留，请换用支持 SSR 的第三方分叉内核",
                tag
            )))
        }
    };
    Ok(v)
}

/// network/tls/sni 通用投影（vmess/vless/trojan 共用；T-B2-7 扩 REALITY/XHTTP/host）
fn apply_transport_tls(outbound: &mut Value, node: &Node) {
    if node.extra["tls"] == json!(true) {
        let sni = node.extra["sni"].as_str().unwrap_or_default();
        let mut tls = json!({ "enabled": true });
        if !sni.is_empty() {
            tls["server_name"] = json!(sni);
        }
        // REALITY（sing-box 1.10 官方形状：tls.reality{enabled,public_key,short_id}）；
        // 白名单只展开 public_key/short_id 两键（fingerprint/spiderX 等 xray 侧参数不在此）
        if let Some(reality) = node.extra["reality"].as_object() {
            let mut r = json!({ "enabled": true });
            if let Some(pk) = reality.get("public_key").and_then(Value::as_str) {
                r["public_key"] = json!(pk);
            }
            if let Some(sid) = reality.get("short_id").and_then(Value::as_str) {
                if !sid.is_empty() {
                    r["short_id"] = json!(sid);
                }
            }
            tls["reality"] = r;
        }
        outbound["tls"] = tls;
    }
    match node.extra["network"].as_str() {
        Some("ws") => {
            let mut t = json!({
                "type": "ws",
                "path": node.extra["path"].as_str().unwrap_or("/")
            });
            if let Some(h) = node.extra["host"].as_str() {
                if !h.is_empty() {
                    t["host"] = json!(h);
                }
            }
            outbound["transport"] = t;
        }
        // XHTTP（sing-box 1.10 transport 形状；extra["xhttp"] 只带 mode 一键）
        Some("xhttp") => {
            let mut t = json!({ "type": "xhttp" });
            if let Some(mode) = node.extra["xhttp"]["mode"].as_str() {
                if !mode.is_empty() {
                    t["mode"] = json!(mode);
                }
            }
            if let Some(h) = node.extra["host"].as_str() {
                if !h.is_empty() {
                    t["host"] = json!(h);
                }
            }
            outbound["transport"] = t;
        }
        _ => {}
    }
}

#[cfg(test)]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
mod tests {
    use super::*;
    use crate::ir::{self, IrRule};
    use crate::sub::Node;

    fn node(kind: NodeKind) -> Node {
        Node {
            tag: "n1".into(),
            kind,
            server: "1.2.3.4".into(),
            port: 8388,
            sub_id: "sub0aaaabbbbcccc".into(),
            groups: Vec::new(),
            extra: serde_json::json!({"method": "aes-256-gcm", "password": "p1"}),
        }
    }

    fn ir_of(nodes: &[Node]) -> IrConfig {
        ir::build(7890, false, nodes, &[] as &[IrRule], ir::TAG_PROXY, &[]).unwrap()
    }

    #[test]
    fn singboxRender_ssr_rejected() {
        // 红线：SSR 三内核官方均不支持（09 §5.1-⑭）→ 方言渲染必须诚实 Err，禁静默降级成 ss
        let err = render(&ir_of(&[node(NodeKind::ShadowsocksR)]));
        match err {
            Err(ProxyError::Config(msg)) => assert!(msg.contains("SSR"), "{msg}"),
            other => panic!("SSR 渲染必须 Config 拒，实得 {other:?}"),
        }
    }

    #[test]
    fn singboxRender_newKinds_supported() {
        // 完成判据面：supported_kinds 七协议与渲染臂同步（新臂零 Err 即形状可用）。
        // 注意 sing-box 出站 type 名与 NodeKind::as_str 不恒同（tuic5 → "tuic"）。
        for (kind, singbox_type) in [
            (NodeKind::Hysteria2, "hysteria2"),
            (NodeKind::Tuic5, "tuic"),
            (NodeKind::WireGuard, "wireguard"),
        ] {
            let cfg = render(&ir_of(&[node(kind)]))
                .unwrap_or_else(|e| panic!("{} 渲染应成功：{e:?}", kind.as_str()));
            let ob = &cfg["outbounds"][2];
            assert_eq!(ob["type"], singbox_type, "{} 出站 type 错位", kind.as_str());
        }
    }

    #[test]
    fn geoRules_singboxRenderRuleSet() {
        // T-B2-10 红线：geo 规则在场才注入 rule_set 数据面块（.db binary 编译体，
        // 核证⑭：官方通道无 .srs），引用规则=row literal 形 {rule_set,payload}
        let nodes = [node(NodeKind::Shadowsocks)];
        let rules = vec![
            IrRule {
                field: IrRuleField::GeoSite("cn".into()),
                patterns: Vec::new(),
                target: ir::TAG_DIRECT.into(),
            },
            IrRule {
                field: IrRuleField::GeoIp("private".into()),
                patterns: Vec::new(),
                target: ir::TAG_PROXY.into(),
            },
        ];
        let cfg = ir::build(7890, false, &nodes, &rules, ir::TAG_PROXY, &[]).unwrap();
        let rendered = render(&cfg).unwrap();
        assert_eq!(
            rendered["rule_set"],
            json!([
                { "type": "local", "format": "binary", "path": "geo/geosite.db", "tag": "geosite" },
                { "type": "local", "format": "binary", "path": "geo/geoip.db", "tag": "geoip" }
            ]),
            "rule_set 段=按引用序的 local/binary 条目：{rendered}"
        );
        let geo: Vec<&Value> = rendered["route"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r.get("rule_set").is_some())
            .collect();
        assert_eq!(
            geo,
            vec![
                &json!({ "payload": "cn", "rule_set": ["geosite"], "outbound": "direct" }),
                &json!({ "payload": "private", "rule_set": ["geoip"], "outbound": "proxy" }),
            ]
        );
        // 负对照：无 geo 规则 → 全配置无 rule_set 键（三内核 golden 零漂移的机制面）
        let plain = render(&ir_of(&nodes)).unwrap();
        assert!(
            plain.get("rule_set").is_none(),
            "无 geo 规则不得混入数据面块：{plain}"
        );
    }
}
