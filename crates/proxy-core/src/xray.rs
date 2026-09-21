//! Xray-core 方言渲染器 + 驱动（B2 T-B2-5）：[`IrConfig`] → xray 配置 JSON。
//!
//! 方言差核证结论（09 §5.1-⑭"以官方文档/源码核证并把结论写进码内"）：
//! - **无 mixed 入站** → http(P) + socks(P+1) 双入站；系统代理与健康探活指
//!   http 端口（恒等于 mixed_port，[`KernelDriver::probe_port`](crate::kernel::KernelDriver::probe_port)
//!   默认实现与双入站的耦合由 `xrayDualInbound_probePortHttp` 钉住）；
//! - **无 selector/urltest 出站组**（xray 官方无策略组）→ 策略组退化：出口 =
//!   [`IrOutbound::Selector`] 的 default_tag（须指向真实节点出站）或首个节点，
//!   路由无 final 字段（xray 语义：未匹配流量走 outbounds 数组第一项 →
//!   兜底出口恒钉索引 0，规则里的 proxy/auto 引用就地改写为该出口）；
//! - domainStrategy 官方合法值 = AsIs / IPIfNonMatch / IPOnDemand（核证
//!   xtls.github.io/config/routing.html；任务书字面 "AsOrigin" 为不存在的值，
//!   按行内核证条款修正）→ 取 **AsIs**（不查 IP，与 sing-box 默认行为最近）；
//! - `ip_is_private` 展开为显式 CIDR 清单：`geoip:private` 需 geoip.dat 资产，
//!   那是 T-B2-10 artifact 通道的承诺面，本行保持零资产依赖；
//! - **TUN 如实关**（caps.tun=false）：xray 主线不含官方 TUN 通道 →
//!   `enter_tun` 门禁如实拒（service.rs `xrayTun_rejectedWithHonestError`）。

use serde_json::{json, Value};

use crate::error::{ProxyError, Result};
use crate::ir::{
    IrConfig, IrInbound, IrOutbound, IrRule, IrRuleField, TAG_AUTO, TAG_BLOCK, TAG_DIRECT,
    TAG_PROXY,
};
use crate::kernel::{KernelCaps, KernelDriver};
use crate::sub::{Node, NodeKind};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 私有地址段（与 sing-box `ip_is_private` 判定集等价的手动展开；v6 环回与 ULA 同列）
const PRIVATE_CIDRS: &[&str] = &[
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "::1/128",
    "fc00::/7",
];

/// IR → xray 配置 JSON（渲染即校验；无法方言表达的构造返回 Err，禁 panic）
pub fn render(ir: &IrConfig) -> Result<Value> {
    let (listen, http_port, socks_port) = match &ir.inbound {
        IrInbound::Mixed { listen, port } => {
            let socks = port.checked_add(1).ok_or_else(|| {
                ProxyError::Config(format!(
                    "xray 方言需双入站端口 {}+1，已越界（u16 上限）：请把 mixed 端口改为 ≤ 65534",
                    port
                ))
            })?;
            (listen.clone(), *port, socks)
        }
        IrInbound::Tun { .. } => {
            return Err(ProxyError::Config(
                "xray 方言不支持 TUN 入站（内核能力已如实关闭）".into(),
            ))
        }
    };

    // 策略组退化决议出的唯一兜底出口（xray 无 final：outbounds 数组第一项）
    let egress = resolve_egress(ir)?;
    let mut outbounds = vec![outbound_by_tag(ir, &egress)?];
    for ob in &ir.outbounds {
        match ob {
            IrOutbound::Node(n) => {
                let t = n.outbound_tag();
                if t != egress {
                    outbounds.push(node_to_outbound(n)?);
                }
            }
            IrOutbound::Direct => {
                if egress != TAG_DIRECT {
                    outbounds
                        .push(json!({ "protocol": "freedom", "tag": TAG_DIRECT, "settings": {} }));
                }
            }
            IrOutbound::Block => {
                if egress != TAG_BLOCK {
                    outbounds
                        .push(json!({ "protocol": "blackhole", "tag": TAG_BLOCK, "settings": {} }));
                }
            }
            // 策略组不渲染：Selector/Urltest 语义已被 egress 决议吸收
            IrOutbound::Selector { .. } | IrOutbound::Urltest { .. } => {}
        }
    }

    let rules: Vec<Value> = ir
        .route
        .rules
        .iter()
        .map(|r| render_rule(r, &egress))
        .collect();
    Ok(json!({
        "log": { "loglevel": "warning" },
        "inbounds": [
            { "protocol": "http", "listen": listen, "port": http_port, "tag": "http-in" },
            {
                "protocol": "socks",
                "listen": "127.0.0.1",
                "port": socks_port,
                "tag": "socks-in",
                "settings": { "auth": "noauth", "udp": true },
            },
        ],
        "outbounds": outbounds,
        "routing": {
            "domainStrategy": "AsIs",
            "rules": rules,
        },
    }))
}

