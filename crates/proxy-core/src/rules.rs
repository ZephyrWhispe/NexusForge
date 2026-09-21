//! B2 T-B2-9（09 §5.2）：分流规则 v2 —— 三目标规则表数据模型 + 自研校验 + IR 投影。
//!
//! 校验是唯一外部输入闸口（[`sanitize`] 由 `ProxyService::set_rules_v2` 全量入口消费），
//! 渲染层的 mihomo `check_pattern` 只是第二道防线。纪律：CIDR 为自研最小 parse
//! （IPv4 四段 ≤255 + IPv6 冒号分组/`::` 压缩，不引新依赖）；domain 类走字符集
//! 白名单；process 归一化为 basename（拾取器 UI 归 B7，规则行=手输进程名）。

use serde::{Deserialize, Serialize};

use crate::error::{ProxyError, Result};
use crate::ir::{IrRule, IrRuleField, TAG_BLOCK, TAG_DIRECT, TAG_PROXY};

/// 规则类型白名单（02§7.1 表行下拉的单一真源）
pub const RULE_KINDS: [&str; 5] = ["domain", "suffix", "keyword", "ip_cidr", "process"];
/// 规则/兜底目标白名单
pub const RULE_TARGETS: [&str; 3] = ["direct", "proxy", "block"];
/// 分流模式（global=全部走代理；rule=规则分流；direct_all=透明兜底直连档）
pub const ROUTE_MODES: [&str; 3] = ["global", "rule", "direct_all"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleV2 {
    /// [`RULE_KINDS`] 之一（sanitize 保证）
    pub kind: String,
    pub pattern: String,
    /// [`RULE_TARGETS`] 之一（sanitize 保证）
    pub target: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesV2 {
    pub rules: Vec<RuleV2>,
    /// 兜底出口："proxy" | "direct" | "block"
    #[serde(default = "default_final")]
    pub final_target: String,
    /// 全局模式："global" | "rule" | "direct_all"
    #[serde(default = "default_route_mode")]
    pub route_mode: String,
}

fn default_final() -> String {
    "proxy".to_string()
}

fn default_route_mode() -> String {
    "rule".to_string()
}

/// 旧 `rules.json`（直连域名清单）→ v2 派生（`open()` 缺文件路径；零迁移，
/// 首次保存才落 rules_v2.json——旧命令 proxy_set_direct_rules 亦会物化）。
pub fn derive_legacy(direct_domains: &[String]) -> RulesV2 {
    RulesV2 {
        rules: direct_domains
            .iter()
            .map(|d| RuleV2 {
                kind: "suffix".to_string(),
                pattern: d.clone(),
                target: "direct".to_string(),
                enabled: true,
            })
            .collect(),
        final_target: default_final(),
        route_mode: default_route_mode(),
    }
}

/// 校验 + 归一（trim、process 去路径）。违规 → [`ProxyError::Config`] 点名违规字段值。
pub fn sanitize(v2: &RulesV2) -> Result<RulesV2> {
    if !ROUTE_MODES.contains(&v2.route_mode.as_str()) {
        return Err(ProxyError::Config(format!(
            "分流模式非法：{:?}（合法值 global/rule/direct_all）",
            v2.route_mode
        )));
    }
    check_oneof(&v2.final_target, "兜底目标")?;
    let mut rules = Vec::with_capacity(v2.rules.len());
    for r in &v2.rules {
        if !RULE_KINDS.contains(&r.kind.as_str()) {
            return Err(ProxyError::Config(format!(
                "规则类型非法：{:?}（合法值 domain/suffix/keyword/ip_cidr/process）",
                r.kind
            )));
        }
        check_oneof(&r.target, "规则目标")?;
        let pattern = r.pattern.trim().to_string();
        if pattern.is_empty() {
            return Err(ProxyError::Config("规则值不得为空".into()));
        }
        let pattern = match r.kind.as_str() {
            "domain" | "suffix" => {
                check_domain_shape(&pattern, &r.kind)?;
                pattern
            }
            "keyword" => {
                if pattern
                    .chars()
                    .any(|c| c.is_control() || c == ',' || c == ';')
                {
                    return Err(ProxyError::Config(format!(
                        "keyword 规则值含控制字符或分隔符（会破坏规则行结构）：{pattern:?}"
                    )));
                }
                pattern
            }
            "ip_cidr" => {
                if !cidr_ok(&pattern) {
                    return Err(ProxyError::Config(format!(
                        "ip_cidr 规则值不是合法 IPv4/IPv6 前缀：{pattern:?}"
                    )));
                }
                pattern
            }
            _ => {
                // process：去路径取 basename（手输进程名的归一化）
                let base = pattern
                    .rsplit_once(['/', '\\'])
                    .map_or(pattern.as_str(), |(_, b)| b);
                if base.is_empty() || base.chars().any(char::is_control) {
                    return Err(ProxyError::Config(format!(
                        "process 规则值去路径后为空或含控制字符：{pattern:?}"
                    )));
                }
                base.to_string()
            }
        };
        rules.push(RuleV2 {
            kind: r.kind.clone(),
            pattern,
            target: r.target.clone(),
            enabled: r.enabled,
        });
    }
    Ok(RulesV2 {
        rules,
        final_target: v2.final_target.clone(),
        route_mode: v2.route_mode.clone(),
    })
}

fn check_oneof(value: &str, what: &str) -> Result<()> {
    if RULE_TARGETS.contains(&value) {
        return Ok(());
    }
    Err(ProxyError::Config(format!(
        "{what}非法：{value:?}（合法值 direct/proxy/block）"
    )))
}

/// domain/suffix 形态字符集白名单：字母数字 . - _（suffix 允许旧清单的泛域名前导点）；
/// 空格/逗号/分号/控制符皆为投毒或破格形态。
fn check_domain_shape(p: &str, kind: &str) -> Result<()> {
    if p.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Ok(());
    }
    Err(ProxyError::Config(format!(
        "{kind} 规则值含非法字符（只允许字母/数字/./-/_）：{p:?}"
    )))
}

/// 自研最小 CIDR 校验（无新依赖红线）：`addr/prefix`，IPv4 四段各 ≤255 + prefix≤32；
/// IPv6 冒号分组（1-4 hex）+ 至多一处 `::` 压缩 + prefix≤128。
/// 不承诺 IPv4-mapped IPv6 尾段（`::ffff:1.2.3.4/120`）——内核侧亦按纯 v6 文本消费，够用。
pub fn cidr_ok(s: &str) -> bool {
    let Some((addr, prefix)) = s.split_once('/') else {
        return false;
    };
    if prefix.is_empty() || prefix.len() > 3 || !prefix.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let bits: u32 = prefix.parse().unwrap_or(u32::MAX);
    if addr.contains(':') {
        bits <= 128 && ipv6_ok(addr)
    } else {
        bits <= 32 && ipv4_ok(addr)
    }
}

fn ipv4_ok(a: &str) -> bool {
    let parts: Vec<&str> = a.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.chars().all(|c| c.is_ascii_digit())
                && p.parse::<u32>().is_ok_and(|v| v <= 255)
        })
}

