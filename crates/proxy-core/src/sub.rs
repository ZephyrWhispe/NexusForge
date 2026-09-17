//! PR3 订阅与节点解析（docs/impl/05 PR3 前半）：通用分享链接 URI → 节点。
//!
//! 合规红线：**只做解析框架，不内置任何节点/订阅**；订阅 URL 全部由用户添加，
//! 内容视为敏感（不落明文日志，仅持久化到 `{appData}/proxy/subs/`）。
//! 支持：ss://（SIP002 + 旧版整体 base64）、vmess://（v2ray JSON）、trojan://、vless://。
//! 订阅正文：整体 base64 或纯文本多行 URI，自动探测。

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::error::{ProxyError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Shadowsocks,
    Vmess,
    Trojan,
    Vless,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Shadowsocks => "shadowsocks",
            Self::Vmess => "vmess",
            Self::Trojan => "trojan",
            Self::Vless => "vless",
        }
    }
}

/// 节点（订阅条目投影；`extra` 存放协议细节字段，供 config.rs 生成出站）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub tag: String,
    pub kind: NodeKind,
    pub server: String,
    pub port: u16,
    pub sub_id: String,
    /// 协议细节（method/password/uuid/sni/flow...），生成出站时展开
    pub extra: serde_json::Value,
}

impl Node {
    /// 出站 tag：`{sub_id短8位}:{tag}` 避免跨订阅重名
    pub fn outbound_tag(&self) -> String {
        let sub = &self.sub_id;
        let sub = if sub.len() > 8 { &sub[..8] } else { sub };
        format!("{sub}:{}", self.tag)
    }
}

/// 解析订阅正文 → 节点列表。整体 base64 或纯文本多行 URI 自动探测。
pub fn parse_subscription(content: &str, sub_id: &str) -> Result<Vec<Node>> {
    let text = detect_and_decode(content);
    let mut nodes = Vec::new();
    let mut unknown = 0usize;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match parse_share_uri(line, sub_id) {
            Ok(n) => nodes.push(n),
            Err(_) => unknown += 1, // 未知协议：跳过不致命（订阅可含广告占位行）
        }
    }
    if nodes.is_empty() {
        let hint = if unknown > 0 {
            format!("（{unknown} 行为不支持的协议已跳过）")
        } else {
            String::new()
        };
        return Err(ProxyError::Subscription(format!(
            "订阅中未解析出可用节点{hint}"
        )));
    }
    Ok(nodes)
}

/// 整体 base64 探测：解码后仍是 URI 行则用解码结果，否则按原文处理
fn detect_and_decode(content: &str) -> String {
    let trimmed = content.trim();
    let b64 = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=');
    if !b64 || trimmed.is_empty() {
        return trimmed.to_string();
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .ok()
        .and_then(|b| String::from_utf8(b).ok());
    match decoded {
        Some(s) if s.contains("://") => s,
        _ => trimmed.to_string(),
    }
}

/// 单条分享链接解析
pub fn parse_share_uri(uri: &str, sub_id: &str) -> Result<Node> {
    if let Some(rest) = uri.strip_prefix("ss://") {
        parse_ss(rest, sub_id)
    } else if let Some(rest) = uri.strip_prefix("vmess://") {
        parse_vmess(rest, sub_id)
    } else if let Some(rest) = uri.strip_prefix("trojan://") {
        parse_authority(rest, sub_id, NodeKind::Trojan)
    } else if let Some(rest) = uri.strip_prefix("vless://") {
        parse_authority(rest, sub_id, NodeKind::Vless)
    } else {
        Err(ProxyError::Subscription("不支持的协议".into()))
    }
}

/// ss://（SIP002：base64(method:password)@host:port#tag / plugin 暂不解析）
fn parse_ss(rest: &str, sub_id: &str) -> Result<Node> {
    let (main, frag) = split_fragment(rest);
    let tag = frag.unwrap_or_else(|| "ss".into());

    if let Some(at) = main.rfind('@') {
        // SIP002：userinfo（base64(method:password) 或明文 method:password）@host:port
        let userinfo = &main[..at];
        let hostport = &main[at + 1..];
        let decoded = b64_or_raw(userinfo);
        let (method, password) = decoded
            .split_once(':')
            .ok_or_else(|| ProxyError::Subscription("ss userinfo 缺 method:password".into()))?;
        let (host, port) = split_host_port(hostport)?;
        return Ok(Node {
            tag,
            kind: NodeKind::Shadowsocks,
            server: host,
            port,
            sub_id: sub_id.into(),
            extra: serde_json::json!({ "method": method, "password": password }),
        });
    }
    // 旧版：base64(method:password@host:port)
    let decoded = b64_decode(main)?;
    let at = decoded
        .rfind('@')
        .ok_or_else(|| ProxyError::Subscription("ss 旧版格式缺 @".into()))?;
    let (method, password) = decoded[..at]
        .split_once(':')
        .ok_or_else(|| ProxyError::Subscription("ss 旧版格式缺 method:password".into()))?;
    let (host, port) = split_host_port(&decoded[at + 1..])?;
    Ok(Node {
        tag,
        kind: NodeKind::Shadowsocks,
        server: host,
        port,
        sub_id: sub_id.into(),
        extra: serde_json::json!({ "method": method, "password": password }),
    })
}

/// vmess://（v2ray JSON：{v,ps,add,port,id,aid,net,type,host,path,tls}）
fn parse_vmess(rest: &str, sub_id: &str) -> Result<Node> {
    let raw = b64_decode(rest)?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| ProxyError::Subscription(format!("vmess JSON: {e}")))?;
    let server = v["add"].as_str().unwrap_or_default().to_string();
    let port = match &v["port"] {
        serde_json::Value::Number(n) => n.as_u64().unwrap_or(0) as u16,
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    };
    if server.is_empty() || port == 0 {
        return Err(ProxyError::Subscription("vmess 缺 add/port".into()));
    }
    let tls = v["tls"].as_str().unwrap_or_default() == "tls";
    Ok(Node {
        tag: v["ps"].as_str().unwrap_or("vmess").to_string(),
        kind: NodeKind::Vmess,
        server,
        port,
        sub_id: sub_id.into(),
        extra: serde_json::json!({
            "uuid": v["id"].as_str().unwrap_or_default(),
            "alter_id": v["aid"].as_u64().unwrap_or(0),
            "security": v["scy"].as_str().unwrap_or("auto"),
            "network": v["net"].as_str().unwrap_or("tcp"),
            "tls": tls,
            "sni": v["sni"].as_str().or(v["host"].as_str()).unwrap_or_default(),
        }),
    })
}