/// 策略组退化决议：final=proxy/auto → Selector.default_tag（须指向节点）或首个节点；
/// final 本就是可渲染出口 tag（direct/block/具体节点）时直接采用。
fn resolve_egress(ir: &IrConfig) -> Result<String> {
    match ir.route.final_target.as_str() {
        TAG_PROXY | TAG_AUTO => {
            let nodes = node_tags(ir);
            for ob in &ir.outbounds {
                if let IrOutbound::Selector {
                    tag, default_tag, ..
                } = ob
                {
                    if tag == TAG_PROXY {
                        // default_tag 只有在指向真实节点出站时生效（auto/direct 等组名对 xray 无意义）
                        if let Some(d) = default_tag {
                            if nodes.iter().any(|t| t == d) {
                                return Ok(d.clone());
                            }
                        }
                    }
                }
            }
            nodes
                .into_iter()
                .next()
                .ok_or_else(|| ProxyError::Config("xray 渲染：IR 中无节点出站".into()))
        }
        other => {
            let renderable = ir.outbounds.iter().any(|o| match o {
                IrOutbound::Node(n) => n.outbound_tag() == other,
                IrOutbound::Direct => other == TAG_DIRECT,
                IrOutbound::Block => other == TAG_BLOCK,
                _ => false,
            });
            if renderable {
                Ok(other.to_string())
            } else {
                Err(ProxyError::Config(format!(
                    "xray 渲染：兜底目标 {other} 不是可渲染的出口（组名对 xray 无意义）"
                )))
            }
        }
    }
}

fn node_tags(ir: &IrConfig) -> Vec<String> {
    ir.outbounds
        .iter()
        .filter_map(|o| match o {
            IrOutbound::Node(n) => Some(n.outbound_tag()),
            _ => None,
        })
        .collect()
}

/// 按 tag 渲染单项出站（egress 决议结果 → outbounds[0] 的兜底对象）
fn outbound_by_tag(ir: &IrConfig, want: &str) -> Result<Value> {
    for ob in &ir.outbounds {
        match ob {
            IrOutbound::Node(n) if n.outbound_tag() == want => return node_to_outbound(n),
            IrOutbound::Direct if want == TAG_DIRECT => {
                return Ok(json!({ "protocol": "freedom", "tag": TAG_DIRECT, "settings": {} }))
            }
            IrOutbound::Block if want == TAG_BLOCK => {
                return Ok(json!({ "protocol": "blackhole", "tag": TAG_BLOCK, "settings": {} }))
            }
            _ => {}
        }
    }
    Err(ProxyError::Config(format!(
        "xray 渲染：出口 {want} 在 IR 中不存在"
    )))
}

/// 单条路由规则 → xray routing.rules；proxy/auto 引用就地改写为决议出口
fn render_rule(rule: &IrRule, egress: &str) -> Value {
    let target = match rule.target.as_str() {
        TAG_PROXY | TAG_AUTO => egress,
        other => other,
    };
    let mut v = match rule.field {
        IrRuleField::IpIsPrivate => json!({ "type": "field", "ip": PRIVATE_CIDRS }),
        IrRuleField::Domain => {
            json!({ "type": "field", "domain": prefixed(&rule.patterns, "full:") })
        }
        IrRuleField::DomainSuffix => {
            json!({ "type": "field", "domain": prefixed(&rule.patterns, "suffix:") })
        }
        IrRuleField::Keyword => {
            json!({ "type": "field", "domain": prefixed(&rule.patterns, "keyword:") })
        }
        IrRuleField::IpCidr => json!({ "type": "field", "ip": rule.patterns }),
        // 核证 xtls.github.io/config/routing.html：processName 为 Windows/macOS
        // 用户级字段，数组形态与 sing-box process_name 同构
        IrRuleField::Process => json!({ "type": "field", "processName": rule.patterns }),
    };
    v["outboundTag"] = json!(target);
    v
}

