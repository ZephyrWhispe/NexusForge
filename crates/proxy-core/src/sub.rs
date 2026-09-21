//! PR3 订阅与节点解析（docs/impl/05 PR3 前半）：通用分享链接 URI → 节点。
//!
//! 合规红线：**只做解析框架，不内置任何节点/订阅**；订阅 URL 全部由用户添加，
//! 内容视为敏感（不落明文日志，仅持久化到 `{appData}/proxy/subs/`）。
//! 支持：ss://（SIP002 含 `?plugin=` 透传 + 旧版整体 base64）、vmess://（v2ray JSON）、
//! trojan://、vless://（含 REALITY/XHTTP 透传键）、hy2://、tuic://、wg://、ssr://（T-B2-7）。
//! 订阅正文：整体 base64 或纯文本多行 URI，自动探测。

use serde::{Deserialize, Serialize};

use crate::error::{ProxyError, Result};

// ---------------- T-B2-10：订阅标准头解析 + 偏离度门禁（纯函数，零依赖） ----------------

/// 订阅流量信息（`upload/download/left/expire` 标准头，字节数/到期时刻）。
/// 各字段独立可选：面板常只发其中几枚，全缺时整体为 None（UI 不谎显）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficInfo {
    #[serde(default)]
    pub upload: u64,
    #[serde(default)]
    pub download: u64,
    #[serde(default)]
    pub left: u64,
    /// 到期时刻（毫秒；订阅头 `expire` 是 Unix 秒，解析处 ×1000）；0=未提供
    #[serde(default)]
    pub expire_ms: u64,
}

/// 一次订阅响应头的解析结果（T-B2-10，02§3-7 地基）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubHeaders {
    pub traffic: Option<TrafficInfo>,
    pub interval_min: Option<u64>,
    pub etag: Option<String>,
}

/// 数值头解析：十进制直解优先；`--` 前缀 = base64 变体方言（部分面板把数字
/// base64 后加 `--` 标记防混淆），解码失败即 None（禁猜值）。
fn parse_header_u64(v: &str) -> Option<u64> {
    let v = v.trim();
    if let Some(b64) = v.strip_prefix("--") {
        let decoded = host_core::util::b64_decode_lenient(b64)?;
        return String::from_utf8(decoded).ok()?.trim().parse().ok();
    }
    v.parse().ok()
}

/// 从（已小写化的）响应头解析订阅元数据。`headers` 键一律小写——HTTP/2 与
/// reqwest 内部表示都是小写，服务层在构造映射时就归一，此处不再大小写试探。
pub fn parse_sub_headers(headers: &std::collections::BTreeMap<String, String>) -> SubHeaders {
    let num = |k: &str| headers.get(k).and_then(|v| parse_header_u64(v));
    let (upload, download, left, expire_s) =
        (num("upload"), num("download"), num("left"), num("expire"));
    let traffic = if upload.is_some() || download.is_some() || left.is_some() || expire_s.is_some()
    {
        Some(TrafficInfo {
            upload: upload.unwrap_or(0),
            download: download.unwrap_or(0),
            left: left.unwrap_or(0),
            expire_ms: expire_s.unwrap_or(0) * 1000,
        })
    } else {
        None
    };
    SubHeaders {
        traffic,
        interval_min: num("profile-update-interval"),
        etag: headers.get("etag").filter(|e| !e.is_empty()).cloned(),
    }
}

