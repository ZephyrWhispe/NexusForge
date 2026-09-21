//! T-B2-8 Clash YAML 订阅白名单解析器（09 §5.2 防投毒红线批）。
//!
//! 订阅正文除传统 URI 多行/base64 外，大量机场直接下发 Clash 配置 YAML。
//! 外部正文 = 不可信输入，本模块的立场是**双层白名单 + 深度自防御**：
//! - **顶层键白名单**：只放行 `proxies` / `proxy-groups`；出现任何未知顶层键
//!   （`proxy-providers` / `rule-providers` / `use` / `prepend-*` / `script` /
//!   `js-*` / `geodata-loader`…即投毒/外部执行面）→ **整份拒绝**且错误消息点名
//!   offending keys。订阅正文不是内核配置，白名单外没有"宽容"的余地。
//! - **条目内字段白名单**：proxy 条目按 type 白名单展开已知字段，未知条目内键
//!   **宽容忽略**（各方言实现差异常态，非安全面）；type 不在支持表 → 跳过计数，
//!   全空时计数进错误提示（复用 sub.rs 的 honest-hint 通道）。
//! - **anchor/alias 炸弹自防御**：yaml-rust2 装载时把别名克隆展开为实体树，
//!   展开后嵌套深度 > [`MAX_DEPTH`] 即拒——不依赖 crate 默认行为（0.13 无深度
//!   选项），且深度检查先于白名单检查，保证炸弹样本以"深度"理由被拒而非巧合。
//!
//! **[收窄 v1 登记]**：订阅内嵌 proxy-groups **不驱动** IR 策略组（组驱动策略组
//! 归 B7 与外部控制 API 同批）；本行只把成员归属解析进 `Node.groups` 供 UI
//! 过滤列展示。

use yaml_rust2::{Yaml, YamlLoader};

use crate::error::{ProxyError, Result};
use crate::sub::{Node, NodeKind};

/// anchor/alias 展开后的嵌套深度上限（自防御红线，行字面 64）
const MAX_DEPTH: usize = 64;

/// 允许出现在订阅正文顶层的键（白名单外整份拒）
const TOP_WHITELIST: &[&str] = &["proxies", "proxy-groups"];

#[derive(Debug)]
pub enum GroupKind {
    Select,
    Urltest,
    Fallback,
}

/// 订阅内嵌 proxy-group（v1 仅记录成员归属，见模块头收窄登记）
#[derive(Debug)]
pub struct ClashGroup {
    pub name: String,
    pub kind: GroupKind,
    pub members: Vec<String>,
}

/// 是否按 Clash YAML 订阅处理：行首（零缩进）顶层键 `proxies:` / `proxy-groups:`
/// 探测，**先于** base64 启发式（`parse_subscription` 顶部挂分支）。
pub fn is_clash_yaml(content: &str) -> bool {
    content
        .lines()
        .any(|l| l.starts_with("proxies:") || l.starts_with("proxy-groups:"))
}

/// 深度守卫：装载后的（别名已展开）树上量测嵌套深度。递归深度受 MAX_DEPTH
/// 约束即栈安全；超限说明是深层 anchor 展开样本 → 拒。
fn within_depth(y: &Yaml, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match y {
        Yaml::Array(xs) => xs.iter().all(|x| within_depth(x, depth + 1)),
        Yaml::Hash(m) => m
            .iter()
            .all(|(k, v)| within_depth(k, depth + 1) && within_depth(v, depth + 1)),
        _ => true,
    }
}

fn str_field<'a>(m: &'a yaml_rust2::yaml::Hash, key: &str) -> Option<&'a str> {
    m.get(&Yaml::String(key.to_string())).and_then(Yaml::as_str)
}

/// 多候选键取字符串字段（方言别名：sni/servername 等同义场）
fn str_field_any<'a>(m: &'a yaml_rust2::yaml::Hash, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| str_field(m, k))
}

fn yaml_bool(y: &Yaml) -> bool {
    match y {
        Yaml::Boolean(b) => *b,
        // clash 方言里 tls 既写 `tls: true` 也写 `tls: tls`（旧版字符串开关）
        Yaml::String(s) => s == "true" || s == "tls",
        _ => false,
    }
}

fn bool_field(m: &yaml_rust2::yaml::Hash, key: &str) -> bool {
    m.get(&Yaml::String(key.to_string())).is_some_and(yaml_bool)
}

/// 数字字段：YAML 里既写 `alterId: 1` 也写 `alterId: "1"`（方言并存）
fn int_field(m: &yaml_rust2::yaml::Hash, key: &str) -> Option<u64> {
    match m.get(&Yaml::String(key.to_string())) {
        Some(Yaml::Integer(n)) => u64::try_from(*n).ok(),
        Some(Yaml::String(s)) => s.parse().ok(),
        _ => None,
    }
}

