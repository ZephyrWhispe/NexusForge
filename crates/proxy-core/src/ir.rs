//! B2 T-B2-1（09 §5.2）：内核无关配置 IR —— 多内核方言渲染的统一输入。
//!
//! 旧 `config::generate` 的 sing-box 投影整体搬入 [`crate::singbox::render`]；
//! xray/mihomo 方言（T-B2-5/6）消费同一 [`IrConfig`]，禁再读 [`crate::sub::Node`] 细节。
//! 纪律：[`IrRuleField`]/[`IrOutbound`] 新增变体 = 编译期强制全渲染臂同步
//! （各方言 match 穷尽、禁 `_`），能力不支持的方言由渲染器返回 Err 表达。

use std::collections::HashSet;

use crate::error::{ProxyError, Result};
use crate::sub::Node;

/// 手动选定组（渲染器可注入 default）
pub const TAG_PROXY: &str = "proxy";
/// 自动优选组
pub const TAG_AUTO: &str = "auto";
/// 直连出站
pub const TAG_DIRECT: &str = "direct";
/// 拒绝出站（仅当规则引用时由 build 注入，T-B2-9）
pub const TAG_BLOCK: &str = "block";

/// urltest 探测端点（sing-box/mihomo 通用；xray 无 policy group，不消费）
pub const URLTEST_URL: &str = "https://www.gstatic.com/generate_204";
/// urltest 探测间隔（秒）
pub const URLTEST_INTERVAL_SECS: u64 = 300;

/// TUN 入站默认参数（内核子面板设置位归后续行）
pub const TUN_INTERFACE_NAME: &str = "NexusForge0";
pub const TUN_ADDRESS: &str = "172.19.0.1/30";

/// 远端 DNS 默认值（DNS 设置页归后续行）
pub const DNS_REMOTE_DEFAULT: &str = "https://1.1.1.1/dns-query";
pub const DNS_LOCAL_DEFAULT: &str = "local";
pub const DNS_STRATEGY_DEFAULT: &str = "prefer_ipv4";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IrInbound {
    Mixed {
        listen: String,
        port: u16,
    },
    Tun {
        interface_name: String,
        address: String,
        strict_route: bool,
    },
}

#[derive(Clone, Debug)]
pub enum IrOutbound {
    /// 单节点出站：方言渲染器按 kind 投影（未知 kind = 渲染 Err，禁 panic）
    Node(Node),
    Direct,
    Block,
    /// 手动选定组；default_tag=None 时方言自选（旧行为=首个成员）
    Selector {
        tag: String,
        outbounds: Vec<String>,
        default_tag: Option<String>,
    },
    /// 自动优选组
    Urltest {
        tag: String,
        outbounds: Vec<String>,
        url: String,
        interval_secs: u64,
    },
}

/// 规则匹配字段（GeoSite/GeoIp 由 T-B2-10 扩：变体新增=编译期强制全渲染臂同步）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrRuleField {
    IpIsPrivate,
    Domain,
    DomainSuffix,
    Keyword,
    IpCidr,
    /// T-B2-9 分应用代理数据面：进程名（basename，无路径）；
    /// 仅 Tun 入站可归因进程，System 态方言照常渲染但不命中（拾取器 UI 归 B7）
    Process,
}

#[derive(Clone, Debug)]
pub struct IrRule {
    pub field: IrRuleField,
    /// IpIsPrivate 恒空；其余字段至少一条 pattern
    pub patterns: Vec<String>,
    /// 出站 tag（须能在 outbounds 中解析，build 校验）
    pub target: String,
}

