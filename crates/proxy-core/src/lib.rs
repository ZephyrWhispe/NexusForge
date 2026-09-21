//! proxy-core 代理与 VPN 框架（docs/impl/05 PR1–PR6）
//!
//! 合规红线（DESIGN §10.5）：只做框架与本地编排，**不内置任何节点/订阅**；
//! sing-box 内核经 Sidecar 按需从官方 Release 下载（PR2）；THIRD_PARTY_LICENSES 登记。
//!
//! 高危安全语义：系统代理还原挂四点 —— panic hook / module stop / `--restore-proxy`
//! / 启动扫描（[`sysproxy::restore_if_ours`]，覆盖 kill -9 残留）。

pub mod clash_yaml;
pub mod config;
pub mod error;
pub mod ir;
pub mod kernel;
pub mod mihomo;
pub mod module;
pub mod rules;
pub mod service;
pub mod sidecar;
pub mod singbox;
pub mod sub;
pub mod sysproxy;
pub mod xray;

pub use config::RouteMode;
pub use error::{ProxyError, Result};
pub use kernel::{KernelDriver, KernelHandle, LogLine, SingBoxDriver};
pub use mihomo::MihomoDriver;
pub use module::ProxyModule;
pub use rules::{RuleV2, RulesV2};
pub use service::{Mode, NodeDelayDto, NodeDto, ProxyService, StatusDto, Sub};
pub use sidecar::Manifest;
pub use sub::{Node, NodeKind};
pub use xray::XrayDriver;
