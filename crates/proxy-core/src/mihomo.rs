//! mihomo (Clash 内核) 方言渲染器 + 驱动（B2 T-B2-6）：[`IrConfig`] → config.yaml。
//!
//! 方言形态核证（09 §5.1-⑭ + §5.1-④裁定，结论记此）：
//! - mihomo 原生 mixed-port（HTTP+SOCKS 单端口，与 sing-box 同类）+ 原生 tun
//!   → 入站不降级；能力表全开 `{tun:true, policy_groups:true, external_controller:true}`。
//! - **workdir 语义**：`mihomo -d <dir>` 要求 config.yaml 住在 dir 内（Country.mmdb
//!   同栖，09 ④ "mihomo config.yaml 且 workdir 用 proxy/mihomo/ 子目录"）→
//!   `cfg_name()` 返回 `"mihomo/config.yaml"`（文件名段仍是字面 config.yaml，
//!   目录段实现子目录裁决；service.rs 写配置前对 cfg 父目录 create_dir_all）。
//! - **手写 YAML 禁新依赖**（模板法）：所有用户来源字符串恒过 [`yaml_scalar`]
//!   （serde_json 引号转义 = YAML 双引号标量合法子集，`\n`/引号/反斜杠全转义）
//!   → 注入红线由 `mihomoYamlScalarEscaping_injectionSafe` 钉死；
//!   规则行是 `TYPE,value,target` 逗号串，value 含逗号/控制符在引号内仍是
//!   畸形规则 → 渲染层白名单拒（`mihomoRule_commaPattern_rejected`）。
//! - **策略组真渲染**（policy_groups=true，与 xray 退化路相对）：
//!   `AUTO` url-test 组（url/interval 消费 IR 单一真源）+ `NexusForge` select 组
//!   （proxies=["AUTO",…节点]，09 行字面），规则/兜底指向组名，无 resolve_egress 决议。
//! - **external-controller 地基**：v1 恒 `external-controller: ""`（禁用）+
//!   `secret: ""` 留位——00-spec 本地端口门禁（默认关+回环+无凭据+自检），
//!   UI 开关与 delay/dns 查询 API 归 B7（`mihomoExternalControllerOff_byDefault`）。
//! - bind-address 消费 IR listen（默认回环），不把 mixed 端口暴露到局域网。
//! - geo：`geodata-mode/geo-auto-update` 渲染位随 T-B2-10；v1 私有地址走显式
//!   CIDR 规则展开（与 xray 方言同款零资产策略）。
//! - supported_kinds：本行 NodeKind 仅四变体可列；七协议全集（+hy2/tuic/wg）
//!   随 T-B2-7 枚举扩臂，ssr 支持集按 ⑭ 届时核证。

use crate::error::{ProxyError, Result};
use crate::ir::{
    IrConfig, IrInbound, IrOutbound, IrRule, IrRuleField, TAG_AUTO, TAG_BLOCK, TAG_DIRECT,
    TAG_PROXY, TUN_ADDRESS, TUN_INTERFACE_NAME,
};
use crate::kernel::{KernelCaps, KernelDriver};
use crate::sub::{Node, NodeKind};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 选定组显示名（09 行字面 "NexusForge"；UI 策略组入口归 B7）
pub const GROUP_PROXY: &str = "NexusForge";
/// url-test 自动优选组名（09 行字面 proxies:["AUTO",…]）
pub const GROUP_AUTO: &str = "AUTO";
/// mihomo 内建直连/拒绝出站关键字
const TARGET_DIRECT: &str = "DIRECT";
const TARGET_BLOCK: &str = "REJECT";

/// 私有地址段（与 xray.rs PRIVATE_CIDRS 同款判定集；含 v6 故分流 IP-CIDR/IP-CIDR6）
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