#[derive(Clone, Debug)]
pub struct IrRoute {
    pub rules: Vec<IrRule>,
    pub final_target: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrDns {
    pub remote: String,
    pub local: String,
    pub strategy: String,
}

#[derive(Clone, Debug)]
pub struct IrConfig {
    pub inbound: IrInbound,
    pub outbounds: Vec<IrOutbound>,
    pub route: IrRoute,
    pub dns: IrDns,
}

/// 组装 IR（语义等价搬移自旧 generate）：空节点拒、selector+auto+节点+direct、
/// ip_is_private 种子规则恒首、legacy 直连域名后缀表恒尾、目标 tag 全量校验。
pub fn build(
    mixed_port: u16,
    tun: bool,
    nodes: &[Node],
    user_rules: &[IrRule],
    final_target: &str,
    direct_domains: &[String],
) -> Result<IrConfig> {
    if nodes.is_empty() {
        return Err(ProxyError::Config("无可用节点：请先添加并更新订阅".into()));
    }
    let node_tags: Vec<String> = nodes.iter().map(Node::outbound_tag).collect();

    let mut selector_outbounds = node_tags.clone();
    selector_outbounds.push(TAG_AUTO.to_string());
    let mut outbounds = vec![
        IrOutbound::Selector {
            tag: TAG_PROXY.to_string(),
            outbounds: selector_outbounds,
            default_tag: None,
        },
        IrOutbound::Urltest {
            tag: TAG_AUTO.to_string(),
            outbounds: node_tags.clone(),
            url: URLTEST_URL.to_string(),
            interval_secs: URLTEST_INTERVAL_SECS,
        },
    ];
    outbounds.extend(nodes.iter().cloned().map(IrOutbound::Node));

    let mut rules = vec![IrRule {
        field: IrRuleField::IpIsPrivate,
        patterns: Vec::new(),
        target: TAG_DIRECT.to_string(),
    }];
    rules.extend(user_rules.iter().cloned());
    if !direct_domains.is_empty() {
        rules.push(IrRule {
            field: IrRuleField::DomainSuffix,
            patterns: direct_domains.to_vec(),
            target: TAG_DIRECT.to_string(),
        });
    }

    // block 出站仅在被引用时注入（final 或任一规则指向 TAG_BLOCK）——禁悬空 tag 引用
    let uses_block = final_target == TAG_BLOCK || rules.iter().any(|r| r.target == TAG_BLOCK);
    if uses_block {
        outbounds.push(IrOutbound::Block);
    }
    outbounds.push(IrOutbound::Direct);

    let mut tags: HashSet<&str> = HashSet::new();
    tags.insert(TAG_PROXY);
    tags.insert(TAG_AUTO);
    tags.insert(TAG_DIRECT);
    if uses_block {
        tags.insert(TAG_BLOCK);
    }
    tags.extend(node_tags.iter().map(String::as_str));
    check_tag(final_target, &tags, "路由兜底")?;
    for (i, r) in user_rules.iter().enumerate() {
        check_tag(&r.target, &tags, &format!("规则 #{i}"))?;
    }

    let inbound = if tun {
        IrInbound::Tun {
            interface_name: TUN_INTERFACE_NAME.to_string(),
            address: TUN_ADDRESS.to_string(),
            strict_route: true,
        }
    } else {
        IrInbound::Mixed {
            listen: "127.0.0.1".to_string(),
            port: mixed_port,
        }
    };
    Ok(IrConfig {
        inbound,
        outbounds,
        route: IrRoute {
            rules,
            final_target: final_target.to_string(),
        },
        dns: IrDns {
            remote: DNS_REMOTE_DEFAULT.to_string(),
            local: DNS_LOCAL_DEFAULT.to_string(),
            strategy: DNS_STRATEGY_DEFAULT.to_string(),
        },
    })
}

fn check_tag(target: &str, tags: &HashSet<&str>, what: &str) -> Result<()> {
    if tags.contains(target) {
        return Ok(());
    }
    Err(ProxyError::Config(format!(
        "{what}指向不存在的出站 tag：{target}"
    )))
}

#[cfg(test)]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
mod tests {
    use super::*;
    use crate::sub::NodeKind;

    fn one_node() -> Vec<Node> {
        vec![Node {
            tag: "a".into(),
            kind: NodeKind::Shadowsocks,
            server: "1.2.3.4".into(),
            port: 8388,
            sub_id: "sub1".into(),
            groups: Vec::new(),
            extra: serde_json::json!({"method": "aes-256-gcm", "password": "p1"}),
        }]
    }

    #[test]
    fn irBuild_emptyNodes_rejected() {
        let err = build(7890, false, &[], &[], TAG_PROXY, &[]);
        assert!(matches!(err, Err(ProxyError::Config(_))), "{err:?}");
    }

    #[test]
    fn irRouteFinalUnknownTag_rejected() {
        let nodes = one_node();
        let err = build(7890, false, &nodes, &[], "nonexistent-outbound", &[]);
        match err {
            Err(ProxyError::Config(msg)) => {
                assert!(
                    msg.contains("nonexistent-outbound"),
                    "错误须点名未知 tag：{msg}"
                );
            }
            other => panic!("未知兜底 tag 必须拒绝，得 {other:?}"),
        }
        // 正对照：合法 final（节点 tag / auto / block）不炸
        assert!(build(7890, false, &nodes, &[], TAG_AUTO, &[]).is_ok());
        assert!(build(7890, false, &nodes, &[], "sub1:a", &[]).is_ok());
        assert!(build(7890, false, &nodes, &[], TAG_BLOCK, &[]).is_ok());
    }

    #[test]
    fn irBuild_userRuleUnknownTarget_rejected() {
        let nodes = one_node();
        let bad = vec![IrRule {
            field: IrRuleField::Domain,
            patterns: vec!["example.com".into()],
            target: "ghost".into(),
        }];
        assert!(matches!(
            build(7890, false, &nodes, &bad, TAG_PROXY, &[]),
            Err(ProxyError::Config(_))
        ));
    }
}
