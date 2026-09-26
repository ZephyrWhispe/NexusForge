//! ProxyService 门面（PR1–PR6 编排）：模式状态机 / 内核生命周期 / 订阅管理 / 延迟测试。
//!
//! 高危安全语义（docs/impl/05 风险标注）：
//! - 内核**意外退出** → 立即还原系统代理 + 模式归零（死端口 = 断网最高危）
//! - TUN 与系统代理**互斥**：开 TUN 前强制还原系统代理
//! - 所有还原走 `sysproxy::restore*`（备份优先，绝不猜用户原值）

use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use host_core::events::{Event, EventBus};
use host_core::ports::SysProxyPort;
use serde::{Deserialize, Serialize};

use crate::error::{ProxyError, Result};
use crate::ir;
use crate::kernel::{driver_for, KernelCaps, KernelDriver, KernelHandle, LogLine, KERNEL_IDS};
use crate::rules::{self, RuleV2, RulesV2};
use crate::sidecar;
use crate::sub::{Node, TrafficInfo};
use crate::sysproxy;

const STATE_FILE: &str = "proxy_state.json";
const SUBS_FILE: &str = "subs.json";
const RULES_FILE: &str = "rules.json";
/// T-B2-9 分流规则 v2 数据文件（缺失=open 时由 rules.json 派生，首次保存才落盘）
const RULES_V2_FILE: &str = "rules_v2.json";
const BIN_DIR: &str = "bin";
const SUBS_DIR: &str = "subs";
const APP_UA: &str = concat!("NexusForge/", env!("CARGO_PKG_VERSION"));
const FETCH_RETRIES: u32 = 3;
const HEALTH_WAIT_MS: u64 = 3000;