/// trojan://password@host:port?sni=..#tag 与 vless://uuid@host:port?..#tag 共用骨架
fn parse_authority(rest: &str, sub_id: &str, kind: NodeKind) -> Result<Node> {
    let (main, frag) = split_fragment(rest);
    let default_tag = match kind {
        NodeKind::Trojan => "trojan",
        NodeKind::Vless => "vless",
        _ => "node",
    };
    let tag = frag.unwrap_or_else(|| default_tag.into());
    let at = main
        .rfind('@')
        .ok_or_else(|| ProxyError::Subscription(format!("{} 缺 @", kind.as_str())))?;
    let secret = &main[..at];
    let tail = &main[at + 1..];
    let (hostport, query) = match tail.split_once('?') {
        Some((hp, q)) => (hp, Some(q)),
        None => (tail, None),
    };
    let (host, port) = split_host_port(hostport)?;
    let params: std::collections::HashMap<&str, &str> = query
        .map(|q| {
            q.split('&')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k, v))
                .collect()
        })
        .unwrap_or_default();

    let extra = match kind {
        NodeKind::Trojan => serde_json::json!({
            "password": secret,
            "sni": params.get("sni").copied().unwrap_or_default(),
        }),
        NodeKind::Vless => {
            let flow = params.get("flow").copied().unwrap_or_default();
            let mut e = serde_json::json!({
                "uuid": secret,
                "sni": params.get("sni").copied().unwrap_or_default(),
                "network": params.get("type").copied().unwrap_or("tcp"),
                "tls": params.get("security").copied() == Some("tls"),
            });
            if !flow.is_empty() {
                e["flow"] = serde_json::json!(flow);
            }
            e
        }
        _ => serde_json::json!({}),
    };
    Ok(Node { tag, kind, server: host, port, sub_id: sub_id.into(), extra })
}

fn split_fragment(rest: &str) -> (&str, Option<String>) {
    match rest.split_once('#') {
        Some((main, frag)) => (main, Some(url_decode(frag))),
        None => (rest, None),
    }
}

fn split_host_port(s: &str) -> Result<(String, u16)> {
    // IPv6 字面量 [::1]:443
    let (host, port_str) = if let Some(stripped) = s.strip_prefix('[') {
        let close = stripped
            .find(']')
            .ok_or_else(|| ProxyError::Subscription("IPv6 缺 ]".into()))?;
        let host = &stripped[..close];
        let rest = &stripped[close + 1..];
        (host.to_string(), rest.trim_start_matches(':').to_string())
    } else {
        match s.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.to_string()),
            None => return Err(ProxyError::Subscription(format!("缺端口: {s}"))),
        }
    };
    let port: u16 = port_str
        .parse()
        .map_err(|_| ProxyError::Subscription(format!("端口非法: {port_str}")))?;
    if host.is_empty() {
        return Err(ProxyError::Subscription("缺主机".into()));
    }
    Ok((host, port))
}

fn b64_or_raw(s: &str) -> String {
    b64_decode(s).unwrap_or_else(|_| url_decode(s))
}