fn ipv6_ok(a: &str) -> bool {
    let group = |g: &str| !g.is_empty() && g.len() <= 4 && g.chars().all(|c| c.is_ascii_hexdigit());
    let (head, tail) = match a.split_once("::") {
        Some((h, t)) => {
            if h.contains("::") || t.contains("::") {
                return false;
            }
            (h, Some(t))
        }
        None => (a, None),
    };
    let count = |s: &str| -> Option<usize> {
        if s.is_empty() {
            return Some(0);
        }
        s.split(':')
            .try_fold(0usize, |acc, g| if group(g) { Some(acc + 1) } else { None })
    };
    match tail {
        None => count(head).is_some_and(|n| n == 8),
        Some(t) => match (count(head), count(t)) {
            // 压缩至少替代一组：两侧显式组合至多 7（全 8 组不带 :: 是上臂形态）
            (Some(h), Some(r)) => h + r <= 7,
            _ => false,
        },
    }
}

/// v2 表 → IR 规则组 + 兜底 tag（`restart_with_config` 消费）。
/// 输入须先过 [`sanitize`]（唯一入口保证），未知串在此显式 Err 防旁路；
/// 启用规则按 (kind, target) 保序分桶，每桶折叠为一条 [`IrRule`]（patterns 聚合）。
pub fn to_ir(v2: &RulesV2) -> Result<(Vec<IrRule>, String)> {
    let final_tag = match v2.route_mode.as_str() {
        "global" => TAG_PROXY.to_string(),
        "direct_all" => TAG_DIRECT.to_string(),
        "rule" => match v2.final_target.as_str() {
            "proxy" => TAG_PROXY,
            "direct" => TAG_DIRECT,
            "block" => TAG_BLOCK,
            other => {
                return Err(ProxyError::Config(format!(
                    "兜底目标不可投影为出站 tag：{other:?}"
                )))
            }
        }
        .to_string(),
        other => return Err(ProxyError::Config(format!("分流模式不可投影：{other:?}"))),
    };
    if v2.route_mode != "rule" {
        // global/direct_all：用户规则全跳过（09 行字面），仅兜底出口变化
        return Ok((Vec::new(), final_tag));
    }
    let mut buckets: Vec<(IrRuleField, String, Vec<String>)> = Vec::new();
    for r in v2.rules.iter().filter(|r| r.enabled) {
        let field = match r.kind.as_str() {
            "domain" => IrRuleField::Domain,
            "suffix" => IrRuleField::DomainSuffix,
            "keyword" => IrRuleField::Keyword,
            "ip_cidr" => IrRuleField::IpCidr,
            "process" => IrRuleField::Process,
            other => {
                return Err(ProxyError::Config(format!(
                    "规则类型不可投影为 IR 字段：{other:?}"
                )))
            }
        };
        let tag = match r.target.as_str() {
            "direct" => TAG_DIRECT,
            "proxy" => TAG_PROXY,
            "block" => TAG_BLOCK,
            other => {
                return Err(ProxyError::Config(format!(
                    "规则目标不可投影为出站 tag：{other:?}"
                )))
            }
        }
        .to_string();
        match buckets
            .iter_mut()
            .find(|(f, t, _)| *f == field && *t == tag)
        {
            Some((_, _, patterns)) => patterns.push(r.pattern.clone()),
            None => buckets.push((field, tag, vec![r.pattern.clone()])),
        }
    }
    Ok((
        buckets
            .into_iter()
            .map(|(field, target, patterns)| IrRule {
                field,
                patterns,
                target,
            })
            .collect(),
        final_tag,
    ))
}