fn prefixed(patterns: &[String], prefix: &str) -> Vec<String> {
    patterns.iter().map(|p| format!("{prefix}{p}")).collect()
}

/// 单节点 → xray 出站（v1 四协议；新 NodeKind 变体 = 编译期强制同步本 match）
fn node_to_outbound(node: &Node) -> Result<Value> {
    let tag = node.outbound_tag();
    let v = match node.kind {
        NodeKind::Shadowsocks => json!({
            "tag": tag,
            "protocol": "shadowsocks",
            "settings": { "servers": [{
                "address": node.server,
                "port": node.port,
                "method": node.extra["method"],
                "password": node.extra["password"],
            }]},
        }),
        NodeKind::Vmess => {
            let mut user = json!({
                "id": node.extra["uuid"],
                "alterId": node.extra["alter_id"],
            });
            if let Some(sec) = node.extra["security"].as_str() {
                user["security"] = json!(sec);
            }
            let mut o = json!({
                "tag": tag,
                "protocol": "vmess",
                "settings": { "vnext": [{
                    "address": node.server,
                    "port": node.port,
                    "users": [user],
                }]},
            });
            apply_stream(&mut o, node);
            o
        }
        NodeKind::Trojan => {
            let mut o = json!({
                "tag": tag,
                "protocol": "trojan",
                "settings": { "servers": [{
                    "address": node.server,
                    "port": node.port,
                    "password": node.extra["password"],
                }]},
            });
            apply_stream(&mut o, node);
            o
        }
        NodeKind::Vless => {
            let mut user = json!({ "id": node.extra["uuid"], "encryption": "none" });
            if let Some(flow) = node.extra["flow"].as_str() {
                if !flow.is_empty() {
                    user["flow"] = json!(flow);
                }
            }
            let mut o = json!({
                "tag": tag,
                "protocol": "vless",
                "settings": { "vnext": [{
                    "address": node.server,
                    "port": node.port,
                    "users": [user],
                }]},
            });
            apply_stream(&mut o, node);
            o
        }
        // T-B2-7 四新协议：xray 官方支持 hysteria2/tuic/wireguard，但本方言臂
        // 暂按 supported_kinds（仍 4）如实拒——扩臂属后续批次能力矩阵（⑭）承诺面，
        // service 层过滤保证这些节点在 xray 下不进渲染；SSR 全内核均不支持（⑭）。
        NodeKind::Hysteria2 | NodeKind::Tuic5 | NodeKind::WireGuard => {
            return Err(ProxyError::Config(format!(
                "xray 方言当前仅支持 v1 四协议，不支持 {}：节点 {}",
                node.kind.as_str(),
                tag
            )))
        }
        NodeKind::ShadowsocksR => {
            return Err(ProxyError::Config(format!(
                "xray 官方主线不支持 SSR（ShadowsocksR）出口：节点 {}",
                tag
            )))
        }
    };
    Ok(v)
}

/// streamSettings 通用投影（tls / ws；xray 用 security+network 两段而非 sing-box 的分层）。
/// REALITY/XHTTP 透传键（extra["reality"]/extra["xhttp"]）在本方言**暂不展开**：
/// xray 的 realitySettings/xhttpSettings 字段形状与 sing-box 白名单不同源，
/// 按 09 §5.1-⑭ 能力矩阵留待后续批次核证后扩臂；行字面（§5.2 T-B2-7）只承诺
/// sing-box 官方映射的渲染端。
fn apply_stream(outbound: &mut Value, node: &Node) {
    let tls = node.extra["tls"] == json!(true);
    let ws = node.extra["network"].as_str() == Some("ws");
    if !tls && !ws {
        return;
    }
    let mut ss = json!({});
    if ws {
        ss["network"] = json!("ws");
        ss["wsSettings"] = json!({ "path": node.extra["path"].as_str().unwrap_or("/") });
    }
    if tls {
        ss["security"] = json!("tls");
        let sni = node.extra["sni"].as_str().unwrap_or_default();
        if !sni.is_empty() {
            ss["tlsSettings"] = json!({ "serverName": sni });
        }
    }
    outbound["streamSettings"] = ss;
}

/// xray 内核驱动（T-B2-5 注册表第二臂）。exe 由 T-B2-4 资产表通道安装。
pub struct XrayDriver {
    exe: PathBuf,
}

impl XrayDriver {
    pub fn new(exe: PathBuf) -> Self {
        Self { exe }
    }
}