/// 订阅偏离度门禁（B9 消费的纯函数，本批只落函数+测）：更新后节点数为 0，
/// 或相对上次变化率 ≥0.5（含恰半数）时必须人工确认——防面板故障清空订阅。
pub fn deviation_needs_confirm(old: usize, new: usize) -> bool {
    if new == 0 {
        return true;
    }
    let diff = old.abs_diff(new);
    // 整数比对避免浮点边界：diff/max(old,1) ≥ 1/2 ⟺ 2*diff ≥ max(old,1)
    diff * 2 >= old.max(1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Shadowsocks,
    Vmess,
    Trojan,
    Vless,
    // ↓ T-B2-7 四新变体。**跨版本单向门（09 §5.1-⑬）**：写过新 kind 的
    // nodes.json 在旧版应用上会整文件反序列化失败（load 的 .ok() 吞掉=静默清空），
    // 发布后不可回滚过本批；⑬ 降险依赖读侧 serde-default 纪律不外溢到本枚举。
    Hysteria2,
    Tuic5,
    WireGuard,
    /// SS-R 为 v2rayN/专用分叉方言，sing-box/xray/mihomo **官方主线均不支持**
    /// （09 §5.1-⑭ 实施核证）→ 三驱动 supported_kinds 恒排除：节点保留可读、
    /// 换核预检报数，但当前任何内核都无法作为其出口。
    #[serde(rename = "ssr")]
    ShadowsocksR,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Shadowsocks => "shadowsocks",
            Self::Vmess => "vmess",
            Self::Trojan => "trojan",
            Self::Vless => "vless",
            Self::Hysteria2 => "hysteria2",
            Self::Tuic5 => "tuic5",
            Self::WireGuard => "wireguard",
            Self::ShadowsocksR => "ssr",
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
    /// T-B2-8：Clash YAML 订阅中该节点所属 proxy-groups 组名（URI 路恒空）。
    /// **[收窄 v1 登记]**（09 §5.2 行字面）：订阅内嵌组**不驱动** IR 选择器，
    /// 仅供 UI 过滤列展示；组驱动策略组归 B7 与外部控制 API 同批。
    /// nodes.json 新键 serde-default 零迁移；⑬ 单向门与 T-B2-7 同批登记。
    #[serde(default)]
    pub groups: Vec<String>,
}

impl Node {
    /// 出站 tag：`{sub_id前8字符}:{tag}` 避免跨订阅重名。
    /// 缺陷⑫（09 §5.1）修复：必须按 chars 而非 &str[..8] 字节切片 ——
    /// 订阅 id 今天恒为 uuid v7（ASCII）但 tag/历史数据不保证，多字节 id 曾直接 panic。
    pub fn outbound_tag(&self) -> String {
        let sub: String = self.sub_id.chars().take(8).collect();
        format!("{sub}:{}", self.tag)
    }
}

/// 解析订阅正文 → 节点列表。Clash YAML 正文探测优先；其余整体 base64 或纯文本多行 URI 自动探测。
pub fn parse_subscription(content: &str, sub_id: &str) -> Result<Vec<Node>> {
    // T-B2-8（09 §5.2 行字面）：行首顶层键 proxies:/proxy-groups: 探测**先于**
    // b64 启发式——YAML 正文含 `:`/换行/中文本来就出不了 b64 字符集，但顺序
    // 钉死防"某 YAML 恰好整体是合法 b64 字符集"的畸形重叠样本被误当 URI 解码。
    if crate::clash_yaml::is_clash_yaml(content) {
        let (nodes, _groups) = crate::clash_yaml::parse_clash_subscription(content, sub_id)?;
        return Ok(nodes);
    }
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
    let decoded = host_core::util::b64_decode(trimmed).and_then(|b| String::from_utf8(b).ok());
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
    } else if let Some(rest) = uri.strip_prefix("hy2://") {
        parse_hy2(rest, sub_id)
    } else if let Some(rest) = uri.strip_prefix("tuic://") {
        parse_tuic(rest, sub_id)
    } else if let Some(rest) = uri.strip_prefix("wg://") {
        parse_wg(rest, sub_id)
    } else if let Some(rest) = uri.strip_prefix("ssr://") {
        parse_ssr(rest, sub_id)
    } else {
        Err(ProxyError::Subscription("不支持的协议".into()))
    }
}