/// YAML 双引号标量：所有用户来源字符串的唯一出口（注入红线）。
/// JSON 转义集是 YAML 双引号风格的合法子集（serde_json 恒转义 `"`,`\`,控制符），
/// 因此任意换行/引号/冒号都被封成单行标量，不可能开新顶层键。
fn yaml_scalar(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// IR → mihomo config.yaml（渲染即校验；无法方言表达的构造 Err，禁 panic）
pub fn render(ir: &IrConfig) -> Result<String> {
    let (mixed_port, bind, tun, tun_addr, tun_iface) = match &ir.inbound {
        IrInbound::Mixed { listen, port } => (*port, listen.clone(), false, None, None),
        IrInbound::Tun {
            interface_name,
            address,
            ..
        } => (
            0,
            "127.0.0.1".to_string(),
            true,
            Some(address.clone()),
            Some(interface_name.clone()),
        ),
    };

    let mut out = String::new();
    out.push_str("# NexusForge mihomo config (generated from IR; edit in-app, not on disk)\n");
    out.push_str(&format!("mixed-port: {mixed_port}\n"));
    out.push_str(&format!("bind-address: {}\n", yaml_scalar(&bind)));
    out.push_str("mode: rule\n");
    out.push_str("log-level: warning\n");
    // external-controller 地基：v1 恒禁用 + secret 留位（00-spec 门禁，UI 归 B7）
    out.push_str("external-controller: \"\"\n");
    out.push_str("secret: \"\"\n");
    out.push_str("tun:\n");
    out.push_str(&format!("  enable: {}\n", tun));
    out.push_str("  stack: mixed\n");
    out.push_str(&format!(
        "  device-name: {}\n",
        yaml_scalar(tun_iface.as_deref().unwrap_or(TUN_INTERFACE_NAME))
    ));
    out.push_str("  addresses:\n");
    out.push_str(&format!(
        "  - {}\n",
        yaml_scalar(tun_addr.as_deref().unwrap_or(TUN_ADDRESS))
    ));

    // proxies：type 白名单展开（未知 kind 在 Rust 类型层不可能）
    out.push_str("proxies:\n");
    let mut node_names: Vec<String> = Vec::new();
    for ob in &ir.outbounds {
        if let IrOutbound::Node(n) = ob {
            node_names.push(n.outbound_tag());
            out.push_str("  - ");
            out.push_str(&render_proxy(n));
            out.push('\n');
        }
    }
    if node_names.is_empty() {
        return Err(ProxyError::Config("mihomo 渲染：IR 中无节点出站".into()));
    }

    // proxy-groups：AUTO(url-test) + NexusForge(select，AUTO 恒前项=旧行为兜底)
    let (auto_url, auto_interval) = ir
        .outbounds
        .iter()
        .find_map(|o| match o {
            IrOutbound::Urltest {
                url, interval_secs, ..
            } => Some((url.clone(), *interval_secs)),
            _ => None,
        })
        .unwrap_or((
            crate::ir::URLTEST_URL.to_string(),
            crate::ir::URLTEST_INTERVAL_SECS,
        ));
    out.push_str("proxy-groups:\n");
    out.push_str(&format!("  - name: {}\n", yaml_scalar(GROUP_AUTO)));
    out.push_str("    type: url-test\n");
    out.push_str("    proxies:\n");
    for n in &node_names {
        out.push_str(&format!("      - {}\n", yaml_scalar(n)));
    }
    out.push_str(&format!("    url: {}\n", yaml_scalar(&auto_url)));
    out.push_str(&format!("    interval: {auto_interval}\n"));
    out.push_str(&format!("  - name: {}\n", yaml_scalar(GROUP_PROXY)));
    out.push_str("    type: select\n");
    out.push_str("    proxies:\n");
    out.push_str(&format!("      - {}\n", yaml_scalar(GROUP_AUTO)));
    for n in &node_names {
        out.push_str(&format!("      - {}\n", yaml_scalar(n)));
    }
    let selector_default = ir.outbounds.iter().find_map(|o| match o {
        IrOutbound::Selector {
            tag, default_tag, ..
        } if tag == TAG_PROXY => default_tag.clone(),
        _ => None,
    });
    // 本 edition 无 let-chains（xray.rs E0670 教训）：先取 Option 再判成员
    if let Some(def) = &selector_default {
        if node_names.iter().any(|n| n == def) {
            out.push_str(&format!("    default: {}\n", yaml_scalar(def)));
        }
    }

    // rules：行=TYPE,value,target 逗号串整体经 yaml_scalar；value 白名单校验
    out.push_str("rules:\n");
    for r in &ir.route.rules {
        let target = resolve_target(&r.target, &node_names);
        for line in render_rule_lines(r, &target)? {
            out.push_str(&format!("  - {}\n", yaml_scalar(&line)));
        }
    }
    // 兜底：MATCH 行指向组名/内建关键字（block 兜底=REJECT 是 mihomo 内建，无需额外出站）
    let final_target = resolve_target(&ir.route.final_target, &node_names);
    out.push_str(&format!(
        "  - {}\n",
        yaml_scalar(&format!("MATCH,{final_target}"))
    ));
    Ok(out)
}

/// 规则/兜底 target → mihomo 组名/内建关键字（节点 tag 原样=组内成员名）
fn resolve_target(target: &str, node_names: &[String]) -> String {
    match target {
        TAG_PROXY => GROUP_PROXY.to_string(),
        TAG_AUTO => GROUP_AUTO.to_string(),
        TAG_DIRECT => TARGET_DIRECT.to_string(),
        TAG_BLOCK => TARGET_BLOCK.to_string(),
        other => node_names
            .iter()
            .find(|n| *n == other)
            .cloned()
            .unwrap_or_else(|| GROUP_PROXY.to_string()),
    }
}

/// 单条 IR 规则 → 零至多条 mihomo 规则行（IpIsPrivate 展开 CIDR 清单；
/// IpCidr 按族分 IP-CIDR/IP-CIDR6）。value 含逗号/控制符 → 渲染层拒（白名单）。
fn render_rule_lines(r: &IrRule, target: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut one = |kind: &str, value: &str| -> Result<()> {
        check_pattern(value)?;
        out.push(format!("{kind},{value},{target}"));
        Ok(())
    };
    match r.field {
        IrRuleField::IpIsPrivate => {
            for cidr in PRIVATE_CIDRS {
                one(
                    if cidr.contains(':') {
                        "IP-CIDR6"
                    } else {
                        "IP-CIDR"
                    },
                    cidr,
                )?;
            }
        }
        IrRuleField::Domain => {
            for p in &r.patterns {
                one("DOMAIN", p)?;
            }
        }
        IrRuleField::DomainSuffix => {
            for p in &r.patterns {
                one("DOMAIN-SUFFIX", p)?;
            }
        }
        IrRuleField::Keyword => {
            for p in &r.patterns {
                one("DOMAIN-KEYWORD", p)?;
            }
        }
        IrRuleField::IpCidr => {
            for p in &r.patterns {
                one(
                    if p.contains(':') {
                        "IP-CIDR6"
                    } else {
                        "IP-CIDR"
                    },
                    p,
                )?;
            }
        }
    }
    Ok(out)
}

/// 规则 value 白名单：逗号会破坏 TYPE,value,target 段结构，控制符是投毒面
fn check_pattern(p: &str) -> Result<()> {
    if p.contains(',') || p.chars().any(|c| c.is_control()) {
        return Err(ProxyError::Config(format!(
            "mihomo 规则值含逗号或控制字符（会破坏规则段结构）：{p:?}"
        )));
    }
    Ok(())
}

/// 单节点 → mihomo proxy 流式映射（"key: value, key: value"，值全 yaml_scalar）
fn render_proxy(n: &Node) -> String {
    let mut kv: Vec<String> = vec![
        format!("name: {}", yaml_scalar(&n.outbound_tag())),
        format!("server: {}", yaml_scalar(&n.server)),
        format!("port: {}", n.port),
        // UDP 显式开：与 sing-box mixed 入站 udp:true 语义对齐（方言默认不承诺）
        "udp: true".into(),
    ];
    match n.kind {
        NodeKind::Shadowsocks => {
            kv.push("type: ss".into());
            kv.push(format!("cipher: {}", yaml_scalar_str(&n.extra["method"])));
            kv.push(format!(
                "password: {}",
                yaml_scalar_str(&n.extra["password"])
            ));
        }
        NodeKind::Vmess => {
            kv.push("type: vmess".into());
            kv.push(format!("uuid: {}", yaml_scalar_str(&n.extra["uuid"])));
            kv.push(format!(
                "alterId: {}",
                n.extra["alter_id"].as_u64().unwrap_or(0)
            ));
            kv.push(format!(
                "cipher: {}",
                yaml_scalar(n.extra["security"].as_str().unwrap_or("auto"))
            ));
        }
        NodeKind::Trojan => {
            kv.push("type: trojan".into());
            kv.push(format!(
                "password: {}",
                yaml_scalar_str(&n.extra["password"])
            ));
        }
        NodeKind::Vless => {
            kv.push("type: vless".into());
            kv.push(format!("uuid: {}", yaml_scalar_str(&n.extra["uuid"])));
            if let Some(flow) = n.extra["flow"].as_str() {
                if !flow.is_empty() {
                    kv.push(format!("flow: {}", yaml_scalar(flow)));
                }
            }
        }
    }
    // tls/sni/ws 通用位（Meta 支持 sni+servername 双写、ws-opts 用扁平 ws-path）
    if n.extra["tls"] == serde_json::json!(true) {
        kv.push("tls: true".into());
        let sni = n.extra["sni"].as_str().unwrap_or_default();
        if !sni.is_empty() {
            kv.push(format!("servername: {}", yaml_scalar(sni)));
        }
    }
    if n.extra["network"].as_str() == Some("ws") {
        kv.push("network: ws".into());
        kv.push(format!(
            "ws-path: {}",
            yaml_scalar(n.extra["path"].as_str().unwrap_or("/"))
        ));
    }
    kv.join(", ")
}

/// JSON Value → 标量文本（缺键/Null → 空串，与 sing-box 方言的 json![] 投影同型兜底）
fn yaml_scalar_str(v: &serde_json::Value) -> String {
    yaml_scalar(v.as_str().unwrap_or_default())
}

/// mihomo 内核驱动（T-B2-6 注册表第三臂）。exe 由 T-B2-4 资产表通道安装到 bin/。
pub struct MihomoDriver {
    exe: PathBuf,
}

impl MihomoDriver {
    pub fn new(exe: PathBuf) -> Self {
        Self { exe }
    }
}

impl KernelDriver for MihomoDriver {
    fn id(&self) -> &'static str {
        "mihomo"
    }

    fn exe_path(&self) -> PathBuf {
        self.exe.clone()
    }

    fn display_name(&self) -> &'static str {
        "mihomo (Clash 内核)"
    }

    /// `mihomo -d` 目录语义（09 ④）：config.yaml 住 proxy/mihomo/ 子目录，
    /// Country.mmdb 同栖；文件名段仍是任务书字面 config.yaml
    fn cfg_name(&self) -> &'static str {
        "mihomo/config.yaml"
    }

    fn supported_kinds(&self) -> &'static [NodeKind] {
        use NodeKind::*;
        &[Shadowsocks, Vmess, Trojan, Vless]
    }

    fn caps(&self) -> KernelCaps {
        KernelCaps {
            tun: true,
            policy_groups: true,
            external_controller: true,
        }
    }

    fn config_render(&self, ir: &IrConfig) -> Result<String> {
        render(ir)
    }

    /// `-d <config 所在目录>`（与 sing-box/xray 的 `-c file` 语义分野；
    /// work_dir 参数是 proxy 根，mihomo 的工作目录必须是配置的父目录）
    fn build_command(&self, _work_dir: &Path, cfg: &Path) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.arg("-d").arg(cfg.parent().unwrap_or(_work_dir));
        cmd
    }
}

