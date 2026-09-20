//! PR3 配置生成（docs/impl/05 PR3 后半）：节点 + 模式 → sing-box 配置 JSON。
//!
//! B2 T-B2-1 后本模块是**兼容适配层**：语义组装搬入 [`crate::ir::build`]（内核无关 IR），
//! 方言投影搬入 [`crate::singbox::render`]；`generate` 签名与输出逐字节不变
//! （golden 夹具锁死，见 `tests/golden_singbox.rs`）。
//!
//! - 入站：mixed（HTTP+SOCKS，系统代理指向它）或 tun（PR5，需管理员 + wintun.dll）
//! - 出站：selector(proxy) + urltest(auto) + 节点 + direct
//! - 路由：私有地址恒直连；规则模式追加用户直连域名列表；global 模式 final=proxy

use serde::Serialize;

use crate::error::Result;
use crate::ir::{self, IrRule};
use crate::sub::Node;

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
    // 旧语义保持：仅 Rule 模式消费 direct_domains，Off/Global 一律忽略
    let empty: &[String] = &[];
    let ir = ir::build(
        opts.mixed_port,
        opts.tun,
        opts.nodes,
        &[] as &[IrRule],
        ir::TAG_PROXY,
        if opts.mode == RouteMode::Rule {
            opts.direct_domains
        } else {
            empty
        },
    )?;
    Ok(crate::singbox::render(&ir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProxyError;
    use crate::sub::NodeKind;

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
        assert_eq!(
            outbounds[0]["outbounds"].as_array().unwrap().len(),
            3,
            "2 节点 + auto"
        );
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
        let domains = vec![
            ".corp.example.com".to_string(),
            "internal.local".to_string(),
        ];
        let opts = GenOptions {
            mixed_port: 7890,
            mode: RouteMode::Rule,
            direct_domains: &domains,
            tun: false,
            nodes: &nodes(),
        };
        let cfg = generate(&opts).unwrap();
        let rules = cfg["route"]["rules"].as_array().unwrap();
        assert!(rules
            .iter()
            .any(|r| r["domain_suffix"].as_array().map(|a| a.len()) == Some(2)));
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