fn parse_port(y: &Yaml) -> Option<u16> {
    match y {
        Yaml::Integer(n) => u16::try_from(*n).ok(),
        Yaml::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// alpn：YAML 里是数组、IR extra 方言是逗号串 → 切齐到 URI 路的形状
fn alpn_field(m: &yaml_rust2::yaml::Hash) -> String {
    m.get(&Yaml::String("alpn".to_string()))
        .and_then(Yaml::as_vec)
        .map(|v| {
            v.iter()
                .filter_map(Yaml::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

/// 单个 proxy 条目 → 节点；`None` = 跳过（不支持类型/缺必填字段），
/// 由调用方计数（行字面：跳过不致命，计数进错误提示）。
fn entry_to_node(item: &Yaml, sub_id: &str) -> Option<Node> {
    let m = item.as_hash()?;
    let name = str_field(m, "name")?.to_string();
    let server = str_field(m, "server")?.to_string();
    let port = parse_port(m.get(&Yaml::String("port".to_string()))?)?;
    let type_name = str_field(m, "type")?.to_ascii_lowercase();
    // (kind, extra) 同臂产出：支持表与类型名的映射单点真源，防两处 match 漂移
    let (kind, extra) = match type_name.as_str() {
        "ss" => (
            NodeKind::Shadowsocks,
            serde_json::json!({
                "method": str_field(m, "cipher").unwrap_or("aes-256-gcm"),
                "password": str_field(m, "password").unwrap_or_default(),
            }),
        ),
        "vmess" => (
            NodeKind::Vmess,
            serde_json::json!({
                "uuid": str_field(m, "uuid").unwrap_or_default(),
                "alter_id": int_field(m, "alterId").unwrap_or(0),
                "security": str_field(m, "cipher").unwrap_or("auto"),
                "network": str_field(m, "network").unwrap_or("tcp"),
                "tls": bool_field(m, "tls"),
                "sni": str_field_any(m, &["sni", "servername"]).unwrap_or_default(),
            }),
        ),
        "trojan" => (
            NodeKind::Trojan,
            serde_json::json!({
                "password": str_field(m, "password").unwrap_or_default(),
                "sni": str_field_any(m, &["sni", "servername"]).unwrap_or_default(),
            }),
        ),
        "vless" => {
            let mut e = serde_json::json!({
                "uuid": str_field(m, "uuid").unwrap_or_default(),
                "sni": str_field_any(m, &["sni", "servername"]).unwrap_or_default(),
                "network": str_field(m, "network").unwrap_or("tcp"),
                "tls": bool_field(m, "tls"),
            });
            if let Some(flow) = str_field(m, "flow") {
                e["flow"] = serde_json::json!(flow);
            }
            // clash-meta reality 方言（reality-opts{public-key,short-id}）→
            // 白名单键 reality{public_key,short_id}，与 URI 路 parse_authority 同形
            if let Some(ro) = m
                .get(&Yaml::String("reality-opts".to_string()))
                .and_then(Yaml::as_hash)
            {
                e["tls"] = serde_json::json!(true);
                e["reality"] = serde_json::json!({
                    "public_key": str_field(ro, "public-key").unwrap_or_default(),
                    "short_id": str_field(ro, "short-id").unwrap_or_default(),
                });
            }
            (NodeKind::Vless, e)
        }
        "hysteria2" => {
            let mut e = serde_json::json!({
                "password": str_field(m, "password").unwrap_or_default(),
                "sni": str_field_any(m, &["sni", "prefix"]).unwrap_or_default(),
                "insecure": bool_field(m, "insecure"),
            });
            if let Some(obfs) = str_field(m, "obfs") {
                e["obfs"] = serde_json::json!(obfs);
                e["obfs_password"] =
                    serde_json::json!(str_field(m, "obfs-password").unwrap_or_default());
            }
            (NodeKind::Hysteria2, e)
        }
        "tuic" => {
            let password = str_field(m, "password").unwrap_or_default();
            let password = if password.is_empty() {
                str_field(m, "token").unwrap_or_default()
            } else {
                password
            };
            (
                NodeKind::Tuic5,
                serde_json::json!({
                    "uuid": str_field(m, "uuid").unwrap_or_default(),
                    "password": password,
                    "sni": str_field_any(m, &["sni", "servername"]).unwrap_or_default(),
                    "alpn": alpn_field(m),
                }),
            )
        }
        "wireguard" => (
            NodeKind::WireGuard,
            serde_json::json!({
                "private_key": str_field(m, "private-key").unwrap_or_default(),
                "public_key": str_field(m, "public-key").unwrap_or_default(),
                "preshared_key": str_field(m, "preshared-key").unwrap_or_default(),
                "address": str_field_any(m, &["ip", "address"]).unwrap_or_default(),
            }),
        ),
        // 白名单外 type（含 snell/hysteria-v1/ssr/direct/reject…）→ 跳过计数；
        // ssr 恒不进支持表与 ⑭ 核证（三内核官方不支持）一致
        _ => return None,
    };
    Some(Node {
        tag: name,
        kind,
        server,
        port,
        sub_id: sub_id.to_string(),
        groups: Vec::new(),
        extra,
    })
}

fn parse_groups(items: &[Yaml]) -> Vec<ClashGroup> {
    let mut out = Vec::new();
    for item in items {
        let Some(m) = item.as_hash() else { continue };
        let Some(name) = str_field(m, "name") else {
            continue;
        };
        let kind = match str_field(m, "type") {
            Some("select") => GroupKind::Select,
            Some("url-test") => GroupKind::Urltest,
            Some("fallback") => GroupKind::Fallback,
            // 组类型白名单外（load-balance/relay 等）→ 组跳过（不致命：
            // 成员归属信息缺失是 UI 过滤列的降级，不是安全事件）
            _ => continue,
        };
        let members = m
            .get(&Yaml::String("proxies".to_string()))
            .and_then(Yaml::as_vec)
            .map(|v| {
                v.iter()
                    .filter_map(Yaml::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        out.push(ClashGroup {
            name: name.to_string(),
            kind,
            members,
        });
    }
    out
}

/// Clash YAML 订阅正文 → （节点表，组表）。错误一律走 Subscription 通道。
pub fn parse_clash_subscription(yaml: &str, sub_id: &str) -> Result<(Vec<Node>, Vec<ClashGroup>)> {
    let docs = YamlLoader::load_from_str(yaml)
        .map_err(|e| ProxyError::Subscription(format!("Clash YAML 解析失败: {e}")))?;
    let doc = docs
        .first()
        .ok_or_else(|| ProxyError::Subscription("Clash YAML 为空文档".into()))?;
    // 深度守卫先于白名单：炸弹样本必须以"深度"理由被拒，理由要诚实
    if !within_depth(doc, 0) {
        return Err(ProxyError::Subscription(format!(
            "Clash YAML 嵌套深度超过上限 {MAX_DEPTH}：疑似 anchor/alias 展开炸弹，整份拒绝"
        )));
    }
    let map = doc
        .as_hash()
        .ok_or_else(|| ProxyError::Subscription("Clash YAML 顶层不是映射".into()))?;
    let offenders: Vec<String> = map
        .keys()
        .filter(|k| !k.as_str().is_some_and(|s| TOP_WHITELIST.contains(&s)))
        .map(|k| match k.as_str() {
            Some(s) => s.to_string(),
            None => "<非字符串顶层键>".to_string(),
        })
        .collect();
    if !offenders.is_empty() {
        return Err(ProxyError::Subscription(format!(
            "Clash YAML 含白名单外顶层键，整份拒绝: {}",
            offenders.join(", ")
        )));
    }
    let empty: Vec<Yaml> = Vec::new();
    let proxies = map
        .get(&Yaml::String("proxies".to_string()))
        .and_then(Yaml::as_vec)
        .unwrap_or(&empty);
    let groups_yaml = map
        .get(&Yaml::String("proxy-groups".to_string()))
        .and_then(Yaml::as_vec)
        .unwrap_or(&empty);

    let mut nodes = Vec::new();
    let mut skipped = 0usize;
    for item in proxies {
        match entry_to_node(item, sub_id) {
            Some(n) => nodes.push(n),
            None => skipped += 1,
        }
    }
    let groups = parse_groups(groups_yaml);
    // 成员归属：按节点 name(=tag) 匹配；组内引用另一策略组名是常态，无节点可配即忽略
    for g in &groups {
        for member in &g.members {
            if let Some(i) = nodes.iter().position(|n| &n.tag == member) {
                if !nodes[i].groups.iter().any(|x| x == &g.name) {
                    nodes[i].groups.push(g.name.clone());
                }
            }
        }
    }
    if nodes.is_empty() {
        let hint = if skipped > 0 {
            format!("（{skipped} 个不支持的节点类型已跳过）")
        } else {
            String::new()
        };
        return Err(ProxyError::Subscription(format!(
            "Clash YAML 订阅中未解析出可用节点{hint}"
        )));
    }
    Ok((nodes, groups))
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例

    use super::*;

    const SS_OK: &str = "- name: hk-ss\n  type: ss\n  server: 1.2.3.4\n  port: 8388\n  cipher: aes-256-gcm\n  password: p1\n";

    #[test]
    fn clashYaml_unknownTopKey_rejectedWhole() {
        // 红线：proxy-providers（外部拉取面）出现 → 整份拒 + 点名 offending key
        let poisoned = format!(
            "proxies:\n{SS_OK}proxy-providers:\n  auto:\n    url: http://evil.example/x\n    type: http\n"
        );
        let err = parse_clash_subscription(&poisoned, "sub0aaaa").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("proxy-providers"),
            "错误消息须点名违规键: {msg}"
        );
        // 正控：同一正文去掉毒键即正常解析（证明拒的是键不是文件）
        let clean = format!("proxies:\n{SS_OK}");
        assert_eq!(
            parse_clash_subscription(&clean, "sub0aaaa")
                .unwrap()
                .0
                .len(),
            1
        );
    }

    #[test]
    fn clashYaml_jsProviderField_rejected() {
        // 红线：js-* / use 系外部执行字段作顶层键 → 整份拒（js-* 在行字面 offending 清单）
        for key in ["js-proxy", "use", "prepend-rules"] {
            let poisoned = format!("proxies:\n{SS_OK}{key}: [evil]\n");
            let err = parse_clash_subscription(&poisoned, "sub0aaaa").unwrap_err();
            assert!(err.to_string().contains(key), "{key} 必须被点名拒绝: {err}");
        }
    }

    #[test]
    fn clashYaml_aliasBomb_rejected() {
        // 深层 anchor 展开样本：每层把前层实体再包一层 {inner: …}，
        // 70 层 > MAX_DEPTH=64 → 以"深度上限"理由拒（而非白名单巧合）
        let mut yaml =
            String::from("proxies:\n- &n0 {name: b, type: ss, server: s, port: 1, password: p}\n");
        for i in 1..70 {
            yaml.push_str(&format!("bomb{i}: &n{i}\n  inner: *n{}\n", i - 1));
        }
        let err = parse_clash_subscription(&yaml, "sub0aaaa").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("深度"), "炸弹样本须以深度理由被拒: {msg}");
        // 正控：浅展开（5 层、白名单内）不受深度守卫影响
        let shallow = format!("proxies:\n{SS_OK}");
        assert!(parse_clash_subscription(&shallow, "sub0aaaa").is_ok());
    }

    #[test]
    fn clashYaml_unsupportedTypeItem_skippedNotFatal() {
        // 正+计数：snell / hysteria(v1) 跳过不致命，ss/vless 正常出节点
        let yaml = "proxies:\n\
- {name: ok-ss, type: ss, server: a.com, port: 1, cipher: aes-256-gcm, password: p}\n\
- {name: snell-x, type: snell, server: a.com, port: 2}\n\
- {name: hy1-x, type: hysteria, server: a.com, port: 3}\n\
- {name: ok-vless, type: vless, server: a.com, port: 443, uuid: u1, network: ws}\n";
        let (nodes, _) = parse_clash_subscription(yaml, "sub0aaaa").unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].tag, "ok-ss");
        assert_eq!(nodes[1].kind, NodeKind::Vless);
        // 全不支持 → Err 且计数进提示（honest-hint 通道）
        let all_bad = "proxies:\n\
- {name: a, type: snell, server: s, port: 1}\n\
- {name: b, type: direct}\n";
        let err = parse_clash_subscription(all_bad, "sub0aaaa").unwrap_err();
        assert!(err.to_string().contains("2 个不支持"), "{err}");
    }

    #[test]
    fn clashGroup_membersParsedIntoNodeGroups() {
        let yaml = "proxies:\n\
- {name: hk1, type: ss, server: a.com, port: 1, cipher: aes-256-gcm, password: p}\n\
- {name: tok2, type: trojan, server: b.com, port: 443, password: p2, sni: b.com}\n\
proxy-groups:\n\
- {name: AUTO, type: url-test, proxies: [hk1, tok2, OTHER], url: http://www.gstatic.com/generate_204, interval: 300}\n\
- {name: 手选, type: select, proxies: [AUTO, hk1]}\n";
        let (nodes, groups) = parse_clash_subscription(yaml, "sub0aaaa").unwrap();
        assert_eq!(groups.len(), 2);
        assert!(matches!(groups[0].kind, GroupKind::Urltest));
        assert!(matches!(groups[1].kind, GroupKind::Select));
        assert_eq!(
            nodes[0].groups,
            vec!["AUTO".to_string(), "手选".to_string()]
        );
        // 组引用另一组名（手选→AUTO）不是节点，不产生成员归属
        assert_eq!(nodes[1].groups, vec!["AUTO".to_string()]);
    }
}