#[cfg(test)]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
mod tests {
    use super::*;
    use crate::ir::{self, IrRule};
    use serde_json::json;

    fn nodes() -> Vec<Node> {
        vec![
            Node {
                tag: "hk-1".into(),
                kind: NodeKind::Shadowsocks,
                server: "1.2.3.4".into(),
                port: 8388,
                sub_id: "sub0aaaabbbbcccc".into(),
                extra: json!({ "method": "aes-256-gcm", "password": "p1" }),
            },
            Node {
                tag: "tokyo-2".into(),
                kind: NodeKind::Trojan,
                server: "example.com".into(),
                port: 443,
                sub_id: "sub0aaaabbbbcccc".into(),
                extra: json!({ "password": "p2", "sni": "example.com", "tls": true }),
            },
        ]
    }

    fn mixed_ir() -> IrConfig {
        ir::build(7890, false, &nodes(), &[], TAG_PROXY, &[]).unwrap()
    }

    /// 骨架顶层键全集（新键出现=注入开洞或方言面扩张，测试即门）
    const TOP_KEYS: &[&str] = &[
        "mixed-port:",
        "bind-address:",
        "mode:",
        "log-level:",
        "external-controller:",
        "secret:",
        "tun:",
        "proxies:",
        "proxy-groups:",
        "rules:",
    ];