/// ss://（SIP002：base64(method:password)@host:port[/?plugin=..]#tag / 旧版整体 base64）
fn parse_ss(rest: &str, sub_id: &str) -> Result<Node> {
    let (main, frag) = split_fragment(rest);
    let tag = frag.unwrap_or_else(|| "ss".into());

    if let Some(at) = main.rfind('@') {
        // SIP002：userinfo（base64(method:password) 或明文 method:password）@host:port[/ ?query]
        let userinfo = &main[..at];
        let (hostport, query) = split_sip002_query(&main[at + 1..]);
        let decoded = b64_or_raw(userinfo);
        let (method, password) = decoded
            .split_once(':')
            .ok_or_else(|| ProxyError::Subscription("ss userinfo 缺 method:password".into()))?;
        let (host, port) = split_host_port(hostport)?;
        let mut extra = serde_json::json!({ "method": method, "password": password });
        // T-B2-7：`?plugin=obfs-local%3Bk%3Dv…` 解码后原样存 extra["plugin"]
        //（渲染端 sing-box plugin 字段透传；参数体不做语义解析=白名单外原样保留）
        if let Some(plugin) = query.get("plugin") {
            let decoded = url_decode(plugin);
            if !decoded.is_empty() {
                extra["plugin"] = serde_json::json!(decoded);
            }
        }
        return Ok(Node {
            tag,
            kind: NodeKind::Shadowsocks,
            server: host,
            port,
            sub_id: sub_id.into(),
            groups: Vec::new(),
            extra,
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
        groups: Vec::new(),
        extra: serde_json::json!({ "method": method, "password": password }),
    })
}

/// vmess://（v2ray JSON：{v,ps,add,port,id,aid,net,type,host,path,tls}）
fn parse_vmess(rest: &str, sub_id: &str) -> Result<Node> {
    let raw = b64_decode(rest)?;
    let v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| ProxyError::Subscription(format!("vmess JSON: {e}")))?;
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
        groups: Vec::new(),
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
        .map(|q| q.split('&').filter_map(|kv| kv.split_once('=')).collect())
        .unwrap_or_default();

    let mut extra = match kind {
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
    // T-B2-7 REALITY/XHTTP/host 透传（09 §5.2 行字面白名单键）：
    // 只按 pbk/sid/mode/host 解析，其余未知参数原样丢弃 = 白名单外不进 extra。
    if let Some(h) = params.get("host").copied() {
        if !h.is_empty() {
            extra["host"] = serde_json::json!(h);
        }
    }
    if params.get("security").copied() == Some("reality") {
        let pbk = params.get("pbk").copied().unwrap_or_default();
        if pbk.is_empty() {
            return Err(ProxyError::Subscription(
                "reality 缺 pbk（public_key）".into(),
            ));
        }
        let sid = params.get("sid").copied().unwrap_or_default();
        if !sid.is_empty() && !is_even_hex(sid) {
            return Err(ProxyError::Subscription(format!(
                "reality short_id 非合法十六进制（须偶数位 hex）: {sid}"
            )));
        }
        extra["tls"] = serde_json::json!(true);
        extra["reality"] = serde_json::json!({ "public_key": pbk, "short_id": sid });
    }
    if params.get("type").copied() == Some("xhttp") {
        extra["network"] = serde_json::json!("xhttp");
        extra["xhttp"] =
            serde_json::json!({ "mode": params.get("mode").copied().unwrap_or_default() });
    }
    Ok(Node {
        tag,
        kind,
        server: host,
        port,
        sub_id: sub_id.into(),
        groups: Vec::new(),
        extra,
    })
}

/// 分离 SIP002 的 host:port 与 `/?k=v&..` 查询段（旧版无查询则全为主机段）
fn split_sip002_query(s: &str) -> (&str, std::collections::HashMap<&str, &str>) {
    match s.split_once('?') {
        Some((hp, q)) => (
            hp.trim_end_matches('/'),
            q.split('&').filter_map(|kv| kv.split_once('=')).collect(),
        ),
        None => (s, std::collections::HashMap::new()),
    }
}

/// hy2://base64(密码)@host:port?sni=..&obfs=salamander&obfs-password=..&insecure=1#tag
fn parse_hy2(rest: &str, sub_id: &str) -> Result<Node> {
    let (main, frag) = split_fragment(rest);
    let tag = frag.unwrap_or_else(|| "hy2".into());
    let at = main
        .rfind('@')
        .ok_or_else(|| ProxyError::Subscription("hy2 缺 @".into()))?;
    let password = b64_or_raw(&main[..at]);
    let (hostport, query) = split_sip002_query(&main[at + 1..]);
    let (host, port) = split_host_port(hostport)?;
    let mut extra = serde_json::json!({
        "password": password,
        "sni": query.get("sni").copied().unwrap_or_default(),
        "insecure": query.get("insecure").copied() == Some("1"),
    });
    if let Some(obfs) = query.get("obfs").copied() {
        extra["obfs"] = serde_json::json!(obfs);
        extra["obfs_password"] =
            serde_json::json!(query.get("obfs-password").copied().unwrap_or_default());
    }
    Ok(Node {
        tag,
        kind: NodeKind::Hysteria2,
        server: host,
        port,
        sub_id: sub_id.into(),
        groups: Vec::new(),
        extra,
    })
}

/// tuic://uuid:password@host:port?token=..&sni=..&alpn=h3#tag（密码空时回落 token）
fn parse_tuic(rest: &str, sub_id: &str) -> Result<Node> {
    let (main, frag) = split_fragment(rest);
    let tag = frag.unwrap_or_else(|| "tuic".into());
    let at = main
        .rfind('@')
        .ok_or_else(|| ProxyError::Subscription("tuic 缺 @".into()))?;
    let secret = &main[..at];
    let (uuid, password) = secret
        .split_once(':')
        .ok_or_else(|| ProxyError::Subscription("tuic 缺 uuid:password".into()))?;
    if !is_uuid(uuid) {
        return Err(ProxyError::Subscription(format!("tuic uuid 非法: {uuid}")));
    }
    let (hostport, query) = split_sip002_query(&main[at + 1..]);
    let (host, port) = split_host_port(hostport)?;
    let password = if password.is_empty() {
        query.get("token").copied().unwrap_or_default().to_string()
    } else {
        password.to_string()
    };
    let extra = serde_json::json!({
        "uuid": uuid,
        "password": password,
        "sni": query.get("sni").copied().unwrap_or_default(),
        "alpn": query.get("alpn").copied().unwrap_or_default(),
    });
    Ok(Node {
        tag,
        kind: NodeKind::Tuic5,
        server: host,
        port,
        sub_id: sub_id.into(),
        groups: Vec::new(),
        extra,
    })
}

/// wg://url-encode(私钥)?endpoint=host:port&publickey=..&preshared=..&address=..#tag
fn parse_wg(rest: &str, sub_id: &str) -> Result<Node> {
    let (main, frag) = split_fragment(rest);
    let tag = frag.unwrap_or_else(|| "wg".into());
    let (priv_raw, query_str) = main
        .split_once('?')
        .ok_or_else(|| ProxyError::Subscription("wg 缺 ? 查询段（endpoint 等）".into()))?;
    let query: std::collections::HashMap<&str, &str> = query_str
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .collect();
    let endpoint = query
        .get("endpoint")
        .ok_or_else(|| ProxyError::Subscription("wg 缺 endpoint 参数".into()))?;
    let (host, port) = split_host_port(endpoint)?;
    let private_key = url_decode(priv_raw);
    if private_key.is_empty() {
        return Err(ProxyError::Subscription("wg 缺私钥".into()));
    }
    // wg 链接的参数值普遍 percent 编码（base64 键的 '=' 结尾必须转义），逐值解码
    let decoded = |k: &str| -> String { query.get(k).copied().map(url_decode).unwrap_or_default() };
    let extra = serde_json::json!({
        "private_key": private_key,
        "public_key": decoded("publickey"),
        "preshared_key": decoded("preshared"),
        "address": decoded("address"),
    });
    Ok(Node {
        tag,
        kind: NodeKind::WireGuard,
        server: host,
        port,
        sub_id: sub_id.into(),
        groups: Vec::new(),
        extra,
    })
}

/// ssr://base64url(host:port:proto:cipher:obfs:url64(params))#tag（v2rayN 方言）。
/// 注：SSR 链接体不含密码字段（extra["password"] 恒空串）；且三内核官方均不支持
/// SSR 出口（09 §5.1-⑭），本解析器只保证节点可读保留与预检报数。
fn parse_ssr(rest: &str, sub_id: &str) -> Result<Node> {
    let (main, frag) = split_fragment(rest);
    let tag = frag.unwrap_or_else(|| "ssr".into());
    let decoded = b64_decode(main)?;
    let segs: Vec<&str> = decoded.split(':').collect();
    if segs.len() != 6 {
        return Err(ProxyError::Subscription(format!(
            "ssr 段数应为 6（host:port:proto:cipher:obfs:params），实得 {}",
            segs.len()
        )));
    }
    let port: u16 = segs[1]
        .parse()
        .map_err(|_| ProxyError::Subscription(format!("ssr 端口非法: {}", segs[1])))?;
    if segs[0].is_empty() {
        return Err(ProxyError::Subscription("ssr 缺主机".into()));
    }
    let params = b64_decode(segs[5]).unwrap_or_default();
    let pp: std::collections::HashMap<&str, &str> = params
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .collect();
    let extra = serde_json::json!({
        "method": segs[3],
        "password": "",
        "protocol": segs[2],
        "obfs": segs[4],
        "protocol_param": pp.get("protoparam").copied().unwrap_or_default(),
        "obfs_param": pp.get("obfsparam").copied().unwrap_or_default(),
    });
    Ok(Node {
        tag,
        kind: NodeKind::ShadowsocksR,
        server: segs[0].to_string(),
        port,
        sub_id: sub_id.into(),
        groups: Vec::new(),
        extra,
    })
}

/// 手写 UUID 校验（8-4-4-4-12 hex 段）：tuic 解析红线，零新增依赖
fn is_uuid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    let lens = [8usize, 4, 4, 4, 12];
    groups.len() == 5
        && groups
            .iter()
            .zip(lens)
            .all(|(g, l)| g.len() == l && g.chars().all(|c| c.is_ascii_hexdigit()))
}