/// 「应用大陆直连预设」模板（geo 资产未装前的域名后缀清单，T-B2-10 后由 geosite 替代）。
/// 前端常量与本表同源核验由 preset 测试钉语义（去重合并），清单本体在此单一真源。
pub const MAINLAND_DIRECT_PRESET: &[&str] = &[
    "cn",
    "com.cn",
    "net.cn",
    "org.cn",
    "gov.cn",
    "edu.cn",
    "baidu.com",
    "qq.com",
    "weixin.qq.com",
    "163.com",
    "126.com",
    "bilibili.com",
    "taobao.com",
    "tmall.com",
    "jd.com",
    "douyin.com",
    "toutiao.com",
    "zhihu.com",
    "xiaohongshu.com",
    "meituan.com",
];

#[cfg(test)]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
mod tests {
    use super::*;
    use crate::ir;
    use crate::sub::{Node, NodeKind};
    use serde_json::json;

    fn rule(kind: &str, pattern: &str, target: &str) -> RuleV2 {
        RuleV2 {
            kind: kind.into(),
            pattern: pattern.into(),
            target: target.into(),
            enabled: true,
        }
    }

    fn one_node() -> Vec<Node> {
        vec![Node {
            tag: "a".into(),
            kind: NodeKind::Shadowsocks,
            server: "1.2.3.4".into(),
            port: 8388,
            sub_id: "sub1".into(),
            groups: Vec::new(),
            extra: json!({"method": "aes-256-gcm", "password": "p1"}),
        }]
    }

    #[test]
    fn rulesV2_sanitize_cidr_and_process() {
        assert!(cidr_ok("10.0.0.0/8"));
        assert!(cidr_ok("192.168.1.7/32"));
        assert!(cidr_ok("2001:db8::/32"));
        assert!(cidr_ok("fe80:0:0:0:0:0:0:1/128"));
        assert!(!cidr_ok("999.1.1.1/8"));
        assert!(!cidr_ok("10.0.0.0/33"));
        assert!(!cidr_ok("2001:db8::1::2/64"));
        assert!(!cidr_ok("a;b"));
        assert!(!cidr_ok("10.0.0/8"));
        // process 归一化：Windows 反斜杠路径与 POSIX 斜杠均取 basename
        let ok = sanitize(&RulesV2 {
            rules: vec![rule(
                "process",
                "C:\\Windows\\System32\\chrome.exe",
                "direct",
            )],
            final_target: "proxy".into(),
            route_mode: "rule".into(),
        })
        .unwrap();
        assert_eq!(ok.rules[0].pattern, "chrome.exe");
    }