    fn assert_no_unexpected_top_key(y: &str) {
        for line in y.lines() {
            if let Some(head) = line.split(':').next() {
                let key = format!("{}:", head);
                if !line.starts_with(' ')
                    && !line.starts_with('#')
                    && !line.starts_with("- ")
                    && !line.starts_with("  ")
                {
                    assert!(
                        TOP_KEYS.contains(&key.as_str()),
                        "出现计划外顶层键 {key:?}（疑似注入开洞）：\n{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn mihomoYamlScalarEscaping_injectionSafe() {
        // 红线：tag 携带 YAML 投毒串——节点名/组列表/规则 target 全链路后，
        // 文档顶层键集合不变、不产生裸新键、注入内容只存在于双引号标量内部
        let mut poison = nodes();
        poison[0].tag = "hk\nrules:\n- JS:evil\nproxies:\n- name: x".into();
        let cfg = ir::build(7890, false, &poison, &[], TAG_PROXY, &[]).unwrap();
        let y = render(&cfg).unwrap();
        assert_no_unexpected_top_key(&y);
        // 注入串必须整体处于引号内且换行已转义（outbound_tag 前缀 sub0aaaa: 在串首）
        assert!(y.contains("\"sub0aaaa:hk\\nrules:\\n- JS:evil\\nproxies:\\n- name: x\""));
        assert!(
            !y.contains("\nrules:\n- JS"),
            "投毒串不得以裸键形态出现在文档中"
        );
        // 顶层键计数恒定（rules: 只有一枚——注入的第二份被转义封死）
        assert_eq!(y.match_indices("\nrules:").count(), 1);
        assert_eq!(y.match_indices("\nproxies:").count(), 1);
    }

    #[test]
    fn mihomoUrltestGroupRendered() {
        let y = render(&mixed_ir()).unwrap();
        assert!(y.contains(&format!("  - name: \"{GROUP_AUTO}\"\n    type: url-test")));
        assert!(y.contains("    type: select"));
        // NexusForge 首成员 = AUTO（旧行为兜底：未选即自动优选）
        let group_sel = y.find("    type: select").expect("select 组必须渲染");
        let after = &y[group_sel..];
        assert!(
            after.contains(&format!("      - \"{GROUP_AUTO}\"")),
            "select 组 proxies 首项必须是 AUTO"
        );
        assert!(y.contains("    url: \"https://www.gstatic.com/generate_204\""));
        assert!(y.contains("    interval: 300"));
    }

    #[test]
    fn mihomoExternalControllerOff_byDefault() {
        let y = render(&mixed_ir()).unwrap();
        assert!(
            y.contains("external-controller: \"\""),
            "地基：控制器恒禁用"
        );
        assert!(y.contains("secret: \"\""), "secret 留位同空");
        for banned in ["9090", "127.0.0.1:9", "external-ui"] {
            assert!(
                !y.contains(banned),
                "默认输出不得含可用监听地址/面板字样 {banned}"
            );
        }
        // 对照：mixed 端口是本地代理入站（非控制通道），回环绑定
        assert!(y.contains("mixed-port: 7890"));
        assert!(y.contains("bind-address: \"127.0.0.1\""));
    }

    #[test]
    fn mihomoRule_commaPattern_rejected() {
        // 规则 value 含逗号 → 段结构破坏，渲染层点名拒（yaml_scalar 之外第二道闸）
        let bad = vec![IrRule {
            field: IrRuleField::DomainSuffix,
            patterns: vec!["ok.com,MATCH,REJECT".into()],
            target: TAG_DIRECT.into(),
        }];
        let cfg = ir::build(7890, false, &nodes(), &bad, TAG_PROXY, &[]).unwrap();
        match render(&cfg) {
            Err(ProxyError::Config(msg)) => {
                assert!(
                    msg.contains("ok.com,MATCH,REJECT"),
                    "错误须点名违规值：{msg}"
                )
            }
            other => panic!("逗号投毒必须拒，得 {other:?}"),
        }
    }

    #[test]
    fn mihomoDialect_tunInboundAndGroups() {
        // tun 入站：enable true + mixed-port 0（无本地入站）；sing-box/xray 之对立面
        let cfg = ir::build(7890, true, &nodes(), &[], TAG_PROXY, &[]).unwrap();
        let y = render(&cfg).unwrap();
        assert!(y.contains("  enable: true"));
        assert!(y.contains("mixed-port: 0"));
        assert!(y.contains("device-name: \"NexusForge0\""));
        assert!(y.contains("  - \"172.19.0.1/30\""));
        // 兜底 MATCH 指向策略组而非决议单点（policy_groups=true 兑现）
        assert!(y.contains(&format!("\"MATCH,{GROUP_PROXY}\"")));
        // ip_is_private → 显式 CIDR 展开（零资产），v6 走 IP-CIDR6
        assert!(y.contains("\"IP-CIDR,10.0.0.0/8,DIRECT\""));
        assert!(y.contains("\"IP-CIDR6,fc00::/7,DIRECT\""));
        assert!(!y.contains("GEO-"), "v1 不引用 geo 资产（T-B2-10 承诺面）");
    }

    #[test]
    fn mihomoDriver_registryShape_cfgWorkdir() {
        let d = MihomoDriver::new(PathBuf::from("mihomo.exe"));
        assert_eq!(d.id(), "mihomo");
        assert_eq!(d.cfg_name(), "mihomo/config.yaml");
        let caps = d.caps();
        assert!(
            caps.tun && caps.policy_groups && caps.external_controller,
            "mihomo 能力表全开（09 §5.2 行字面）"
        );
        // build_command：-d 指 config 父目录（workdir 语义核证），非 -c
        let cmd = d.build_command(
            Path::new("ignored"),
            Path::new("C:\\appdata\\proxy\\mihomo\\config.yaml"),
        );
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["-d", "C:\\appdata\\proxy\\mihomo"]);
    }
}