impl KernelDriver for XrayDriver {
    fn id(&self) -> &'static str {
        "xray"
    }

    fn exe_path(&self) -> PathBuf {
        self.exe.clone()
    }

    fn display_name(&self) -> &'static str {
        "Xray-core"
    }

    /// 同目录与 sing-box 的 config.json 互踩规避（09 §5.1-④）
    fn cfg_name(&self) -> &'static str {
        "config-xray.json"
    }

    fn supported_kinds(&self) -> &'static [NodeKind] {
        use NodeKind::*;
        &[Shadowsocks, Vmess, Trojan, Vless]
    }

    fn caps(&self) -> KernelCaps {
        KernelCaps {
            tun: false,
            policy_groups: false,
            external_controller: false,
        }
    }

    fn config_render(&self, ir: &IrConfig) -> Result<String> {
        Ok(serde_json::to_string_pretty(&render(ir)?)?)
    }

    fn build_command(&self, _work_dir: &Path, cfg: &Path) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.arg("run").arg("-c").arg(cfg);
        cmd
    }
}

#[cfg(test)]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
mod tests {
    use super::*;
    use crate::ir::{self, IrRule, IrRuleField};
    use crate::sub::Node;

    fn nodes() -> Vec<Node> {
        vec![
            Node {
                tag: "hk-1".into(),
                kind: NodeKind::Shadowsocks,
                server: "1.2.3.4".into(),
                port: 8388,
                sub_id: "sub0aaaabbbbcccc".into(),
                groups: Vec::new(),
                extra: json!({ "method": "aes-256-gcm", "password": "p1" }),
            },
            Node {
                tag: "tokyo-2".into(),
                kind: NodeKind::Trojan,
                server: "example.com".into(),
                port: 443,
                sub_id: "sub0aaaabbbbcccc".into(),
                groups: Vec::new(),
                extra: json!({ "password": "p2", "sni": "example.com", "tls": true }),
            },
        ]
    }

    fn mixed_ir(port: u16) -> ir::IrConfig {
        ir::build(port, false, &nodes(), &[], TAG_PROXY, &[]).unwrap()
    }

    #[test]
    fn xrayNoPolicyGroups_selectedOrFirstSingleOutbound() {
        // 红线：输出不得出现 selector/urltest 对象
        let cfg = render(&mixed_ir(7890)).unwrap();
        let text = serde_json::to_string(&cfg).unwrap();
        assert!(
            !text.contains("selector") && !text.contains("urltest"),
            "xray 无策略组，退化渲染不得漏出组对象：{text}"
        );
        let outbounds = cfg["outbounds"].as_array().unwrap();
        // default_tag=None → 首节点兜底，且恒钉 outbounds[0]（xray 未匹配流量走第一项）
        assert_eq!(outbounds[0]["tag"], "sub0aaaa:hk-1");
        // 选定态：Selector.default_tag 指向第二节点 → 兜底出口换人
        let mut sel = mixed_ir(7890);
        for ob in &mut sel.outbounds {
            if let IrOutbound::Selector {
                tag, default_tag, ..
            } = ob
            {
                if tag == TAG_PROXY {
                    *default_tag = Some("sub0aaaa:tokyo-2".into());
                }
            }
        }
        let cfg2 = render(&sel).unwrap();
        assert_eq!(cfg2["outbounds"][0]["tag"], "sub0aaaa:tokyo-2");
        // default_tag 悬空（指向不存在的 tag）回落首节点，禁渲染出幽灵引用
        for ob in &mut sel.outbounds {
            if let IrOutbound::Selector {
                tag, default_tag, ..
            } = ob
            {
                if tag == TAG_PROXY {
                    *default_tag = Some("ghost".into());
                }
            }
        }
        assert_eq!(
            render(&sel).unwrap()["outbounds"][0]["tag"],
            "sub0aaaa:hk-1"
        );
    }