fn is_even_hex(s: &str) -> bool {
    !s.is_empty() && s.len().is_multiple_of(2) && s.chars().all(|c| c.is_ascii_hexdigit())
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
    // base64 URL-safe 与标准变体兼容处理（订阅链接可能带 padding 的 URL-safe 段）
    let cleaned = s.trim_end_matches('=');
    host_core::util::b64_decode_lenient(cleaned)
        .or_else(|| host_core::util::b64_decode(s))
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
        let b64 = host_core::util::b64_encode(inner.as_bytes());
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
        let b64 = host_core::util::b64_encode(payload.to_string().as_bytes());
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
        let lines =
            "ss://YWVzLTI1Ni1nY206cGFzc3dvcmQxMjM=@1.2.3.4:8388#a1\ntrojan://p@5.5.5.5:443#a2";
        let b64 = host_core::util::b64_encode(lines.as_bytes());
        let nodes = parse_subscription(&b64, SUB).unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].kind, NodeKind::Shadowsocks);
        assert_eq!(nodes[1].kind, NodeKind::Trojan);
    }

    #[test]
    fn rejects_empty_and_unknown() {
        assert!(parse_subscription("dGV4dA==", SUB).is_err()); // "text" 无 URI
        assert!(parse_share_uri("unknown://x", SUB).is_err());
        let nodes = parse_subscription(
            "unknown://x\nss://YWVzLTI1Ni1nY206cGFzc3dvcmQxMjM=@1.2.3.4:8388#a",
            SUB,
        )
        .unwrap();
        assert_eq!(nodes.len(), 1, "未知行跳过不致命");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    fn subHysteria2_missingPort_rejected() {
        // 正控：base64(密码) + sni/insecure/obfs 参数全量落 extra
        let b64 = host_core::util::b64_encode("p@ssw0rd".as_bytes());
        let ok = parse_share_uri(
            &format!("hy2://{b64}@1.2.3.4:8443?sni=a.com&insecure=1&obfs=salamander&obfs-password=op#H节点"),
            SUB,
        )
        .unwrap();
        assert_eq!(ok.kind, NodeKind::Hysteria2);
        assert_eq!(ok.server, "1.2.3.4");
        assert_eq!(ok.port, 8443);
        assert_eq!(ok.tag, "H节点");
        assert_eq!(ok.extra["password"], "p@ssw0rd");
        assert_eq!(ok.extra["insecure"], true);
        assert_eq!(ok.extra["obfs"], "salamander");
        assert_eq!(ok.extra["obfs_password"], "op");
        // 红线：缺端口诚实拒（不默认 443 猜端口）
        assert!(parse_share_uri("hy2://cHcxMjM=@1.2.3.4?sni=a.com#H", SUB).is_err());
    }

    #[test]
    #[allow(non_snake_case)]
    fn subTuic5_badUuid_rejected() {
        let good = parse_share_uri(
            "tuic://b831381d-6324-4d53-ad4f-8cda48b30811:pw@1.2.3.4:443?sni=x.com&alpn=h3#T",
            SUB,
        )
        .unwrap();
        assert_eq!(good.kind, NodeKind::Tuic5);
        assert_eq!(good.extra["uuid"], "b831381d-6324-4d53-ad4f-8cda48b30811");
        assert_eq!(good.extra["password"], "pw");
        // 红线：uuid 段非法（长度/非 hex）即整条拒，禁半解析入 extra
        assert!(parse_share_uri("tuic://not-a-uuid:pw@1.2.3.4:443#b", SUB).is_err());
        assert!(parse_share_uri(
            "tuic://b831381d-6324-4d53-ad4f-8cda48b3081g:pw@1.2.3.4:443",
            SUB
        )
        .is_err());
        // 密码段为空 → 回落 token 参数（tuic v5 链接两种方言都常见）
        let tok = parse_share_uri(
            "tuic://b831381d-6324-4d53-ad4f-8cda48b30811:@1.2.3.4:443?token=sekret",
            SUB,
        )
        .unwrap();
        assert_eq!(tok.extra["password"], "sekret");
    }

    #[test]
    #[allow(non_snake_case)]
    fn subWireGuard_missingEndpoint_rejected() {
        let good = parse_share_uri(
            "wg://c0Rq0YK3mZ0vV0aX0nQX1mR0d0d0d0d0d0d0d0d0d0f%3D?endpoint=1.2.3.4:51820&publickey=Yx9YI4mZ0vV0aX0nQX1mR0d0d0d0d0d0d0d0d0d0dg%3D%3D&address=10.0.0.2%2F32&wG",
            SUB,
        )
        .unwrap();
        assert_eq!(good.kind, NodeKind::WireGuard);
        assert_eq!(good.server, "1.2.3.4");
        assert_eq!(good.port, 51820);
        // percent 编码键解码还原（base64 尾 '=' 转义面）
        assert_eq!(
            good.extra["private_key"],
            "c0Rq0YK3mZ0vV0aX0nQX1mR0d0d0d0d0d0d0d0d0d0f="
        );
        assert_eq!(good.extra["address"], "10.0.0.2/32");
        // 红线：无查询段 / 查询缺 endpoint 参数均拒（wg 无 host 段可猜）
        assert!(parse_share_uri("wg://privkey%3D?publickey=abc#w", SUB).is_err());
        assert!(parse_share_uri("wg://privkey%3D#w", SUB).is_err());
    }

    #[test]
    #[allow(non_snake_case)]
    fn subSsr_badBase64Url_rejected() {
        // 红线：非法 base64 主体拒（禁半解析）
        assert!(parse_share_uri("ssr://!!!not-base64!!!#x", SUB).is_err());
        // 段数不足 6 也拒（host:port:proto:cipher:obfs:params 缺一不可）
        let short = host_core::util::b64_encode(b"1.2.3.4:8080:origin");
        assert!(parse_share_uri(&format!("ssr://{short}#x"), SUB).is_err());
        // 正控：v2rayN 六段方言完整解析 + serde 名恒 "ssr"（nodes.json 落盘形状）
        let params = host_core::util::b64_encode(b"protoparam=pp1&obfsparam=ob1");
        let inner = format!("1.2.3.4:8080:origin:aes-256-cfb:plain:{params}");
        let b64 = host_core::util::b64_encode(inner.as_bytes());
        let n = parse_share_uri(&format!("ssr://{b64}#SSR节点"), SUB).unwrap();
        assert_eq!(n.kind, NodeKind::ShadowsocksR);
        assert_eq!(n.port, 8080);
        assert_eq!(n.extra["method"], "aes-256-cfb");
        assert_eq!(n.extra["obfs_param"], "ob1");
        assert_eq!(n.extra["protocol_param"], "pp1");
        assert_eq!(n.extra["password"], "");
        assert_eq!(serde_json::to_string(&n.kind).unwrap(), "\"ssr\"");
    }

    #[test]
    #[allow(non_snake_case)]
    fn subReality_badHexShortId_rejected() {
        // 红线：sid 非空且（奇数位 or 非 hex）→ 整条拒，禁把坏 hex 透传进渲染端
        assert!(parse_share_uri(
            "vless://abc@1.1.1.1:443?security=reality&pbk=KEY&sid=abc12#x",
            SUB
        )
        .is_err());
        assert!(parse_share_uri(
            "vless://abc@1.1.1.1:443?security=reality&pbk=KEY&sid=zz1234#x",
            SUB
        )
        .is_err());
        // 缺 pbk 同样拒（REALITY 无公钥必连不上）
        assert!(parse_share_uri("vless://abc@1.1.1.1:443?security=reality#x", SUB).is_err());
        // 正控：合法偶 hex sid + 空 sid 放行；REALITY/XHTTP/host 透传键形状
        let n = parse_share_uri(
            "vless://c0ee752e-de1a-4fde-a6bf-0f04b1c9d6e3@1.1.1.1:443?security=reality&pbk=REALPUBKEY&sid=0123&host=cdn.example.com&type=xhttp&mode=auto#R",
            SUB,
        )
        .unwrap();
        assert_eq!(n.extra["reality"]["public_key"], "REALPUBKEY");
        assert_eq!(n.extra["reality"]["short_id"], "0123");
        assert_eq!(n.extra["network"], "xhttp");
        assert_eq!(n.extra["xhttp"]["mode"], "auto");
        assert_eq!(n.extra["host"], "cdn.example.com");
        assert_eq!(n.extra["tls"], true);
        let empty_sid = parse_share_uri(
            "vless://abc@1.1.1.1:443?security=reality&pbk=KEY&sid=#x",
            SUB,
        )
        .unwrap();
        assert_eq!(empty_sid.extra["reality"]["short_id"], "");
    }

    #[test]
    #[allow(non_snake_case)]
    fn ssPluginParam_parsedIntoExtra() {
        // 正控：SIP002 `/?plugin=` percent 编码参数解码后原样入 extra["plugin"]
        let n = parse_share_uri(
            "ss://YWVzLTI1Ni1nY206cGFzc3dvcmQxMjM=@1.2.3.4:8388/?plugin=obfs-local%3Bobfs%3Dtls%3Bobfs-host%3Dwww.bing.com#s",
            SUB,
        )
        .unwrap();
        assert_eq!(
            n.extra["plugin"],
            "obfs-local;obfs=tls;obfs-host=www.bing.com"
        );
        assert_eq!(n.extra["method"], "aes-256-gcm");
        // 无 plugin 查询段的旧链接不落该键（extra 零污染，golden 输入形状不变）
        let m =
            parse_share_uri("ss://YWVzLTI1Ni1nY206cGFzc3dvcmQxMjM=@1.2.3.4:8388#s2", SUB).unwrap();
        assert!(m.extra.get("plugin").is_none());
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    fn subHeaders_trafficIntervalEtag_parsed() {
        let mut h = std::collections::BTreeMap::new();
        h.insert("upload".into(), "1024".into());
        // `--` 前缀 base64 变体方言：base64("2048") = MjA0OA==
        h.insert(
            "download".into(),
            format!("--{}", host_core::util::b64_encode(b"2048")),
        );
        h.insert("left".into(), " 999999 ".into());
        h.insert("expire".into(), "1893456000".into()); // 秒 → 毫秒
        h.insert("profile-update-interval".into(), "1440".into());
        h.insert("etag".into(), "\"abc123\"".into());
        let got = parse_sub_headers(&h);
        assert_eq!(got.interval_min, Some(1440));
        assert_eq!(got.etag.as_deref(), Some("\"abc123\""));
        let t = got.traffic.expect("有任一数值头即成块");
        assert_eq!((t.upload, t.download, t.left), (1024, 2048, 999999));
        assert_eq!(t.expire_ms, 1893456000 * 1000);
        // 全空头 = traffic None（UI 负例的数据源）；空 etag 串不落键
        let empty = parse_sub_headers(&std::collections::BTreeMap::from([
            ("etag".to_string(), String::new()),
            ("upload".to_string(), "not-a-number".to_string()),
        ]));
        assert!(empty.traffic.is_none());
        assert!(empty.etag.is_none());
        assert!(empty.interval_min.is_none());
    }

    #[test]
    #[allow(non_snake_case)]
    fn deviationGate_halfChangeOrEmpty_confirms() {
        // 红线边界：清空必确认；恰半数（变化率=0.5）确认；略少于半数放行
        assert!(deviation_needs_confirm(10, 0));
        assert!(deviation_needs_confirm(0, 0), "0→0 也是 new==0");
        assert!(deviation_needs_confirm(10, 5));
        assert!(!deviation_needs_confirm(10, 6));
        assert!(deviation_needs_confirm(10, 15));
        assert!(!deviation_needs_confirm(10, 14));
        // 首次添加（old=0 有节点）：0→1 视为全量变化必确认（分母钳 1 防除零）
        assert!(deviation_needs_confirm(0, 1));
        assert!(!deviation_needs_confirm(100, 100));
    }

    #[test]
    fn outbound_tag_prefixes_sub() {
        let n = parse_share_uri(
            "ss://YWVzLTI1Ni1nY206cGFzc3dvcmQxMjM=@1.2.3.4:8388#x",
            "abcdefgh12345678",
        )
        .unwrap();
        assert_eq!(n.outbound_tag(), "abcdefgh:x");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    fn outboundTag_nonAscii_noPanic() {
        // 缺陷⑫回归：多字节 sub_id 曾按字节切片 &sub[..8] 直接 panic（跨字符边界）
        let n = Node {
            tag: "节点一".into(),
            kind: NodeKind::Trojan,
            server: "example.com".into(),
            port: 443,
            sub_id: "订阅中文标识abcdef".into(),
            groups: Vec::new(),
            extra: serde_json::json!({"password": "p"}),
        };
        // chars().take(8)：6 个汉字 + a + b = 8 个字符（旧字节切片在此直接 panic）
        assert_eq!(n.outbound_tag(), "订阅中文标识ab:节点一");
        // ASCII 短 id 不截断（旧行为保持）
        let m = Node {
            sub_id: "abc".into(),
            ..n
        };
        assert_eq!(m.outbound_tag(), "abc:节点一");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    fn subDetectsClashYaml_beforeB64Heuristic() {
        // T-B2-8：正文含中文/冒号/换行（出得了 b64 字符集但走不进 URI 路）→
        // 行首顶层键探测必须先接管，解析出节点并带上组归属
        let yaml = "proxies:\n\
- {name: 香港A, type: ss, server: 1.2.3.4, port: 8388, cipher: aes-256-gcm, password: p1}\n\
proxy-groups:\n\
- {name: 落地, type: select, proxies: [香港A]}\n";
        let nodes = super::parse_subscription(yaml, "sub0aaaa").unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].tag, "香港A");
        assert_eq!(nodes[0].groups, vec!["落地".to_string()]);
        // 正控对立面：URI 多行正文不受探测分支影响（旧路原样）
        let uri =
            "ss://YWVzLTI1Ni1nY206cGFzc3dvcmQxMjM=@1.2.3.4:8388#s1\ntrojan://pw@ex.com:443#t1";
        assert_eq!(super::parse_subscription(uri, "sub0aaaa").unwrap().len(), 2);
    }
}