/// 运行模式（UI 三态；Off = 内核停止 + 系统代理还原）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    System,
    Tun,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::System => "system",
            Self::Tun => "tun",
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "off" => Ok(Self::Off),
            "system" => Ok(Self::System),
            "tun" => Ok(Self::Tun),
            _ => Err(ProxyError::BadState(format!("未知模式: {s}"))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sub {
    pub id: String,
    pub name: String,
    pub url: String,
    pub updated_ms: u64,
    pub node_count: usize,
    /// T-B2-10 订阅标准头（None = 面板未发/从未解析，UI 不谎显）；
    /// serde default 零迁移：旧 subs.json 缺键 = None
    #[serde(default)]
    pub traffic: Option<TrafficInfo>,
    #[serde(default)]
    pub interval_min: Option<u64>,
    /// If-None-Match 条件请求指纹：304 命中只刷 updated_ms，节点不动
    #[serde(default)]
    pub etag: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistState {
    mixed_port: u16,
    /// 选定内核（T-B2-2）；serde default 零迁移：旧文件缺键 = sing-box
    #[serde(default = "default_kernel")]
    kernel: String,
    /// 手动选定出口节点 (sub_id, tag)（T-B2-11）；serde default 零迁移：
    /// 缺键/None = 自动（urltest 组首选），旧 proxy_state.json 原样可读
    #[serde(default)]
    selected_node: Option<(String, String)>,
}

fn default_kernel() -> String {
    KERNEL_SINGBOX.to_string()
}

/// 当前唯一注册内核 id（T-B2-5/6 增 xray/mihomo）
pub const KERNEL_SINGBOX: &str = "sing-box";

/// 安装入口内核解析（T-B2-4）：kernel 缺省 None = "sing-box"（旧调用兼容）；
/// 未知/未接入下载通道的内核 id 在任何触网前由 [`sidecar::asset_for`] 如实拒。
/// 纯函数以便单测钉住 async 安装路径（tauri State 在单测不可构造，命令层透传由此 seam 覆盖）。
pub fn resolve_install_kernel(kernel: Option<&str>) -> Result<&'static sidecar::AssetSpec> {
    sidecar::asset_for(kernel.unwrap_or(KERNEL_SINGBOX))
}

/// geo 渲染预检（T-B2-10）：规则引用的每个 geo 类，本内核所需本地资产必须已落盘。
/// mihomo [`KernelDriver::geo_asset_ids`] 空集 = 原生托管自取是合法形态，天然放行。
fn check_geo_prereqs(
    driver: &dyn KernelDriver,
    proxy_dir: &Path,
    user_rules: &[ir::IrRule],
) -> Result<()> {
    let mut cats: Vec<&'static str> = Vec::new();
    for r in user_rules {
        if let Some(c) = r.field.geo_category() {
            if !cats.contains(&c) {
                cats.push(c);
            }
        }
    }
    for cat in cats {
        for id in driver.geo_asset_ids(cat) {
            let spec = sidecar::artifact_for(id)?;
            if !sidecar::artifact_installed(spec, proxy_dir) {
                return Err(ProxyError::Config(format!(
                    "分流规则引用 {cat}，但所需 {} 未安装（{id}）：请在代理页·内核区安装",
                    spec.label
                )));
            }
        }
    }
    Ok(())
}

/// [`ProxyService::http_get_ex`] 的结果形态：304 命中 = not_modified 且无正文/头
struct FetchOutcome {
    not_modified: bool,
    headers: BTreeMap<String, String>,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusDto {
    pub mode: String,
    pub kernel_running: bool,
    pub kernel_id: Option<String>,
    pub inbound_port: u16,
    pub nodes_total: usize,
    pub subs_total: usize,
    pub admin: bool,
    pub wintun_installed: bool,
    pub kernel_installed: bool,
    pub kernel_version: Option<String>,
    pub has_backup: bool,
    /// 上次启动扫描是否自动还原了残留代理（UI 提示）
    pub restored_last_run: bool,
    /// 选定内核 id（持久化于 proxy_state.json；UI 内核卡高亮）
    pub kernel: String,
    /// 全部已注册内核的装机/能力清单（内核选择卡数据源）
    pub kernels: Vec<KernelInfoDto>,
    /// geo/数据资产装机清单（T-B2-10，GEO_ASSETS 数据驱动，UI 禁资产特例分支）
    pub artifacts: Vec<ArtifactStatusDto>,
    /// 手动选定出口节点 (sub_id, tag)（T-B2-11）；None = 自动（urltest 组首选）
    pub selected_node: Option<(String, String)>,
    /// 选定节点因订阅更新/能力过滤已不在场 = 本代配置实际回落首节点，如实上报
    /// （UI 出口灯/节点行徽章据此显示"选定已失效"而非假装仍在生效）
    pub selected_stale: bool,
}

/// 出口自检结果（T-B2-11）：经本地 mixed 出口 GET gstatic generate_204。
/// 内核未运行是 BadState 错误（不假 200），可达但出口不通 = ok:false 的正常结果。
#[derive(Clone, Debug, Serialize)]
pub struct EgressProbeDto {
    pub ok: bool,
    pub ms: Option<u64>,
    pub status: Option<u16>,
}

/// 单资产条目（sidecar::GEO_ASSETS 镜像 + manifest 版本对账）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactStatusDto {
    pub id: String,
    pub label: String,
    pub installed: bool,
    pub version: Option<String>,
}

/// 单内核条目（注册表驱动，UI 禁写内核特例分支）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KernelInfoDto {
    pub id: String,
    pub display_name: String,
    pub installed: bool,
    pub version: Option<String>,
    /// 该内核当前是否就是运行中的句柄
    pub running: bool,
    pub caps: KernelCaps,
    pub supported_kinds: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NodeDto {
    pub tag: String,
    pub kind: String,
    pub server: String,
    pub port: u16,
    pub sub_id: String,
    /// T-B2-8：Clash YAML 订阅内该节点所属 proxy-groups 组名（UI 过滤列；
    /// URI 路订阅恒空数组，收窄登记见 clash_yaml 模块头）
    pub groups: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NodeDelayDto {
    pub tag: String,
    pub sub_id: String,
    /// TCP 连接延迟毫秒；None = 3s 超时不可达
    pub ms: Option<u64>,
}

struct Inner {
    mode: Mode,
    handle: Option<KernelHandle>,
    mixed_port: u16,
    kernel: String,
    subs: Vec<Sub>,
    nodes: Vec<Node>,
    /// T-B2-9：分流规则 v2（旧直连域名清单是其 suffix+direct 投影）
    rules_v2: RulesV2,
    /// T-B2-11：手动选定出口 (sub_id, tag)；None = 自动（urltest）
    selected_node: Option<(String, String)>,
}

pub struct ProxyService {
    proxy_dir: PathBuf,
    bus: Arc<EventBus>,
    sp: Arc<dyn SysProxyPort>,
    inner: RwLock<Inner>,
    restored_last_run: RwLock<bool>,
    /// 测试专用驱动注入位（换核生命周期须真实双进程；生产路径恒空）
    #[cfg(test)]
    test_drivers: RwLock<Vec<(String, Arc<dyn KernelDriver>)>>,
    /// 测试注入：这些 tag 的 delay 任务必 panic（缺陷⑨ 定长向量红线入口）
    #[cfg(test)]
    delay_panic_tags: RwLock<Vec<String>>,
}

impl ProxyService {
    /// 打开服务：加载持久化 + 启动扫描还原 kill -9 残留（验收项）
    pub fn open(
        app_data_dir: &Path,
        bus: Arc<EventBus>,
        sp: Arc<dyn SysProxyPort>,
    ) -> Result<Arc<Self>> {
        let proxy_dir = app_data_dir.join("proxy");
        std::fs::create_dir_all(proxy_dir.join(SUBS_DIR))?;
        std::fs::create_dir_all(proxy_dir.join(BIN_DIR))?;

        let state: PersistState = std::fs::read(proxy_dir.join(STATE_FILE))
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or(PersistState {
                mixed_port: 7890,
                kernel: default_kernel(),
                selected_node: None,
            });
        let legacy_domains: Vec<String> = std::fs::read(proxy_dir.join(RULES_FILE))
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        // v2 优先；缺失/损坏视同缺失 → 旧清单派生（不落盘，首次保存才物化=零迁移）
        let rules_v2: RulesV2 = std::fs::read(proxy_dir.join(RULES_V2_FILE))
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_else(|| rules::derive_legacy(&legacy_domains));

        // 订阅元数据 + 节点文件加载
        let (subs, nodes) = load_subs(&proxy_dir)?;

        let svc = Arc::new(Self {
            proxy_dir,
            bus,
            sp,
            inner: RwLock::new(Inner {
                mode: Mode::Off,
                handle: None,
                mixed_port: state.mixed_port,
                kernel: state.kernel.clone(),
                selected_node: state.selected_node.clone(),
                subs,
                nodes,
                rules_v2,
            }),
            restored_last_run: RwLock::new(false),
            #[cfg(test)]
            test_drivers: RwLock::new(Vec::new()),
            #[cfg(test)]
            delay_panic_tags: RwLock::new(Vec::new()),
        });

        // 启动扫描：上次运行残留的系统代理 → 自动还原（验收「kill -9 → 重启恢复」）
        let restored = sysproxy::restore_if_ours(&svc.proxy_dir, svc.sp.as_ref(), state.mixed_port)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "启动扫描还原系统代理失败");
                false
            });
        *svc.restored_last_run.write() = restored;

        Ok(svc)
    }

    pub fn status(&self) -> StatusDto {
        let inner = self.inner.read();
        let running_id = inner.handle.as_ref().map(|h| h.driver_id().to_string());
        let kernels: Vec<KernelInfoDto> = KERNEL_IDS
            .iter()
            .filter_map(|id| driver_for(&self.bin_dir(), id).ok())
            .map(|d| KernelInfoDto {
                id: d.id().to_string(),
                display_name: d.display_name().to_string(),
                installed: d.exe_path().is_file(),
                // per-kernel manifest（T-B2-4）：manifest-<id>.json 优先，
                // 旧单文件仅 sing-box 兼容读（零迁移）
                version: sidecar::read_manifest(&self.bin_dir(), d.id()).map(|m| m.kernel_version),
                running: running_id.as_deref() == Some(d.id()),
                caps: d.caps(),
                supported_kinds: d
                    .supported_kinds()
                    .iter()
                    .map(|k| k.as_str().to_string())
                    .collect(),
            })
            .collect();
        let selected = kernels.iter().find(|k| k.id == inner.kernel);
        let artifacts = sidecar::GEO_ASSETS
            .iter()
            .map(|a| ArtifactStatusDto {
                id: a.id.to_string(),
                label: a.label.to_string(),
                installed: sidecar::artifact_installed(a, &self.proxy_dir),
                version: sidecar::read_artifact_manifest(&self.proxy_dir, a.id)
                    .map(|m| m.kernel_version),
            })
            .collect();
        StatusDto {
            mode: inner.mode.as_str().into(),
            kernel_running: inner.handle.as_ref().map(|h| h.alive()).unwrap_or(false),
            kernel_id: running_id,
            inbound_port: inner.mixed_port,
            nodes_total: inner.nodes.len(),
            subs_total: inner.subs.len(),
            admin: self.sp.is_admin(),
            wintun_installed: sidecar::wintun_installed(&self.bin_dir()),
            // 旧两字段的"内核"语义 = 选定内核（sing-box 时代与旧行为重合）
            kernel_installed: selected.map(|k| k.installed).unwrap_or(false),
            kernel_version: selected.and_then(|k| k.version.clone()),
            has_backup: sysproxy::has_backup(&self.proxy_dir),
            restored_last_run: *self.restored_last_run.read(),
            kernel: inner.kernel.clone(),
            kernels,
            artifacts,
            selected_node: inner.selected_node.clone(),
            // 失效判定与 apply_selected 的回落条件同源：(sub_id, tag) 不在现节点表
            selected_stale: inner.selected_node.as_ref().is_some_and(|(sid, tag)| {
                !inner
                    .nodes
                    .iter()
                    .any(|n| &n.sub_id == sid && &n.tag == tag)
            }),
        }
    }

    /// 内核安装（PR2；T-B2-4 资产表泛化）：kernel 缺省 "sing-box"（旧调用兼容），
    /// 未知内核在触网前被 [`sidecar::asset_for`] 如实拒。官方 Release 直链下载 →
    /// zip 校验解压 → per-kernel manifest + TOFU pin（ack_pin 恒 false：UI 确认通道归 B9，
    /// 本行只保证"篡改面不因缺 UI 而静默放行"）。
    pub async fn kernel_install(
        &self,
        kernel: Option<&str>,
        version: Option<String>,
    ) -> Result<sidecar::Manifest> {
        let spec = resolve_install_kernel(kernel)?;
        let version = version.unwrap_or_else(|| spec.default_version.to_string());
        let url = (spec.url_for)(&version);
        let bytes = self
            .http_get_ex(&url, None, Self::MAX_BIN_BYTES)
            .await?
            .bytes;
        let manifest =
            sidecar::install_binary_from_zip(spec, &self.bin_dir(), &bytes, &version, false)?;
        tracing::info!(kernel = %spec.id, version = %version, sha256 = %manifest.sha256, "内核安装完成");
        self.publish_state();
        Ok(manifest)
    }

    /// wintun.dll 安装（PR5 TUN 前置；T-B2-4 起与内核走同一 install_binary_from_zip 通道）
    pub async fn wintun_install(&self) -> Result<()> {
        let spec = &sidecar::WINTUN_ASSET;
        let url = (spec.url_for)(spec.default_version);
        let bytes = self
            .http_get_ex(&url, None, Self::MAX_BIN_BYTES)
            .await?
            .bytes;
        sidecar::install_binary_from_zip(
            spec,
            &self.bin_dir(),
            &bytes,
            spec.default_version,
            false,
        )?;
        self.publish_state();
        Ok(())
    }

    /// geo 数据资产安装（T-B2-10，Binary 资产唯一写盘入口收口于
    /// [`sidecar::install_raw_asset`]）：拉取 → checksum_url 在场则 fail-closed 校验
    /// （官方 .sha256sum 首个空白分词 vs 本地计算，不一致在任何写盘前 Integrity 拒）
    /// → TOFU pin + tmp/rename。版本恒 spec.default_version（mihomo 滚动 latest
    /// 的 pin 版本键恒 "latest"，无版本手选面）。
    pub async fn artifact_install(
        &self,
        artifact: &str,
        ack_pin: bool,
    ) -> Result<sidecar::Manifest> {
        let spec = sidecar::artifact_for(artifact)?;
        let version = spec.default_version;
        let bytes = self
            .http_get_ex(&(spec.url_for)(version), None, Self::MAX_BIN_BYTES)
            .await?
            .bytes;
        if let Some(sha_url) = spec.checksum_url {
            let sha_raw = self
                .http_get_ex(&(sha_url)(version), None, Self::MAX_SHA_BYTES)
                .await?
                .bytes;
            let sha_text = String::from_utf8_lossy(&sha_raw);
            let expected = sha_text
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let actual = sidecar::sha256_hex(&bytes);
            if expected.is_empty() || expected != actual {
                return Err(ProxyError::Integrity(format!(
                    "{} 官方校验和与下载字节不一致（期望 {expected}，实际 {actual}）：包可能被篡改，未安装",
                    spec.label
                )));
            }
        }
        let manifest = sidecar::install_raw_asset(spec, &self.proxy_dir, &bytes, version, ack_pin)?;
        tracing::info!(artifact = %spec.id, version, sha256 = %manifest.sha256, "geo 资产安装完成");
        self.publish_state();
        Ok(manifest)
    }

    /// 订阅/文本类上限（SEC-10：gzip bomb/巨响应内存耗尽防护）
    const MAX_SUB_BYTES: usize = 8 * 1024 * 1024;
    /// 内核/wintun/geo 二进制上限
    const MAX_BIN_BYTES: usize = 256 * 1024 * 1024;
    /// 校验和文本上限
    const MAX_SHA_BYTES: usize = 64 * 1024;

    /// 统一 HTTP 拉取（rustls + 显式 UA；订阅重试上限 3 次）
    #[allow(dead_code)] // SEC-10 后调用方全部改走 http_get_ex 显式上限；包装保留作缺省上限入口
    async fn http_get(&self, url: &str) -> Result<Vec<u8>> {
        Ok(self
            .http_get_ex(url, None, Self::MAX_SUB_BYTES)
            .await?
            .bytes)
    }

    /// 带条件请求的拉取（T-B2-10 订阅 304 面）：`if_none_match` 附 If-None-Match 头，
    /// 命中 304 短路返回（零字节、零头）。响应头一律小写键归一（reqwest/HTTP2 内部
    /// 表示即小写，parse_sub_headers 约定单一真源）。非 2xx 语义沿用旧 http_get
    /// （不检查状态码——订阅面板方言混杂，正文解析失败自有诚实报错）。
    /// 带条件请求的拉取（T-B2-10 订阅 304 面）：`if_none_match` 附 If-None-Match 头，
    /// 命中 304 短路返回（零字节、零头）。响应头一律小写键归一（reqwest/HTTP2 内部
    /// 表示即小写，parse_sub_headers 约定单一真源）。
    ///
    /// SEC-10：① 重定向 ≤3 跳；② 非 2xx 明确报错（不再"不检查状态码"——错误页
    /// 被当订阅解析是更差的谎报）；③ content_length 预检 + 流式累计硬上限
    /// （gzip bomb 与谎报头均不可绕过）。
    async fn http_get_ex(
        &self,
        url: &str,
        if_none_match: Option<&str>,
        max_bytes: usize,
    ) -> Result<FetchOutcome> {
        let client = reqwest::Client::builder()
            .user_agent(APP_UA)
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .map_err(|e| ProxyError::Download(format!("HTTP 客户端构建失败: {e}")))?;
        let mut last_err = String::new();
        for attempt in 0..FETCH_RETRIES {
            let mut req = client.get(url);
            if let Some(etag) = if_none_match {
                req = req.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            match req.send().await {
                Ok(resp) => {
                    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
                        return Ok(FetchOutcome {
                            not_modified: true,
                            headers: BTreeMap::new(),
                            bytes: Vec::new(),
                        });
                    }
                    let status = resp.status();
                    if !status.is_success() {
                        return Err(ProxyError::Download(format!(
                            "下载失败：HTTP {status}（{url}）"
                        )));
                    }
                    if let Some(len) = resp.content_length() {
                        if len as usize > max_bytes {
                            return Err(ProxyError::Download(format!(
                                "响应过大：{len} > {max_bytes} 字节上限（{url}）"
                            )));
                        }
                    }
                    let headers = resp
                        .headers()
                        .iter()
                        .filter_map(|(k, v)| {
                            v.to_str()
                                .ok()
                                .map(|s| (k.as_str().to_lowercase(), s.to_string()))
                        })
                        .collect();
                    // 流式累计：content_length 谎报/缺失时以实际字节兜底
                    let mut bytes: Vec<u8> = Vec::new();
                    let mut stream = resp;
                    loop {
                        match stream.chunk().await {
                            Ok(Some(chunk)) => {
                                if bytes.len() + chunk.len() > max_bytes {
                                    return Err(ProxyError::Download(format!(
                                        "下载超过 {max_bytes} 字节上限（{url}）"
                                    )));
                                }
                                bytes.extend_from_slice(&chunk);
                            }
                            Ok(None) => {
                                return Ok(FetchOutcome {
                                    not_modified: false,
                                    headers,
                                    bytes,
                                });
                            }
                            Err(e) => {
                                last_err = format!("读取响应体失败: {e}");
                                break;
                            }
                        }
                    }
                }
                Err(e) => last_err = format!("请求失败: {e}"),
            }
            if attempt + 1 < FETCH_RETRIES {
                tokio::time::sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
            }
        }
        Err(ProxyError::Download(format!(
            "订阅拉取失败（重试 {FETCH_RETRIES} 次）: {last_err}"
        )))
    }

    // ---------------- 订阅管理（PR3） ----------------

    pub fn subs(&self) -> Vec<Sub> {
        self.inner.read().subs.clone()
    }

    pub fn sub_add(&self, name: &str, url: &str) -> Result<Sub> {
        if url.trim().is_empty() {
            return Err(ProxyError::Subscription("订阅 URL 不能为空".into()));
        }
        let sub = Sub {
            id: uuid::Uuid::now_v7().to_string(),
            name: if name.trim().is_empty() {
                "订阅".into()
            } else {
                name.trim().to_string()
            },
            url: url.trim().to_string(),
            updated_ms: 0,
            node_count: 0,
            traffic: None,
            interval_min: None,
            etag: None,
        };
        let mut inner = self.inner.write();
        inner.subs.push(sub.clone());
        self.save_subs(&inner)?;
        Ok(sub)
    }

    pub fn sub_remove(&self, id: &str) -> Result<bool> {
        let mut inner = self.inner.write();
        let before = inner.subs.len();
        inner.subs.retain(|s| s.id != id);
        let removed = inner.subs.len() != before;
        inner.nodes.retain(|n| n.sub_id != id);
        self.save_subs(&inner)?;
        drop(inner);
        let _ = std::fs::remove_file(self.sub_nodes_path(id));
        self.publish_nodes(Some(id));
        Ok(removed)
    }

    /// 拉取并解析订阅（PR3；T-B2-10 条件请求 + 标准头消费）；内容持久化到 subs/（敏感，不进日志）。
    /// 304 命中 = 只刷 updated_ms（节点与流量头原样）；200 时 traffic 头在场才覆盖，
    /// interval_min/etag 头缺省保留旧值——面板不发头不算"用户清空"。
    pub async fn sub_update(&self, id: &str) -> Result<Sub> {
        let (url, old_etag) = {
            let inner = self.inner.read();
            inner
                .subs
                .iter()
                .find(|s| s.id == id)
                .map(|s| (s.url.clone(), s.etag.clone()))
                .ok_or_else(|| ProxyError::NotFound(format!("订阅 {id} 不存在")))?
        };
        let fetched = self
            .http_get_ex(&url, old_etag.as_deref(), Self::MAX_SUB_BYTES)
            .await?;
        if fetched.not_modified {
            return self.apply_sub_not_modified(id);
        }
        let headers = crate::sub::parse_sub_headers(&fetched.headers);
        let content = String::from_utf8_lossy(&fetched.bytes[..]);
        let nodes = crate::sub::parse_subscription(&content, id)?;
        let node_count = nodes.len();

        // 持久化节点文件 + 更新元数据
        host_core::util::write_atomic(&self.sub_nodes_path(id), &serde_json::to_vec(&nodes)?)?;
        let sub = {
            let mut inner = self.inner.write();
            inner.nodes.retain(|n| n.sub_id != id);
            inner.nodes.extend(nodes);
            let idx = inner
                .subs
                .iter()
                .position(|s| s.id == id)
                .ok_or_else(|| ProxyError::NotFound(format!("订阅 {id} 不存在")))?;
            let sub = &mut inner.subs[idx];
            sub.updated_ms = now_ms();
            sub.node_count = node_count;
            if headers.traffic.is_some() {
                sub.traffic = headers.traffic;
            }
            if headers.interval_min.is_some() {
                sub.interval_min = headers.interval_min;
            }
            if headers.etag.is_some() {
                sub.etag = headers.etag;
            }
            sub.clone()
        };
        self.save_subs(&self.inner.read())?;
        self.publish_nodes(Some(id));
        Ok(sub)
    }

    /// 304 命中的落账（红线语义：正文与节点文件一字不动，仅"上次成功更新"时刻刷新；
    /// 不发 sub_update 事件——节点集未变，UI 无需失效重取）
    fn apply_sub_not_modified(&self, id: &str) -> Result<Sub> {
        let sub = {
            let mut inner = self.inner.write();
            let idx = inner
                .subs
                .iter()
                .position(|s| s.id == id)
                .ok_or_else(|| ProxyError::NotFound(format!("订阅 {id} 不存在")))?;
            inner.subs[idx].updated_ms = now_ms();
            inner.subs[idx].clone()
        };
        self.save_subs(&self.inner.read())?;
        Ok(sub)
    }

    pub fn nodes(&self) -> Vec<NodeDto> {
        let inner = self.inner.read();
        inner
            .nodes
            .iter()
            .map(|n| NodeDto {
                tag: n.tag.clone(),
                kind: n.kind.as_str().into(),
                server: n.server.clone(),
                port: n.port,
                sub_id: n.sub_id.clone(),
                groups: n.groups.clone(),
            })
            .collect()
    }

    // ---------------- 直连规则（v1 投影）与分流规则 v2（T-B2-9） ----------------

    /// 旧命令语义：v2 表的 suffix+direct 投影（读写双向兼容，rules.json 恒双写）
    pub fn direct_rules(&self) -> Vec<String> {
        self.inner
            .read()
            .rules_v2
            .rules
            .iter()
            .filter(|r| r.kind == "suffix" && r.target == "direct" && r.enabled)
            .map(|r| r.pattern.clone())
            .collect()
    }

    /// 旧命令语义"全量替换直连集"：仅置换 suffix+direct 桶，其余 v2 规则不动；
    /// 双写 rules.json（回滚兼容）+ rules_v2.json（物化）。
    /// 去重=保序首现 retain（缺陷随行修：旧 `Vec::dedup` 只删相邻重复）。
    pub fn set_direct_rules(&self, rules: Vec<String>) -> Result<()> {
        let mut seen: Vec<String> = Vec::new();
        for r in rules {
            let r = r.trim().to_string();
            if !r.is_empty() && !seen.contains(&r) {
                seen.push(r);
            }
        }
        let mut inner = self.inner.write();
        inner
            .rules_v2
            .rules
            .retain(|r| !(r.kind == "suffix" && r.target == "direct"));
        inner
            .rules_v2
            .rules
            .extend(seen.iter().cloned().map(|pattern| RuleV2 {
                kind: "suffix".to_string(),
                pattern,
                target: "direct".to_string(),
                enabled: true,
            }));
        host_core::util::write_atomic(
            &self.proxy_dir.join(RULES_FILE),
            &serde_json::to_vec(&seen)?,
        )?;
        host_core::util::write_atomic(
            &self.proxy_dir.join(RULES_V2_FILE),
            &serde_json::to_vec(&inner.rules_v2)?,
        )?;
        Ok(())
    }

    pub fn rules_v2(&self) -> RulesV2 {
        self.inner.read().rules_v2.clone()
    }

    /// 全量写 v2 表：[`rules::sanitize`] 是唯一校验闸（CIDR/域形态/进程名/枚举白名单，
    /// 违规 Config 点名字段值）；rules.json 随写投影保持双文件一致。
    pub fn set_rules_v2(&self, v2: RulesV2) -> Result<()> {
        let v2 = rules::sanitize(&v2)?;
        let legacy: Vec<String> = v2
            .rules
            .iter()
            .filter(|r| r.kind == "suffix" && r.target == "direct" && r.enabled)
            .map(|r| r.pattern.clone())
            .collect();
        host_core::util::write_atomic(
            &self.proxy_dir.join(RULES_V2_FILE),
            &serde_json::to_vec(&v2)?,
        )?;
        host_core::util::write_atomic(
            &self.proxy_dir.join(RULES_FILE),
            &serde_json::to_vec(&legacy)?,
        )?;
        self.inner.write().rules_v2 = v2;
        Ok(())
    }

    /// mixed 端口（设置中心可改）。System 态下改端口会**同步重写注册表**
    /// （SEC-09：不变式"代理开启 ⇒ 注册表 server == our_server(当前端口)"），
    /// 否则改端口后强杀，残留识别会把旧端口判为"用户改过"→ 删备份不还原 → 断网。
    pub fn set_mixed_port(&self, port: u16) -> Result<()> {
        if port == 0 {
            return Err(ProxyError::BadState("端口不能为 0".into()));
        }
        let mut inner = self.inner.write();
        let was_system = inner.mode == Mode::System;
        inner.mixed_port = port;
        self.persist_state(&inner)?;
        drop(inner);
        if was_system {
            if let Err(e) = sysproxy::enable(&self.proxy_dir, self.sp.as_ref(), port) {
                return Err(ProxyError::SysProxy(format!(
                    "System 态改端口重写注册表失败（已持久化新端口，请重切一次系统代理）: {e}"
                )));
            }
        }
        Ok(())
    }

    /// 持久化状态文件唯一写点（整包写，杜绝 set_mixed_port 曾有的 kernel 覆盖；
    /// T-B2-11 起 selected_node 同包持久化）
    fn persist_state(&self, inner: &Inner) -> Result<()> {
        host_core::util::write_atomic(
            &self.proxy_dir.join(STATE_FILE),
            &serde_json::to_vec(&PersistState {
                mixed_port: inner.mixed_port,
                kernel: inner.kernel.clone(),
                selected_node: inner.selected_node.clone(),
            })?,
        )?;
        Ok(())
    }

    // ---------------- 模式状态机（PR4/PR5 核心） ----------------

    /// 切换模式。同步方法（内核 spawn / 注册表写均为毫秒级），IPC 层 spawn_blocking。
    /// `self: &Arc<Self>`：on_exit 回调需要 Weak 引用避免 Service↔Handle 循环持有。
    pub fn set_mode(self: &Arc<Self>, mode: Mode) -> Result<()> {
        // 同模式重入 = 显式 no-op（旧行为会白白重启一次内核）
        if self.inner.read().mode == mode {
            return Ok(());
        }
        match mode {
            Mode::Off => self.stop_kernel_and_restore(),
            Mode::System => self.enter_system(),
            Mode::Tun => self.enter_tun(),
        }
    }

    fn enter_system(self: &Arc<Self>) -> Result<()> {
        // TUN → System 切换：先停旧内核；起核失败不得留 stale mode（⑧同型窗口，归零如实 Off）
        if let Err(e) = self.restart_with_config(false) {
            let _ = self.stop_kernel_and_restore();
            return Err(e);
        }
        // 系统代理：备份原值 → 写入我们的 mixed 入站
        let port = self.inner.read().mixed_port;
        sysproxy::enable(&self.proxy_dir, self.sp.as_ref(), port)?;
        {
            let mut inner = self.inner.write();
            inner.mode = Mode::System;
        }
        self.publish_state();
        tracing::info!(port, "系统代理已启用");
        Ok(())
    }

    fn enter_tun(self: &Arc<Self>) -> Result<()> {
        // 内核能力门禁（T-B2-5 红线）：caps.tun=false 的内核（xray 主线）在一切
        // 权限/组件噪音之前如实拒——用户先看到的是"换核"这一真正解法
        let kernel = self.inner.read().kernel.clone();
        if !self.driver(&kernel)?.caps().tun {
            return Err(ProxyError::BadState(
                "当前内核不支持 TUN：请换 sing-box/mihomo".into(),
            ));
        }
        if !self.sp.is_admin() {
            return Err(ProxyError::Permission("TUN 模式需要管理员权限".into()));
        }
        if !sidecar::wintun_installed(&self.bin_dir()) {
            return Err(ProxyError::BadState(
                "wintun.dll 未安装：请先安装 TUN 组件".into(),
            ));
        }
        // 互斥：TUN 接管全流量，系统代理必须还原（否则双重代理）
        sysproxy::restore_quiet(&self.proxy_dir, self.sp.as_ref());
        // 缺陷⑧随行修：起核失败不得留下 stale mode——立即归零 + 还原，状态如实 Off
        if let Err(e) = self.restart_with_config(true) {
            let _ = self.stop_kernel_and_restore();
            return Err(e);
        }
        let mut inner = self.inner.write();
        inner.mode = Mode::Tun;
        drop(inner);
        self.publish_state();
        tracing::info!("TUN 模式已启用（系统代理已强制关闭）");
        Ok(())
    }

    fn stop_kernel_and_restore(&self) -> Result<()> {
        {
            let mut inner = self.inner.write();
            if let Some(h) = inner.handle.take() {
                h.stop();
            }
            inner.mode = Mode::Off;
        }
        // 无论模式是什么都还原一次（幂等；备份不存在时保守关闭开关）
        sysproxy::restore_quiet(&self.proxy_dir, self.sp.as_ref());
        self.publish_state();
        Ok(())
    }

    /// 换内核（T-B2-2）。Off 态只改选择并持久化；运行态以当前模式配置重启新核，
    /// 起新核失败 = 回滚旧核（系统代理从未被还原过、端口复用即恢复，无需重挂）。
    /// 持久化文件只写成功态——失败回滚后磁盘上仍是旧核。
    pub fn set_kernel(self: &Arc<Self>, id: &str) -> Result<()> {
        self.driver(id)?; // 未知 id 在任何状态变更前被拒（restart 内部会再解析同一驱动）
        let (old, mode) = {
            let inner = self.inner.read();
            (inner.kernel.clone(), inner.mode)
        };
        if old == id {
            return Ok(());
        }
        self.inner.write().kernel = id.to_string();
        if mode == Mode::Off {
            let inner = self.inner.read();
            self.persist_state(&inner)?;
            drop(inner);
            self.publish_state();
            tracing::info!(kernel = %id, "代理内核选择已更新（未运行，下次启动生效）");
            return Ok(());
        }
        let tun = mode == Mode::Tun;
        match self.restart_with_config(tun) {
            Ok(()) => {
                let inner = self.inner.read();
                self.persist_state(&inner)?;
                drop(inner);
                self.publish_state();
                tracing::info!(kernel = %id, "运行中换核完成");
                Ok(())
            }
            Err(e) => {
                self.inner.write().kernel = old.clone();
                if let Err(re) = self.restart_with_config(tun) {
                    // 回滚也起不来：兜底归零（断网最高危场景纪律），合并上报两错
                    let _ = self.stop_kernel_and_restore();
                    return Err(ProxyError::Kernel(format!(
                        "切换内核 {id} 失败：{e}；回滚原内核同样失败：{re}（已停止为关闭态）"
                    )));
                }
                self.publish_state();
                Err(e)
            }
        }
    }

    /// 内核重启（T-B2-3）：仅运行中有意义——停旧进程 → 以当前模式重生成配置 → 起新，
    /// mode/kernel 均不变（System 态系统代理注册表值从未被还原，端口复用即恢复）。
    /// 起新失败按缺陷⑧纪律归零（旧进程已停无从回滚，restore 兜底 + 状态如实 Off）。
    pub fn restart_kernel(self: &Arc<Self>) -> Result<()> {
        let mode = {
            let inner = self.inner.read();
            if inner.handle.is_none() {
                return Err(ProxyError::BadState("内核未在运行：无需重启".into()));
            }
            inner.mode
        };
        let tun = mode == Mode::Tun;
        if let Err(e) = self.restart_with_config(tun) {
            let _ = self.stop_kernel_and_restore();
            return Err(ProxyError::Kernel(format!(
                "内核重启失败：{e}（已停止为关闭态）"
            )));
        }
        tracing::info!(mode = mode.as_str(), "内核已重启");
        Ok(())
    }

    // ---------------- 手动选节点（T-B2-11） ----------------

    /// 选定出口节点：存在性校验不过即 NotFound 拒（红线：不存在的节点不写状态，
    /// 幽灵选定会在下一次 restart 才爆成内核拒写）。运行中按当前模式重启生效
    /// （selector.default 换人），失败走缺陷⑧同款纪律：归零关闭态 + Kernel 上抛。
    pub fn select_node(self: &Arc<Self>, sub_id: &str, tag: &str) -> Result<()> {
        {
            let inner = self.inner.read();
            if !inner
                .nodes
                .iter()
                .any(|n| n.sub_id == sub_id && n.tag == tag)
            {
                return Err(ProxyError::NotFound(format!(
                    "节点不存在：{sub_id}/{tag}（请先更新订阅核对节点名）"
                )));
            }
        }
        let mode = {
            let mut inner = self.inner.write();
            inner.selected_node = Some((sub_id.to_string(), tag.to_string()));
            self.persist_state(&inner)?;
            inner.mode
        };
        if mode == Mode::Off || self.inner.read().handle.is_none() {
            self.publish_state();
            return Ok(());
        }
        let tun = mode == Mode::Tun;
        if let Err(e) = self.restart_with_config(tun) {
            let _ = self.stop_kernel_and_restore();
            return Err(ProxyError::Kernel(format!(
                "选定节点后内核重启失败：{e}（已停止为关闭态）"
            )));
        }
        Ok(())
    }

    /// 回到自动出口（清 selected_node；语义 = urltest 组自选，非"停"）
    pub fn set_node_auto(self: &Arc<Self>) -> Result<()> {
        let mode = {
            let mut inner = self.inner.write();
            inner.selected_node = None;
            self.persist_state(&inner)?;
            inner.mode
        };
        if mode == Mode::Off || self.inner.read().handle.is_none() {
            self.publish_state();
            return Ok(());
        }
        let tun = mode == Mode::Tun;
        if let Err(e) = self.restart_with_config(tun) {
            let _ = self.stop_kernel_and_restore();
            return Err(ProxyError::Kernel(format!(
                "切回自动出口后内核重启失败：{e}（已停止为关闭态）"
            )));
        }
        Ok(())
    }

    /// 出口自检（02§5-1 种子）：reqwest 走本地 mixed 代理 GET gstatic 204，5s 超时。
    /// 内核未运行 = BadState 如实（不假 200 也不假 ok:false）；TUN 态本代无本地
    /// mixed 入站，同样 BadState 点名（出口面归 B7 外部 API 演进）。
    /// 连通但出口不通（内核在跑、上游全挂）= Ok(EgressProbeDto{ok:false})——这是
    /// 自检的正常失败结果而非命令错误。
    pub async fn egress_probe(&self) -> Result<EgressProbeDto> {
        let (running, tun, port) = {
            let inner = self.inner.read();
            (
                inner.handle.as_ref().is_some_and(|h| h.alive()),
                inner.mode == Mode::Tun,
                inner.mixed_port,
            )
        };
        if !running {
            return Err(ProxyError::BadState(
                "内核未在运行：无出口可自检：请先切换系统代理/TUN 模式".into(),
            ));
        }
        if tun {
            return Err(ProxyError::BadState(
                "TUN 模式下代配置无本地 mixed 入站：出口自检暂不可用".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .proxy(
                reqwest::Proxy::all(format!("http://127.0.0.1:{port}"))
                    .map_err(|e| ProxyError::BadState(format!("自检代理地址构造失败: {e}")))?,
            )
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| ProxyError::BadState(format!("自检客户端构造失败: {e}")))?;
        let start = std::time::Instant::now();
        let resp = client
            .get("https://www.gstatic.com/generate_204")
            .send()
            .await;
        let dto = match resp {
            Ok(r) => EgressProbeDto {
                ok: r.status().is_success(),
                ms: Some(start.elapsed().as_millis() as u64),
                status: Some(r.status().as_u16()),
            },
            Err(_) => EgressProbeDto {
                ok: false,
                ms: None,
                status: None,
            },
        };
        Ok(dto)
    }

    /// 消费臂纯函数（restart_with_config 与单测共用）：手动选定 → Selector.default_tag。
    /// 选点在本代节点集（已过能力过滤）在场=换人；缺件（订阅更新删点/过滤剔除）=
    /// 回落首节点旧行为，失效面由 [`StatusDto::selected_stale`] 如实上报。
    fn apply_selected(
        cfg: &mut crate::ir::IrConfig,
        nodes: &[Node],
        selected: Option<&(String, String)>,
    ) {
        let Some(sel) = selected else { return };
        let Some(default) = nodes
            .iter()
            .find(|n| n.sub_id == sel.0 && n.tag == sel.1)
            .or_else(|| nodes.first())
            .map(Node::outbound_tag)
        else {
            return; // nodes 空 = ir::build 已拒，此臂不可达仅防
        };
        for ob in &mut cfg.outbounds {
            if let crate::ir::IrOutbound::Selector {
                tag, default_tag, ..
            } = ob
            {
                if tag == crate::ir::TAG_PROXY {
                    *default_tag = Some(default);
                    return;
                }
            }
        }
    }

    /// 驱动解析：测试注入表优先（生产路径恒空），其后走注册表
    fn driver(&self, id: &str) -> Result<Arc<dyn KernelDriver>> {
        #[cfg(test)]
        {
            if let Some((_, d)) = self.test_drivers.read().iter().find(|(k, _)| k == id) {
                return Ok(d.clone());
            }
        }
        driver_for(&self.bin_dir(), id)
    }

    /// 用当前节点重新生成配置并（重）启选定内核；起后健康探活 3s
    fn restart_with_config(self: &Arc<Self>, tun: bool) -> Result<()> {
        let (nodes, rules_v2, port, kernel, selected_node) = {
            let inner = self.inner.read();
            (
                inner.nodes.clone(),
                inner.rules_v2.clone(),
                inner.mixed_port,
                inner.kernel.clone(),
                inner.selected_node.clone(),
            )
        };
        // 停旧内核（模式切换 / 换核）
        {
            let mut inner = self.inner.write();
            if let Some(h) = inner.handle.take() {
                h.stop();
            }
        }
        let driver = self.driver(&kernel)?;
        // T-B2-7 能力过滤（09 §5.1-⑭ 能力表单一真源）：supported_kinds 之外的
        // 节点不进本内核渲染（如 SSR 全内核不支持）；节点数据保留，仅本代配置剔除。
        let supported = driver.supported_kinds();
        let nodes: Vec<_> = nodes
            .into_iter()
            .filter(|n| supported.contains(&n.kind))
            .collect();
        let excluded = {
            let inner = self.inner.read();
            inner.nodes.len() - nodes.len()
        };
        if excluded > 0 {
            tracing::info!(
                excluded,
                kernel = kernel.as_str(),
                "部分节点协议不被当前内核支持，已从本代配置剔除"
            );
        }
        if nodes.is_empty() && excluded > 0 {
            return Err(ProxyError::Config(format!(
                "当前内核（{kernel}）不支持任何已添加节点协议：请换内核或更新订阅（{excluded} 个节点被剔除）"
            )));
        }
        // v2 分流表 → IR 规则组 + 兜底 tag（route_mode=global/direct_all 时规则全跳过；
        // block 出站由 ir::build 仅在被引用时注入——方言渲染零条件跟随）
        let (user_rules, final_target) = rules::to_ir(&rules_v2)?;
        // T-B2-10 渲染预检：geo 类规则在场且本内核消费本地资产缺失 → 写盘前诚实拒
        //（内核读缺文件 = 启动失败堆在日志页，不如此处点名资产+指路安装入口）
        check_geo_prereqs(driver.as_ref(), &self.proxy_dir, &user_rules)?;
        let mut ir_cfg = ir::build(port, tun, &nodes, &user_rules, &final_target, &[])?;
        // T-B2-11 消费臂：手动选定 → Selector.default_tag（缺件回落首节点，
        // 失效面经 status().selected_stale 如实上报；None=自动 urltest 原样）
        Self::apply_selected(&mut ir_cfg, &nodes, selected_node.as_ref());

        let rendered = driver.config_render(&ir_cfg)?;
        let cfg_path = self.proxy_dir.join(driver.cfg_name());
        // mihomo 方言 config.yaml 住 proxy/mihomo/ 子目录（09 ④ -d 语义）：
        // 通用建父目录，两臂方言（config.json/config-xray.json 在根）下为无害 no-op
        if let Some(parent) = cfg_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        host_core::util::write_atomic(&cfg_path, rendered.as_bytes())?;

        let weak = Arc::downgrade(self);
        let on_exit: Arc<dyn Fn(i32) + Send + Sync> = Arc::new(move |code| {
            // 内核意外退出：立即还原系统代理 + 模式归零（断网最高危场景兜底）
            tracing::error!(code, "代理内核意外退出");
            if let Some(svc) = weak.upgrade() {
                let _ = svc.stop_kernel_and_restore();
            }
        });

        let handle = driver.start(&self.proxy_dir, &cfg_path, on_exit)?;
        handle.set_log_cb({
            let bus = self.bus.clone();
            Arc::new(move |line: &str| {
                bus.publish(Event::new(
                    "proxy.log_line",
                    "proxy",
                    serde_json::json!({ "line": line }),
                ))
                .ok();
            })
        });

        // 健康探活：mixed 模式等端口可连（端口由驱动 probe_port 决议：xray 双入站
        // 时即 http 端口，恒等于 mixed_port）；TUN 无本地端口 → 等进程存活即认为就绪
        let healthy = if tun {
            std::thread::sleep(Duration::from_millis(300));
            handle.alive()
        } else {
            let probe = driver.probe_port(&ir_cfg);
            let deadline = std::time::Instant::now() + Duration::from_millis(HEALTH_WAIT_MS);
            loop {
                if handle.health_check(probe) {
                    break true;
                }
                if !handle.alive() {
                    break false;
                }
                if std::time::Instant::now() > deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        if !healthy {
            handle.stop();
            return Err(ProxyError::Kernel(
                "内核启动后未就绪（配置错误？详见日志页）".into(),
            ));
        }
        self.inner.write().handle = Some(handle);
        self.publish_state();
        Ok(())
    }

    /// 应用退出 / 模块 stop：停内核 + 还原系统代理（幂等，静默）
    pub fn shutdown(&self) {
        let _ = self.stop_kernel_and_restore();
    }

    // ---------------- 延迟测试（PR6） ----------------

    /// TCP 连接延迟（v1 轻量方案；每节点并发、3s 超时）。
    /// 缺陷⑨ 修：结果向量恒等于节点数——(tag, sub_id) 在 spawn 前登记，
    /// join Err（任务 panic/abort）臂补 ms:None 占位，节点行不会凭空丢列。
    pub async fn delay_test(&self) -> Vec<NodeDelayDto> {
        let nodes = self.inner.read().nodes.clone();
        let mut tasks = Vec::with_capacity(nodes.len());
        for n in nodes {
            #[cfg(test)]
            let force_panic = self.delay_panic_tags.read().contains(&n.tag);
            tasks.push((
                (n.tag.clone(), n.sub_id.clone()),
                tokio::spawn(async move {
                    #[cfg(test)]
                    if force_panic {
                        panic!("测试注入：delay 任务 abort（缺陷⑨ 红线入口）");
                    }
                    let addr = std::net::SocketAddr::new(
                        resolve_host(&n.server)
                            .await
                            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
                        n.port,
                    );
                    let start = std::time::Instant::now();
                    match tokio::time::timeout(
                        Duration::from_secs(3),
                        tokio::net::TcpStream::connect(addr),
                    )
                    .await
                    {
                        Ok(Ok(_)) => Some(start.elapsed().as_millis() as u64),
                        _ => None,
                    }
                }),
            ));
        }
        let mut out = Vec::with_capacity(tasks.len());
        for ((tag, sub_id), t) in tasks {
            // JoinError（panic/cancel）→ None 占位仍入列：长度恒等的定长向量红线
            let ms = t.await.unwrap_or(None);
            out.push(NodeDelayDto { tag, sub_id, ms });
        }
        out
    }

    pub fn logs(&self, limit: usize) -> Vec<LogLine> {
        let inner = self.inner.read();
        inner
            .handle
            .as_ref()
            .map(|h| h.logs_snapshot(limit))
            .unwrap_or_default()
    }

    // ---------------- 内部 ----------------

    fn bin_dir(&self) -> PathBuf {
        self.proxy_dir.join(BIN_DIR)
    }

    fn sub_nodes_path(&self, id: &str) -> PathBuf {
        self.proxy_dir
            .join(SUBS_DIR)
            .join(format!("{id}.nodes.json"))
    }

    fn save_subs(&self, inner: &Inner) -> Result<()> {
        host_core::util::write_atomic(
            &self.proxy_dir.join(SUBS_FILE),
            &serde_json::to_vec(&inner.subs)?,
        )?;
        Ok(())
    }

    fn publish_state(&self) {
        let s = self.status();
        self.bus
            .publish(Event::new(
                "proxy.state_changed",
                "proxy",
                serde_json::json!({
                    "mode": s.mode,
                    "kernel_running": s.kernel_running,
                    "kernel_id": s.kernel_id,
                    "inbound_port": s.inbound_port,
                    "kernel": s.kernel,
                }),
            ))
            .ok();
    }

    /// 节点集变更事件（缺陷⑩根治面在 T-B2-7）：`sub_id` 由调用方给出真实来源，
    /// None 表示跨订阅/启动期的整体重载（前端仍按 total 兜底刷新）。
    fn publish_nodes(&self, sub_id: Option<&str>) {
        let inner = self.inner.read();
        self.bus
            .publish(Event::new(
                "proxy.nodes_changed",
                "proxy",
                serde_json::json!({ "sub_id": sub_id, "total": inner.nodes.len() }),
            ))
            .ok();
    }
}

/// 加载 subs.json 元数据 + 各订阅节点文件
fn load_subs(proxy_dir: &Path) -> Result<(Vec<Sub>, Vec<Node>)> {
    let mut subs: Vec<Sub> = std::fs::read(proxy_dir.join(SUBS_FILE))
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default();
    let mut nodes = Vec::new();
    for sub in &mut subs {
        if let Ok(list) = std::fs::read(
            proxy_dir
                .join(SUBS_DIR)
                .join(format!("{}.nodes.json", sub.id)),
        )
        .and_then(|raw| {
            serde_json::from_slice::<Vec<Node>>(&raw)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        }) {
            sub.node_count = list.len();
            nodes.extend(list);
        }
    }
    Ok((subs, nodes))
}

async fn resolve_host(host: &str) -> Option<std::net::IpAddr> {
    use std::net::ToSocketAddrs;
    // server:port → 第一个地址的 IP（v1 用同步解析；DNS 失败返回 None → 连接必失败 → None 延迟）
    (host, 1u16).to_socket_addrs().ok()?.next().map(|a| a.ip())
}

use host_core::util::now_ms_u64 as now_ms;

#[cfg(test)]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
mod tests {
    use super::*;
    use crate::ir::IrConfig;
    use crate::kernel::spawn_kernel;
    use crate::sub::NodeKind;
    use host_core::ports::SysProxyState;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    #[derive(Clone)]
    struct MockSp {
        state: StdArc<Mutex<SysProxyState>>,
        /// T-B2-2：enter_tun 负例需可控管理员位（默认 false = 旧行为，存量 5 测零改动）
        admin: StdArc<AtomicBool>,
    }

    impl MockSp {
        fn new() -> Self {
            Self {
                state: StdArc::new(Mutex::new(SysProxyState::default())),
                admin: StdArc::new(AtomicBool::new(false)),
            }
        }
    }

    impl SysProxyPort for MockSp {
        fn read(&self) -> std::result::Result<SysProxyState, host_core::error::AppError> {
            Ok(self.state.lock().clone())
        }
        fn write(
            &self,
            state: &SysProxyState,
        ) -> std::result::Result<(), host_core::error::AppError> {
            *self.state.lock() = state.clone();
            Ok(())
        }
        fn refresh(&self) -> std::result::Result<(), host_core::error::AppError> {
            Ok(())
        }
        fn is_admin(&self) -> bool {
            self.admin.load(Ordering::SeqCst)
        }
    }

    /// 测试内核驱动：真实子进程（cmd ping 长驻）走 spawn_kernel 全生命周期，
    /// 但 exe 检查被覆写 start 绕过；succeeds=false 模拟"起核必败"（换核回滚红线）。
    struct TestKernelDriver {
        id: &'static str,
        cfg: &'static str,
        succeeds: bool,
        start_attempts: AtomicUsize,
        /// 置位后下一次 start 强制失败（缺陷⑧复现：运行中再入失败）
        fail_next: AtomicBool,
        /// T-B2-5：能力位 tun=false 的建模（xray 主线门禁红线）
        tun_cap: bool,
        /// T-B2-10：geo 预检门的建模位（非空 = 该"内核"消费本地 geo 资产）
        geo_assets: &'static [&'static str],
    }

    impl TestKernelDriver {
        fn new(id: &'static str, cfg: &'static str) -> StdArc<Self> {
            Self::with(id, cfg, true, true)
        }
        /// 起核必败驱动（换核回滚红线专用）
        fn failing(id: &'static str, cfg: &'static str) -> StdArc<Self> {
            Self::with(id, cfg, false, true)
        }
        /// 无 TUN 能力驱动（xray 门禁红线专用，真实形态=全关能力表）
        fn no_tun(id: &'static str, cfg: &'static str) -> StdArc<Self> {
            Self::with(id, cfg, true, false)
        }
        /// geo 资产预检门驱动（T-B2-10 红线专用：对任意 geo 类点名同一资产集）
        fn geo_gated(
            id: &'static str,
            cfg: &'static str,
            assets: &'static [&'static str],
        ) -> StdArc<Self> {
            StdArc::new(Self {
                id,
                cfg,
                succeeds: true,
                start_attempts: AtomicUsize::new(0),
                fail_next: AtomicBool::new(false),
                tun_cap: true,
                geo_assets: assets,
            })
        }
        fn with(
            id: &'static str,
            cfg: &'static str,
            succeeds: bool,
            tun_cap: bool,
        ) -> StdArc<Self> {
            StdArc::new(Self {
                id,
                cfg,
                succeeds,
                start_attempts: AtomicUsize::new(0),
                fail_next: AtomicBool::new(false),
                tun_cap,
                geo_assets: &[],
            })
        }
    }

    impl KernelDriver for TestKernelDriver {
        fn id(&self) -> &'static str {
            self.id
        }
        fn exe_path(&self) -> PathBuf {
            // 覆写了 start，此路径从不被探测；给个非空值防误用
            PathBuf::from("test-kernel.exe")
        }
        fn display_name(&self) -> &'static str {
            "测试内核"
        }
        fn cfg_name(&self) -> &'static str {
            self.cfg
        }
        fn supported_kinds(&self) -> &'static [NodeKind] {
            &[NodeKind::Shadowsocks]
        }
        fn caps(&self) -> KernelCaps {
            KernelCaps {
                tun: self.tun_cap,
                policy_groups: false,
                external_controller: false,
            }
        }
        fn config_render(&self, _ir: &IrConfig) -> Result<String> {
            Ok("{}\n".to_string())
        }
        fn geo_asset_ids(&self, _cat: &str) -> &'static [&'static str] {
            self.geo_assets
        }
        fn build_command(&self, _work_dir: &Path, _cfg: &Path) -> std::process::Command {
            let mut c = std::process::Command::new(
                std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()),
            );
            c.arg("/C")
                .arg("ping")
                .arg("-n")
                .arg("600")
                .arg("127.0.0.1");
            c
        }
        fn start(
            &self,
            work_dir: &Path,
            cfg: &Path,
            on_exit: crate::kernel::ExitCb,
        ) -> Result<KernelHandle> {
            self.start_attempts.fetch_add(1, Ordering::SeqCst);
            if !self.succeeds || self.fail_next.swap(false, Ordering::SeqCst) {
                return Err(ProxyError::Kernel(format!("测试内核 {} 启动失败", self.id)));
            }
            spawn_kernel(self.id, self.build_command(work_dir, cfg), on_exit)
        }
    }

    fn open_service(tag: &str) -> (Arc<ProxyService>, StdArc<MockSp>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nf_proxy_svc_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sp = StdArc::new(MockSp::new());
        let svc = ProxyService::open(
            &dir,
            Arc::new(host_core::events::EventBus::new()),
            sp.clone(),
        )
        .unwrap();
        (svc, sp, dir)
    }

    #[test]
    fn open_restores_residual_proxy() {
        let (svc, sp, dir) = open_service("residual");
        // 直接制造 kill -9 残留现场：备份 + 当前值 = 我们的标记
        sysproxy::enable(&dir.join("proxy"), sp.as_ref(), 7890).unwrap();
        assert!(sp.read().unwrap().enable);
        // 重开（模拟重启）
        let svc2 = ProxyService::open(&dir, svc.bus.clone(), sp.clone()).unwrap();
        assert!(!sp.read().unwrap().enable, "启动扫描应还原残留系统代理");
        assert!(svc2.status().restored_last_run);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sub_crud_persists_across_reopen() {
        let (svc, _sp, dir) = open_service("crud");
        let sub = svc.sub_add("测试订阅", "https://example.com/sub").unwrap();
        assert_eq!(svc.subs().len(), 1);
        drop(svc);
        let sp = StdArc::new(MockSp::new());
        let svc2 =
            ProxyService::open(&dir, Arc::new(host_core::events::EventBus::new()), sp).unwrap();
        assert_eq!(svc2.subs().len(), 1, "订阅应持久化");
        assert_eq!(svc2.subs()[0].name, "测试订阅");
        assert!(svc2.sub_remove(&sub.id).unwrap());
        assert_eq!(svc2.subs().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mode_rejects_unknown_and_tun_without_admin() {
        let (svc, _sp, dir) = open_service("tun");
        assert!(matches!(Mode::parse("bogus"), Err(ProxyError::BadState(_))));
        // 无管理员（MockSp is_admin=false）→ TUN 拒绝
        let arc_svc: Arc<ProxyService> = svc;
        assert!(matches!(
            arc_svc.set_mode(Mode::Tun),
            Err(ProxyError::Permission(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn direct_rules_roundtrip() {
        let (svc, _sp, dir) = open_service("rules");
        svc.set_direct_rules(vec![
            ".corp.example.com".into(),
            " internal.local ".into(),
            String::new(),
        ])
        .unwrap();
        assert_eq!(
            svc.direct_rules(),
            vec![".corp.example.com", "internal.local"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- T-B2-9：分流规则 v2（09 §5.2 字面回归） ----------------

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    fn rulesV2_legacyFile_derivesSuffixDirect() {
        let dir = std::env::temp_dir().join(format!(
            "nf_proxy_svc_rulesv2_legacy_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let proxy_dir = dir.join("proxy");
        std::fs::create_dir_all(&proxy_dir).unwrap();
        std::fs::write(
            proxy_dir.join(RULES_FILE),
            serde_json::to_vec(&vec!["bilibili.com".to_string(), "cn".to_string()]).unwrap(),
        )
        .unwrap();
        let svc = ProxyService::open(
            &dir,
            Arc::new(host_core::events::EventBus::new()),
            StdArc::new(MockSp::new()),
        )
        .unwrap();
        let v2 = svc.rules_v2();
        assert_eq!(v2.final_target, "proxy");
        assert_eq!(v2.route_mode, "rule");
        assert_eq!(v2.rules.len(), 2);
        assert!(v2
            .rules
            .iter()
            .all(|r| r.kind == "suffix" && r.target == "direct" && r.enabled));
        assert_eq!(
            v2.rules
                .iter()
                .map(|r| r.pattern.as_str())
                .collect::<Vec<_>>(),
            ["bilibili.com", "cn"]
        );
        // 零迁移承诺：open 只派生不物化，首次保存才写 rules_v2.json
        assert!(!proxy_dir.join(RULES_V2_FILE).exists());
        assert_eq!(svc.direct_rules(), vec!["bilibili.com", "cn"]);
        svc.set_rules_v2(v2.clone()).unwrap();
        assert!(proxy_dir.join(RULES_V2_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    fn rulesV2_badCidr_rejected() {
        // 红线：非法 CIDR（段越界 999 与注入串 a;b）均拒且点名违规值；
        // 拒后内存/盘上状态原样（单条坏输入不得打翻既有规则表）
        let (svc, _sp, dir) = open_service("rulesv2_badcidr");
        svc.set_direct_rules(vec!["cn".into()]).unwrap();
        for bad in ["999.1.1.1/8", "a;b"] {
            let attempt = RulesV2 {
                rules: vec![RuleV2 {
                    kind: "ip_cidr".into(),
                    pattern: bad.into(),
                    target: "direct".into(),
                    enabled: true,
                }],
                final_target: "proxy".into(),
                route_mode: "rule".into(),
            };
            match svc.set_rules_v2(attempt) {
                Err(ProxyError::Config(msg)) => {
                    assert!(msg.contains(bad), "错误须点名违规值 {bad}：{msg}");
                }
                other => panic!("{bad} 必须被 Config 拒绝，得 {other:?}"),
            }
        }
        assert_eq!(svc.direct_rules(), vec!["cn"], "拒写不得扰动现表");
        // 正对照：合法 v6/v4 前缀放行
        assert!(svc
            .set_rules_v2(RulesV2 {
                rules: vec![
                    RuleV2 {
                        kind: "ip_cidr".into(),
                        pattern: "2001:db8::/32".into(),
                        target: "direct".into(),
                        enabled: true,
                    },
                    RuleV2 {
                        kind: "ip_cidr".into(),
                        pattern: "10.0.0.0/8".into(),
                        target: "block".into(),
                        enabled: false,
                    },
                ],
                final_target: "proxy".into(),
                route_mode: "rule".into(),
            })
            .is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    fn setDirectRules_nonAdjacentDup_removed() {
        // 缺陷随行修：旧 `Vec::dedup` 只删相邻重复，"a b a" 会留两个 a
        let (svc, _sp, dir) = open_service("directdup");
        svc.set_direct_rules(vec![
            "a.com".into(),
            "b.com".into(),
            "a.com".into(),
            "  ".into(),
            " b.com ".into(),
        ])
        .unwrap();
        assert_eq!(svc.direct_rules(), vec!["a.com", "b.com"]);
        let raw: Vec<String> =
            serde_json::from_slice(&std::fs::read(dir.join("proxy").join(RULES_FILE)).unwrap())
                .unwrap();
        assert_eq!(raw, vec!["a.com", "b.com"], "盘上须与内存同清单");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn b64_helper_still_works_for_sub_content() {
        use base64::Engine;
        // http_get 返回的字节可能整体 base64 —— 引擎能力自检
        let s = base64::engine::general_purpose::STANDARD.encode("hello");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&s)
                .unwrap(),
            b"hello"
        );
    }

    // ---------------- T-B2-2：内核注册表 / 持久化 / 换核生命周期 ----------------

    fn inject(svc: &Arc<ProxyService>, d: StdArc<TestKernelDriver>) {
        svc.test_drivers.write().push((d.id.to_string(), d));
    }

    /// ir::build 空节点即拒（"无可用节点"），生命周期测必须至少一枚节点
    fn add_demo_node(svc: &Arc<ProxyService>) {
        svc.inner.write().nodes.push(Node {
            tag: "n1".into(),
            kind: NodeKind::Shadowsocks,
            server: "127.0.0.1".into(),
            port: 9,
            sub_id: "sub-test".into(),
            groups: Vec::new(),
            extra: serde_json::Value::Null,
        });
    }

    /// 健康探活桩：TCP listener 占住 ephemeral mixed_port——
    /// handle.health_check(port) 对 backlog 中的连接即成功，测试驱动真实通过探活。
    fn stub_health_port(svc: &Arc<ProxyService>) -> (std::net::TcpListener, u16) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        svc.set_mixed_port(port).unwrap();
        (listener, port)
    }

    #[test]
    fn stateLegacy_missingKernel_defaultsSingbox() {
        let (svc, _sp, dir) = open_service("legacy");
        drop(svc);
        // 旧文件形态：仅 mixed_port 无 kernel 键 → serde default 零迁移
        std::fs::write(
            dir.join("proxy").join(STATE_FILE),
            br#"{"mixed_port":8899}"#,
        )
        .unwrap();
        let svc2 = ProxyService::open(
            &dir,
            Arc::new(host_core::events::EventBus::new()),
            StdArc::new(MockSp::new()),
        )
        .unwrap();
        let s = svc2.status();
        assert_eq!(
            s.kernel, KERNEL_SINGBOX,
            "缺 kernel 键的旧状态必须落到 sing-box"
        );
        assert_eq!(s.inbound_port, 8899);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kernelSelect_persistsAcrossReopen() {
        let (svc, _sp, dir) = open_service("kselect");
        let a = TestKernelDriver::new("test-core-a", "config-a.json");
        inject(&svc, a.clone());
        // 红线预检：未知核在任何状态变更前被拒（持久化文件不落坏选择）
        assert!(matches!(
            svc.set_kernel("bogus-core"),
            Err(ProxyError::Kernel(_))
        ));
        assert_eq!(svc.status().kernel, KERNEL_SINGBOX);
        assert_eq!(
            a.start_attempts.load(Ordering::SeqCst),
            0,
            "Off 态选择只写不启动"
        );
        svc.set_mixed_port(8899).unwrap();
        svc.set_kernel("test-core-a").unwrap();
        svc.set_kernel("test-core-a").unwrap(); // 同核 = no-op
        assert_eq!(svc.status().kernel, "test-core-a");
        drop(svc);
        let svc2 = ProxyService::open(
            &dir,
            Arc::new(host_core::events::EventBus::new()),
            StdArc::new(MockSp::new()),
        )
        .unwrap();
        let s = svc2.status();
        assert_eq!(s.kernel, "test-core-a", "选定内核必须跨重启持久");
        assert_eq!(s.inbound_port, 8899, "双字段整包写不得互踩 mixed_port");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kernelSwap_running_restartsNewDriverAndKeepsSysproxy() {
        let (svc, sp, dir) = open_service("swap");
        let a = TestKernelDriver::new("test-core-a", "config-a.json");
        let b = TestKernelDriver::new("test-core-b", "config-b.json");
        inject(&svc, a.clone());
        inject(&svc, b.clone());
        add_demo_node(&svc);
        let (listener, port) = stub_health_port(&svc);
        svc.set_kernel("test-core-a").unwrap();
        svc.set_mode(Mode::System).unwrap();
        assert_eq!(a.start_attempts.load(Ordering::SeqCst), 1);
        {
            let st = sp.read().unwrap();
            assert!(st.enable && st.server.contains(&port.to_string()));
        }
        // 运行中换核：新核起、旧核停、System 模式与系统代理全程不动
        svc.set_kernel("test-core-b").unwrap();
        assert_eq!(b.start_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(
            a.start_attempts.load(Ordering::SeqCst),
            1,
            "换核不得重起旧核"
        );
        let s = svc.status();
        assert_eq!(s.kernel, "test-core-b");
        assert_eq!(s.kernel_id.as_deref(), Some("test-core-b"));
        assert!(s.kernel_running);
        assert_eq!(s.mode, "system");
        {
            let st = sp.read().unwrap();
            assert!(
                st.enable && st.server.contains(&port.to_string()),
                "换核窗口系统代理必须仍指向同一 mixed 端口（内核托管不断线）"
            );
        }
        let raw = std::fs::read_to_string(dir.join("proxy").join(STATE_FILE)).unwrap();
        assert!(raw.contains("test-core-b"));
        assert!(raw.contains(&format!("\"mixed_port\":{port}")));
        svc.set_mode(Mode::Off).unwrap();
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kernelSwap_newKernelFails_rollsBackOld() {
        let (svc, sp, dir) = open_service("rollback");
        let a = TestKernelDriver::new("test-core-a", "config-a.json");
        let b = TestKernelDriver::failing("test-core-b", "config-b.json");
        inject(&svc, a.clone());
        inject(&svc, b.clone());
        add_demo_node(&svc);
        let (listener, port) = stub_health_port(&svc);
        svc.set_kernel("test-core-a").unwrap();
        svc.set_mode(Mode::System).unwrap();
        assert_eq!(a.start_attempts.load(Ordering::SeqCst), 1);
        // 红线：新核起不来 → 回滚旧核恢复服务，选择/磁盘都停在旧核，绝不静默降级
        let e = svc.set_kernel("test-core-b").unwrap_err();
        assert!(
            matches!(e, ProxyError::Kernel(_)),
            "换核失败必须是 Kernel 错"
        );
        let s = svc.status();
        assert_eq!(s.kernel, "test-core-a", "失败后选定内核回滚");
        assert_eq!(s.kernel_id.as_deref(), Some("test-core-a"));
        assert!(s.kernel_running, "回滚后旧核句柄必须真实存活");
        assert_eq!(s.mode, "system");
        assert_eq!(b.start_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(
            a.start_attempts.load(Ordering::SeqCst),
            2,
            "回滚 = 旧核重起一次"
        );
        {
            let st = sp.read().unwrap();
            assert!(st.enable && st.server.contains(&port.to_string()));
        }
        let raw = std::fs::read_to_string(dir.join("proxy").join(STATE_FILE)).unwrap();
        assert!(!raw.contains("test-core-b"), "失败换核不得污染持久化文件");
        svc.set_mode(Mode::Off).unwrap();
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn setMode_sameMode_isNoop() {
        let (svc, _sp, dir) = open_service("noop");
        let a = TestKernelDriver::new("test-core-a", "config-a.json");
        inject(&svc, a.clone());
        add_demo_node(&svc);
        let (listener, _port) = stub_health_port(&svc);
        svc.set_kernel("test-core-a").unwrap();
        svc.set_mode(Mode::System).unwrap();
        assert_eq!(a.start_attempts.load(Ordering::SeqCst), 1);
        // 抽掉探活桩：同模式再入若真的重启内核，3s 探活必败 → Err；no-op 则 Ok
        drop(listener);
        svc.set_mode(Mode::System).unwrap();
        assert_eq!(svc.status().mode, "system");
        assert_eq!(
            a.start_attempts.load(Ordering::SeqCst),
            1,
            "同模式重入不得停核重启（无谓断网窗口）"
        );
        svc.set_mode(Mode::Off).unwrap();
        svc.set_mode(Mode::Off).unwrap();
        assert_eq!(svc.status().mode, "off");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enterTun_startFails_stateHonestOff() {
        // 缺陷⑧复现：System 运行中 → Tun 起核失败 → 状态如实 Off，
        // 绝不残留 "system" 文案 + 已还原代理 + 已停内核的三重不符
        let (svc, sp, dir) = open_service("tunfail");
        sp.admin.store(true, Ordering::SeqCst);
        std::fs::write(dir.join("proxy").join("bin").join("wintun.dll"), b"mock").unwrap();
        let a = TestKernelDriver::new("test-core-a", "config-a.json");
        inject(&svc, a.clone());
        add_demo_node(&svc);
        let (listener, _port) = stub_health_port(&svc);
        svc.set_kernel("test-core-a").unwrap();
        svc.set_mode(Mode::System).unwrap();
        a.fail_next.store(true, Ordering::SeqCst);
        let r = svc.set_mode(Mode::Tun);
        assert!(matches!(r, Err(ProxyError::Kernel(_))), "起核失败必须上抛");
        let s = svc.status();
        assert_eq!(s.mode, "off", "失败后模式必须归零而非停留 system");
        assert!(!s.kernel_running);
        assert!(!sp.read().unwrap().enable, "系统代理已如实还原");
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- T-B2-3/T-B2-4：安装扩参 / 内核参数透传 ----------------

    #[test]
    fn installKernel_command_kernelParamPassthrough() {
        // 命令层 kernel 参数（Option<&str>）→ 资产表解析：缺省 = sing-box（旧调用
        // 兼容零迁移），xray/mihomo 各命中自身规格（T-B2-3 时代"仅 sing-box 可装"
        // 的 kernelInstallGate_nonSingbox_honestReject 由本行 KERNEL_ASSETS 接管），
        // 未知 id 在触网前 Kernel 拒并指路支持面。
        assert_eq!(resolve_install_kernel(None).unwrap().id, KERNEL_SINGBOX);
        assert_eq!(resolve_install_kernel(Some("xray")).unwrap().id, "xray");
        assert_eq!(resolve_install_kernel(Some("mihomo")).unwrap().id, "mihomo");
        assert_eq!(
            (resolve_install_kernel(Some("xray")).unwrap().url_for)("26.3.27"),
            sidecar::xray_download_url("26.3.27"),
            "fn 指针必须接各自官方直链，不得串道"
        );
        let e = resolve_install_kernel(Some("bogus")).unwrap_err();
        match e {
            ProxyError::Kernel(msg) => {
                assert!(msg.contains("bogus"), "错误必须点名被拒内核: {msg}");
                assert!(msg.contains("下载通道"), "错误必须指路未接入原因: {msg}");
            }
            other => panic!("未知内核必须是 Kernel 错，实得 {other:?}"),
        }
    }

    #[test]
    fn restartKernel_offHonestErr() {
        // 红线负例：未运行内核"重启"必须 BadState 如实拒（不是静默成功也不是启动）
        let (svc, _sp, dir) = open_service("restart_off");
        let e = svc.restart_kernel().unwrap_err();
        assert!(
            matches!(e, ProxyError::BadState(_)),
            "关闭态重启必须是 BadState 错"
        );
        assert!(e.to_string().contains("无需重启"));
        let s = svc.status();
        assert!(!s.kernel_running && s.mode == "off", "被拒后状态零漂移");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restartKernel_running_keepsModeAndRestartsSameKernel() {
        // 重启 = 停旧进程 → 当前配置重起：mode/kernel 双双不变、系统代理不复挂
        let (svc, sp, dir) = open_service("restart_run");
        let a = TestKernelDriver::new("test-core-a", "config-a.json");
        inject(&svc, a.clone());
        add_demo_node(&svc);
        let (listener, port) = stub_health_port(&svc);
        svc.set_kernel("test-core-a").unwrap();
        svc.set_mode(Mode::System).unwrap();
        assert_eq!(a.start_attempts.load(Ordering::SeqCst), 1);
        svc.restart_kernel().unwrap();
        assert_eq!(
            a.start_attempts.load(Ordering::SeqCst),
            2,
            "重启必须真实重起一次同内核进程"
        );
        let s = svc.status();
        assert_eq!(s.mode, "system", "重启不改模式");
        assert_eq!(s.kernel, "test-core-a");
        assert_eq!(s.kernel_id.as_deref(), Some("test-core-a"));
        assert!(s.kernel_running);
        {
            let st = sp.read().unwrap();
            assert!(
                st.enable && st.server.contains(&port.to_string()),
                "System 态重启全程不动系统代理（端口复用即恢复服务）"
            );
        }
        // 失败纪律（缺陷⑧同款）：起新败 → 归零关闭态 + Kernel 错文案指路
        a.fail_next.store(true, Ordering::SeqCst);
        let e = svc.restart_kernel().unwrap_err();
        assert!(matches!(e, ProxyError::Kernel(_)));
        assert!(e.to_string().contains("已停止为关闭态"));
        let s = svc.status();
        assert_eq!(s.mode, "off", "重启失败不得残留运行假象");
        assert!(!s.kernel_running);
        assert!(!sp.read().unwrap().enable, "失败后系统代理已如实还原");
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- T-B2-5：xray 能力门禁红线 ----------------

    #[test]
    fn xrayTun_rejectedWithHonestError() {
        // 红线：caps.tun=false 内核请求 TUN，必须在权限/组件检查**之前**如实拒——
        // 管理员与 wintun 全部就位仍须拒，且文案给出真正解法"换核"
        let (svc, sp, dir) = open_service("xray_tun");
        sp.admin.store(true, Ordering::SeqCst);
        std::fs::write(dir.join("proxy").join("bin").join("wintun.dll"), b"mock").unwrap();
        let x = TestKernelDriver::no_tun("xray", "config-xray.json");
        inject(&svc, x.clone());
        add_demo_node(&svc);
        svc.set_kernel("xray").unwrap(); // Off 态换核 = 纯持久化不起核
        let e = svc.set_mode(Mode::Tun).unwrap_err();
        match e {
            ProxyError::BadState(msg) => {
                assert!(
                    msg.contains("不支持 TUN") && msg.contains("换 sing-box/mihomo"),
                    "{msg}"
                )
            }
            other => panic!("TUN 能力拒必须是 BadState（非 Permission、非起核），实得 {other:?}"),
        }
        assert_eq!(
            x.start_attempts.load(Ordering::SeqCst),
            0,
            "被拒内核必须零起核（拒在一切副作用之前）"
        );
        let s = svc.status();
        assert_eq!(s.mode, "off", "拒后状态如实 Off");
        assert_eq!(s.kernel, "xray", "拒绝不回滚内核选择");
        assert!(
            !dir.join("proxy").join("config-xray.json").exists(),
            "被拒配置不得落盘"
        );
        assert!(!sp.read().unwrap().enable, "系统代理全程未动");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- T-B2-7：节点事件携带真实 sub_id（缺陷⑩面收口） ----------------

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    fn publishNodes_eventCarriesRealSubId() {
        let (svc, _sp, dir) = open_service("nodesubid");
        // 总线订阅必须先于发布就位（broadcast 不回放历史消息）
        let mut rx = svc.bus.subscribe("proxy.nodes_changed").unwrap();
        let sub = svc
            .sub_add("链路A", "http://127.0.0.1:9/never-fetched")
            .unwrap();
        // 移除订阅是唯一免网络的真实发布路径（sub_update 需拉取）
        svc.sub_remove(&sub.id).unwrap();
        let ev = rx
            .try_recv()
            .expect("sub_remove 必须发出 proxy.nodes_changed 事件");
        assert_eq!(
            ev.payload["sub_id"], sub.id,
            "缺陷⑩红线：事件 sub_id 不得再恒 null"
        );
        assert_eq!(ev.payload["total"], 0);
        // 兼容形状：None（启动期整体重载）仍落 JSON null，前端旧消费面不破
        svc.publish_nodes(None);
        let ev2 = rx.try_recv().expect("publish_nodes(None) 也须发事件");
        assert!(ev2.payload["sub_id"].is_null(), "{:?}", ev2.payload);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- T-B2-10：订阅 304 落账 + geo 渲染预检（09 §5.2 字面回归） ----------------

    #[test]
    fn subEtagNotModified_keepsNodesRefreshStamp() {
        // 红线：If-None-Match 命中 304 = 只刷"上次成功更新"时刻——节点文件/流量头/
        // etag 一字不动，且不发 nodes_changed（节点集未变，UI 无需失效重取）
        let (svc, _sp, dir) = open_service("etag304");
        let sub = svc.sub_add("带头订阅", "http://127.0.0.1:9/never").unwrap();
        let nodes_path = svc.sub_nodes_path(&sub.id);
        std::fs::write(
            &nodes_path,
            serde_json::to_vec(&vec![Node {
                tag: "keep".into(),
                kind: NodeKind::Shadowsocks,
                server: "1.2.3.4".into(),
                port: 8388,
                sub_id: sub.id.clone(),
                groups: Vec::new(),
                extra: serde_json::json!({"method": "aes-256-gcm", "password": "p"}),
            }])
            .unwrap(),
        )
        .unwrap();
        {
            let mut inner = svc.inner.write();
            let i = inner.subs.iter().position(|s| s.id == sub.id).unwrap();
            inner.subs[i].updated_ms = 111;
            inner.subs[i].node_count = 1;
            inner.subs[i].etag = Some("\"v7\"".into());
            inner.subs[i].traffic = Some(TrafficInfo {
                upload: 1,
                download: 2,
                left: 3,
                expire_ms: 4,
            });
        }
        let nodes_before = std::fs::read(&nodes_path).unwrap();
        let mut rx = svc.bus.subscribe("proxy.nodes_changed").unwrap();
        let after = svc.apply_sub_not_modified(&sub.id).unwrap();
        assert!(
            after.updated_ms > 111,
            "304 必须刷新更新时刻：{}",
            after.updated_ms
        );
        assert_eq!(after.node_count, 1);
        assert_eq!(after.etag.as_deref(), Some("\"v7\""));
        assert_eq!(after.traffic.as_ref().unwrap().left, 3);
        assert!(rx.try_recv().is_err(), "节点集未变不得播 nodes_changed");
        assert_eq!(
            std::fs::read(&nodes_path).unwrap(),
            nodes_before,
            "节点文件一字不动"
        );
        drop(svc);
        let svc2 = ProxyService::open(
            &dir,
            Arc::new(host_core::events::EventBus::new()),
            StdArc::new(MockSp::new()),
        )
        .unwrap();
        let s2 = &svc2.subs()[0];
        assert_eq!(s2.updated_ms, after.updated_ms, "落账必须持久化 subs.json");
        assert_eq!(
            s2.etag.as_deref(),
            Some("\"v7\""),
            "serde default 零迁移+持久往返"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn geoPrecheck_missingAsset_blocksThenPasses() {
        // 辅证（非任务书字面名）：geo 规则在场 × 内核所需资产缺失 → 起核前 Config 拒
        //（点名类别+资产+指路），零 start_attempts 零落盘；装上资产后同一调用放行
        let (svc, _sp, dir) = open_service("geopre");
        let arc: Arc<ProxyService> = svc;
        let d =
            TestKernelDriver::geo_gated("test-core-geo", "config-geo.json", &["singbox-geosite"]);
        inject(&arc, d.clone());
        arc.set_kernel("test-core-geo").unwrap();
        add_demo_node(&arc);
        arc.set_rules_v2(RulesV2 {
            rules: vec![RuleV2 {
                kind: "geo_site".into(),
                pattern: "cn".into(),
                target: "direct".into(),
                enabled: true,
            }],
            final_target: "proxy".into(),
            route_mode: "rule".into(),
        })
        .unwrap();
        let (_listener, _port) = stub_health_port(&arc);
        let err = arc.restart_with_config(false).unwrap_err();
        match err {
            ProxyError::Config(msg) => assert!(
                msg.contains("geosite")
                    && msg.contains("singbox-geosite")
                    && msg.contains("未安装"),
                "须点名类别+资产 id：{msg}"
            ),
            other => panic!("预检必须是 Config 错，得 {other:?}"),
        }
        assert_eq!(
            d.start_attempts.load(Ordering::SeqCst),
            0,
            "预检拦在起核之前"
        );
        assert!(
            !dir.join("proxy").join("config-geo.json").exists(),
            "预检拒时配置不得落盘"
        );
        // 正对照：raw 通道装入假字节资产 → 预检判据只看落盘事实，同一调用过闸
        sidecar::install_raw_asset(
            sidecar::artifact_for("singbox-geosite").unwrap(),
            &dir.join("proxy"),
            b"fake-geosite-db",
            "20260920133716",
            false,
        )
        .unwrap();
        arc.restart_with_config(false).unwrap();
        assert_eq!(d.start_attempts.load(Ordering::SeqCst), 1);
        arc.set_mode(Mode::Off).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- T-B2-11：测速与选节点（09 §5.2 字面回归） ----------------

    /// 多节点版 add_demo_node：同订阅 "sub-test"，按给定 tag 落内存态
    fn add_demo_nodes(svc: &Arc<ProxyService>, tags: &[&str]) {
        let mut inner = svc.inner.write();
        for t in tags {
            inner.nodes.push(Node {
                tag: (*t).into(),
                kind: NodeKind::Shadowsocks,
                server: "127.0.0.1".into(),
                port: 9,
                sub_id: "sub-test".into(),
                groups: Vec::new(),
                extra: serde_json::Value::Null,
            });
        }
    }

    fn selector_default_of(cfg: &crate::ir::IrConfig) -> Option<String> {
        cfg.outbounds.iter().find_map(|ob| match ob {
            crate::ir::IrOutbound::Selector {
                tag, default_tag, ..
            } if tag == crate::ir::TAG_PROXY => default_tag.clone(),
            _ => None,
        })
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    async fn delayTest_resultVectorFixedLength() {
        // 缺陷⑨ 红线：任一节点测速任务 panic/abort，结果向量长度仍恒等于节点数，
        // 挂掉的以 ms:None 占位（前端节点行不会凭空丢列）
        let (svc, _sp, dir) = open_service("delayvec");
        let arc: Arc<ProxyService> = svc;
        add_demo_nodes(&arc, &["ok1", "boom", "ok2"]);
        arc.delay_panic_tags.write().push("boom".into());
        let out = arc.delay_test().await;
        assert_eq!(out.len(), 3, "结果向量必须定长 == 节点数");
        let boom = out
            .iter()
            .find(|d| d.tag == "boom")
            .expect("boom 行不得消失");
        assert_eq!(boom.ms, None, "panic 臂须以 None 占位");
        assert_eq!(boom.sub_id, "sub-test");
        assert_eq!(
            out.iter().filter(|d| d.tag != "boom").count(),
            2,
            "其余节点照常出列"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    fn nodeSelect_absentTag_rejected() {
        // 红线：不存在的 (sub_id, tag) 即 NotFound 拒，且幽灵选定零写入（内存+状态皆原样）
        let (svc, _sp, dir) = open_service("selectabsent");
        let arc: Arc<ProxyService> = svc;
        add_demo_node(&arc);
        let err = arc.select_node("sub-test", "ghost").unwrap_err();
        match err {
            ProxyError::NotFound(msg) => assert!(
                msg.contains("sub-test/ghost") && msg.contains("节点不存在"),
                "须点名被拒节点：{msg}"
            ),
            other => panic!("缺失节点必须 NotFound 拒，得 {other:?}"),
        }
        let st = arc.status();
        assert_eq!(st.selected_node, None, "拒写不得扰动选定态");
        assert!(!st.selected_stale);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    fn nodeSelect_staleAfterSubUpdate_fallsBackFirstAndFlagsStale() {
        // 选定 → 订阅更新把该节点删掉：sticky 展示 + selected_stale 如实置位；
        // 消费侧回落首节点（= 手动选点前的旧行为），绝不把幽灵 tag 写进配置
        let (svc, _sp, dir) = open_service("selectstale");
        let arc: Arc<ProxyService> = svc;
        add_demo_nodes(&arc, &["n1", "n2"]);
        arc.select_node("sub-test", "n2").unwrap();
        let st = arc.status();
        assert_eq!(st.selected_node, Some(("sub-test".into(), "n2".into())));
        assert!(!st.selected_stale);

        // 模拟 sub_update 后 n2 被删的内存态（与真实更新对 inner.nodes 的替换等价）
        arc.inner.write().nodes.retain(|n| n.tag != "n2");
        let st = arc.status();
        assert_eq!(
            st.selected_node,
            Some(("sub-test".into(), "n2".into())),
            "失效不静默清选：sticky 展示供用户改选"
        );
        assert!(st.selected_stale, "删点后必须如实置 stale");
        // 再选幽灵即 NotFound（存在性校验收口）
        assert!(matches!(
            arc.select_node("sub-test", "n2"),
            Err(ProxyError::NotFound(_))
        ));

        // 消费臂回落：stale 选定 → 首节点 tag（既有单节点场景行为不变）
        let nodes = arc.inner.read().nodes.clone();
        let mut cfg =
            crate::ir::build(7890, false, &nodes, &[], crate::ir::TAG_PROXY, &[]).unwrap();
        ProxyService::apply_selected(&mut cfg, &nodes, Some(&("sub-test".into(), "n2".into())));
        assert_eq!(
            selector_default_of(&cfg).as_deref(),
            Some("sub-test:n1"),
            "stale 回落首节点"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    fn selectorDefaultTag_followsSelected() {
        // 纯函数消费臂（TestKernelDriver 渲染 "{}"，方言消费由各方言 golden 锁）：
        // 手动选定 → TAG_PROXY selector 的 default_tag 换人；None（自动）→ 不写幽灵
        let (svc, _sp, dir) = open_service("selectorsel");
        let arc: Arc<ProxyService> = svc;
        add_demo_nodes(&arc, &["n1", "n2"]);
        let nodes = arc.inner.read().nodes.clone();

        let base =
            || crate::ir::build(7890, false, &nodes, &[], crate::ir::TAG_PROXY, &[]).unwrap();

        let mut cfg = base();
        assert_eq!(selector_default_of(&cfg), None, "build 默认即自动");
        ProxyService::apply_selected(&mut cfg, &nodes, Some(&("sub-test".into(), "n2".into())));
        assert_eq!(selector_default_of(&cfg).as_deref(), Some("sub-test:n2"));
        ProxyService::apply_selected(&mut cfg, &nodes, Some(&("sub-test".into(), "n1".into())));
        assert_eq!(
            selector_default_of(&cfg).as_deref(),
            Some("sub-test:n1"),
            "换人即改写"
        );
        let mut cfg = base();
        ProxyService::apply_selected(&mut cfg, &nodes, None);
        assert_eq!(selector_default_of(&cfg), None, "自动态零扰动");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
    async fn egressProbe_kernelOff_honestErr() {
        // 红线：内核未在运行 ≠ 假 200 也 ≠ Ok(ok:false)——BadState 点名指路
        let (svc, _sp, dir) = open_service("egressoff");
        let arc: Arc<ProxyService> = svc;
        let err = arc.egress_probe().await.unwrap_err();
        match err {
            ProxyError::BadState(msg) => {
                assert!(msg.contains("内核未在运行"), "须点名真因：{msg}");
            }
            other => panic!("关闭态自检必须 BadState，得 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