    #[test]
    fn xrayDualInbound_probePortHttp() {
        let ir_cfg = mixed_ir(7890);
        let cfg = render(&ir_cfg).unwrap();
        let inbounds = cfg["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 2, "xray 无 mixed：必须 http+socks 双入站");
        assert_eq!(inbounds[0]["protocol"], "http");
        assert_eq!(
            inbounds[0]["port"], 7890,
            "系统代理指向 http 端口 = mixed_port"
        );
        assert_eq!(inbounds[1]["protocol"], "socks");
        assert_eq!(inbounds[1]["port"], 7891, "P/P+1 相邻约定");
        // 探活端口默认实现与双入站的耦合：http 端口恒等于 mixed_port
        let d = XrayDriver::new(PathBuf::from("xray.exe"));
        assert_eq!(d.probe_port(&ir_cfg), 7890);
        let tun = ir::build(7890, true, &nodes(), &[], TAG_PROXY, &[]).unwrap();
        assert_eq!(d.probe_port(&tun), 0, "TUN 无本地端口，探活位归零");
    }

    #[test]
    fn xrayRender_ruleTargetsRemappedToEgress() {
        // 用户规则引用 proxy/auto → 就地改写为决议出口；direct 引用原样保留
        let user = vec![
            IrRule {
                field: IrRuleField::DomainSuffix,
                patterns: vec!["intranet.local".into()],
                target: TAG_DIRECT.into(),
            },
            IrRule {
                field: IrRuleField::Keyword,
                patterns: vec!["ads".into()],
                target: TAG_PROXY.into(),
            },
            IrRule {
                field: IrRuleField::Domain,
                patterns: vec!["blocked.test".into()],
                target: TAG_BLOCK.into(),
            },
            IrRule {
                field: IrRuleField::IpCidr,
                patterns: vec!["10.10.0.0/16".into()],
                target: TAG_AUTO.into(),
            },
        ];
        let cfg_ir = ir::build(7890, false, &nodes(), &user, TAG_PROXY, &[] as &[String]).unwrap();
        let cfg = render(&cfg_ir).unwrap();
        let rules = cfg["routing"]["rules"].as_array().unwrap();
        let tags: Vec<&str> = rules
            .iter()
            .map(|r| r["outboundTag"].as_str().unwrap())
            .collect();
        // [0]=ip_is_private(direct) [1..]=user… —— 首条恒私有地址种子
        assert_eq!(tags[0], TAG_DIRECT);
        assert_eq!(
            &tags[1..],
            &["direct", "sub0aaaa:hk-1", "block", "sub0aaaa:hk-1"]
        );
        // block 出站被引用才渲染，且只渲染一次（egress 非 block 时无重复）
        let ob_text = serde_json::to_string(&cfg["outbounds"]).unwrap();
        assert_eq!(ob_text.matches("\"blackhole\"").count(), 1);
        assert_eq!(ob_text.matches("\"freedom\"").count(), 1);
    }

    #[test]
    fn xrayRender_tunInbound_rejected() {
        let tun = ir::build(7890, true, &nodes(), &[], TAG_PROXY, &[]).unwrap();
        match render(&tun) {
            Err(ProxyError::Config(msg)) => assert!(msg.contains("TUN"), "{msg}"),
            other => panic!("xray 方言必须拒 TUN 入站，得 {other:?}"),
        }
    }

    #[test]
    fn xrayRender_unsupportedKinds_rejected() {
        // T-B2-7：xray 臂保持 4 协议（supported_kinds 与渲染臂同步，⑭ 能力矩阵）：
        // hy2/tuic/wg/ssr 混入 IR 时渲染层必须点名拒，禁静默降级成 ss/vmess 冒充
        let mk = |kind: NodeKind| Node {
            tag: "x".into(),
            kind,
            server: "1.1.1.1".into(),
            port: 443,
            sub_id: "sub0aaaabbbbcccc".into(),
            groups: Vec::new(),
            extra: json!({"password": "p", "uuid": "u"}),
        };
        for kind in [
            NodeKind::Hysteria2,
            NodeKind::Tuic5,
            NodeKind::WireGuard,
            NodeKind::ShadowsocksR,
        ] {
            let cfg =
                ir::build(7890, false, &[mk(kind)], &[] as &[IrRule], TAG_PROXY, &[]).unwrap();
            match render(&cfg) {
                Err(ProxyError::Config(msg)) => assert!(
                    msg.contains(kind.as_str()) || msg.contains("SSR"),
                    "错误须点名被拒协议：{msg}"
                ),
                other => panic!("xray 必须拒 {}，得 {other:?}", kind.as_str()),
            }
        }
    }

    #[test]
    fn xrayRender_socksPortOverflow_rejected() {
        // mixed=65535 → socks=65536 越界，必须在渲染层诚实拒而非 panic/回绕
        let err = render(&mixed_ir(u16::MAX));
        assert!(matches!(err, Err(ProxyError::Config(_))), "{err:?}");
    }
}