fn b64_decode(s: &str) -> Result<String> {
    // base64 URL-safe 与标准变体兼容处理
    let cleaned = s.trim_end_matches('=');
    let std_engine = base64::engine::general_purpose::STANDARD;
    let url_engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    url_engine
        .decode(cleaned)
        .ok()
        .or_else(|| std_engine.decode(s).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(|| ProxyError::Subscription("base64 解码失败".into()))
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 2 < bytes.len() + 1 {
            if let (Some(h), Some(l)) = (hex(bytes.get(i + 1)), hex(bytes.get(i + 2))) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: Option<&u8>) -> Option<u8> {
    match b {
        Some(c @ b'0'..=b'9') => Some(c - b'0'),
        Some(c @ b'a'..=b'f') => Some(c - b'a' + 10),
        Some(c @ b'A'..=b'F') => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUB: &str = "sub1";

    #[test]
    fn parses_sip002_ss() {
        // aes-256-gcm:password123 → base64 = YWVzLTI1Ni1nY206cGFzc3dvcmQxMjM=
        let uri = "ss://YWVzLTI1Ni1nY206cGFzc3dvcmQxMjM=@1.2.3.4:8388#%E9%A6%99%E6%B8%AF%201";
        let n = parse_share_uri(uri, SUB).unwrap();
        assert_eq!(n.kind, NodeKind::Shadowsocks);
        assert_eq!(n.server, "1.2.3.4");
        assert_eq!(n.port, 8388);
        assert_eq!(n.tag, "香港 1");
        assert_eq!(n.extra["method"], "aes-256-gcm");
        assert_eq!(n.extra["password"], "password123");
    }

    #[test]
    fn parses_legacy_ss() {
        // base64("aes-128-gcm:test@5.6.7.8:443")
        let inner = "aes-128-gcm:test@5.6.7.8:443";
        let b64 = base64::engine::general_purpose::STANDARD.encode(inner);
        let n = parse_share_uri(&format!("ss://{b64}"), SUB).unwrap();
        assert_eq!(n.server, "5.6.7.8");
        assert_eq!(n.port, 443);
        assert_eq!(n.extra["method"], "aes-128-gcm");
    }

    #[test]
    fn parses_vmess_json() {
        let payload = serde_json::json!({
            "v": "2", "ps": "测试节点", "add": "example.com", "port": "443",
            "id": "b831381d-6324-4d53-ad4f-8cda48b30811", "aid": "0",
            "net": "ws", "host": "example.com", "path": "/ws", "tls": "tls"
        });
        let b64 = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
        let n = parse_share_uri(&format!("vmess://{b64}"), SUB).unwrap();
        assert_eq!(n.kind, NodeKind::Vmess);
        assert_eq!(n.server, "example.com");
        assert_eq!(n.port, 443);
        assert_eq!(n.tag, "测试节点");
        assert!(n.extra["tls"].as_bool().unwrap());
        assert_eq!(n.extra["network"], "ws");
    }

    #[test]
    fn parses_trojan_and_vless() {
        let t = parse_share_uri("trojan://pw123@9.9.9.9:443?sni=example.com#T节点", SUB).unwrap();
        assert_eq!(t.kind, NodeKind::Trojan);
        assert_eq!(t.extra["password"], "pw123");
        assert_eq!(t.extra["sni"], "example.com");

        let v = parse_share_uri(
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@1.1.1.1:443?security=tls&sni=foo.com&type=tcp&flow=xtls-rprx-vision#V节点",
            SUB,
        )
        .unwrap();
        assert_eq!(v.kind, NodeKind::Vless);
        assert_eq!(v.extra["flow"], "xtls-rprx-vision");
        assert!(v.extra["tls"].as_bool().unwrap());
    }

    #[test]
    fn parses_whole_b64_subscription() {
        let lines = "ss://YWVzLTI1Ni1nY206cGFzc3dvcmQxMjM=@1.2.3.4:8388#a1\ntrojan://p@5.5.5.5:443#a2";
        let b64 = base64::engine::general_purpose::STANDARD.encode(lines);
        let nodes = parse_subscription(&b64, SUB).unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].kind, NodeKind::Shadowsocks);
        assert_eq!(nodes[1].kind, NodeKind::Trojan);
    }

    #[test]
    fn rejects_empty_and_unknown() {
        assert!(parse_subscription("dGV4dA==", SUB).is_err()); // "text" 无 URI
        assert!(parse_share_uri("unknown://x", SUB).is_err());
        let nodes = parse_subscription("unknown://x\nss://YWVzLTI1Ni1nY206cGFzc3dvcmQxMjM=@1.2.3.4:8388#a", SUB).unwrap();
        assert_eq!(nodes.len(), 1, "未知行跳过不致命");
    }

    #[test]
    fn outbound_tag_prefixes_sub() {
        let n = parse_share_uri("ss://YWVzLTI1Ni1nY206cGFzc3dvcmQxMjM=@1.2.3.4:8388#x", "abcdefgh12345678").unwrap();
        assert_eq!(n.outbound_tag(), "abcdefgh:x");
    }
}