    #[test]
    fn rulesV2_badDomain_rejected() {
        let bad = RulesV2 {
            rules: vec![rule("suffix", "bad domain;rm", "direct")],
            final_target: "proxy".into(),
            route_mode: "rule".into(),
        };
        let err = sanitize(&bad).unwrap_err();
        match err {
            ProxyError::Config(msg) => {
                assert!(msg.contains("bad domain;rm"), "须点名违规值：{msg}")
            }
            other => panic!("须为 Config 错，得 {other:?}"),
        }
    }

    #[test]
    fn rulesToIR_groupsByKindTargetAndHonorsRouteMode() {
        let v2 = RulesV2 {
            rules: vec![
                rule("suffix", "cn", "direct"),
                rule("domain", "a.com", "proxy"),
                rule("suffix", "bilibili.com", "direct"),
                RuleV2 {
                    enabled: false,
                    ..rule("keyword", "x", "block")
                },
            ],
            final_target: "block".into(),
            route_mode: "rule".into(),
        };
        let (rules, final_tag) = to_ir(&v2).unwrap();
        assert_eq!(final_tag, ir::TAG_BLOCK);
        assert_eq!(rules.len(), 2, "同 (kind,target) 折叠一桶");
        assert_eq!(rules[0].field, IrRuleField::DomainSuffix);
        assert_eq!(
            rules[0].patterns,
            vec!["cn".to_string(), "bilibili.com".to_string()]
        );
        let global = RulesV2 {
            rules: v2.rules.clone(),
            final_target: "proxy".into(),
            route_mode: "global".into(),
        };
        let (rules, final_tag) = to_ir(&global).unwrap();
        assert!(
            rules.is_empty() && final_tag == ir::TAG_PROXY,
            "global：规则全跳过"
        );
        let (_, final_tag) = to_ir(&RulesV2 {
            rules: Vec::new(),
            final_target: "proxy".into(),
            route_mode: "direct_all".into(),
        })
        .unwrap();
        assert_eq!(final_tag, ir::TAG_DIRECT);
    }

    /// 红线：block 出站仅在规则/final 引用 block 时注入，且三内核方言形状各自正确；
    /// 无 block 使用时三份渲染输出均不含 block 形态（防"永远黑洞"的静默扩容）。
    #[test]
    fn rulesBlock_outboundAddedOnlyWhenUsed() {
        let nodes = one_node();

        let render_all = |v2: &RulesV2| -> (String, String, String) {
            let (user_rules, final_tag) = to_ir(v2).unwrap();
            let cfg = ir::build(7890, false, &nodes, &user_rules, &final_tag, &[]).unwrap();
            (
                crate::singbox::render(&cfg).unwrap().to_string(),
                crate::xray::render(&cfg).unwrap().to_string(),
                crate::mihomo::render(&cfg).unwrap(),
            )
        };

        // 无 block：三渲染均不含 block/blackhole/REJECT 形态
        let plain = RulesV2 {
            rules: vec![rule("suffix", "cn", "direct")],
            final_target: "proxy".into(),
            route_mode: "rule".into(),
        };
        let (sb, xr, mh) = render_all(&plain);
        assert!(
            !sb.contains("\"block\""),
            "sing-box 不得混入 block 出站：{sb}"
        );
        assert!(!xr.contains("blackhole"), "xray 不得混入 blackhole：{xr}");
        assert!(!mh.contains("REJECT"), "mihomo 不得混入 REJECT：{mh}");

        // 有 block 规则：三方言各出自己的 block 形态
        let with_block = RulesV2 {
            rules: vec![rule("keyword", "ads", "block")],
            final_target: "proxy".into(),
            route_mode: "rule".into(),
        };
        let (sb, xr, mh) = render_all(&with_block);
        assert!(
            sb.contains(r#"{"tag":"block","type":"block"}"#),
            "sing-box block 出站形状：{sb}"
        );
        assert!(
            xr.contains(r#""protocol":"blackhole""#) && xr.contains(r#""tag":"block""#),
            "xray blackhole 形状：{xr}"
        );
        assert!(
            mh.contains("DOMAIN-KEYWORD,ads,REJECT"),
            "mihomo REJECT 行：{mh}"
        );
    }
}
