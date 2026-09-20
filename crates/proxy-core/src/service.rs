//! ProxyService 门面（PR1–PR6 编排）：模式状态机 / 内核生命周期 / 订阅管理 / 延迟测试。
//!
//! 高危安全语义（docs/impl/05 风险标注）：
//! - 内核**意外退出** → 立即还原系统代理 + 模式归零（死端口 = 断网最高危）
//! - TUN 与系统代理**互斥**：开 TUN 前强制还原系统代理
//! - 所有还原走 `sysproxy::restore*`（备份优先，绝不猜用户原值）

use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use host_core::events::{Event, EventBus};
use host_core::ports::SysProxyPort;
use serde::{Deserialize, Serialize};

use crate::error::{ProxyError, Result};
use crate::ir;
use crate::kernel::{driver_for, KernelCaps, KernelDriver, KernelHandle, LogLine, KERNEL_IDS};
use crate::sidecar;
use crate::sub::Node;
use crate::sysproxy;

const STATE_FILE: &str = "proxy_state.json";
const SUBS_FILE: &str = "subs.json";
const RULES_FILE: &str = "rules.json";
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistState {
    mixed_port: u16,
    /// 选定内核（T-B2-2）；serde default 零迁移：旧文件缺键 = sing-box
    #[serde(default = "default_kernel")]
    kernel: String,
}

fn default_kernel() -> String {
    KERNEL_SINGBOX.to_string()
}

/// 当前唯一注册内核 id（T-B2-5/6 增 xray/mihomo）
pub const KERNEL_SINGBOX: &str = "sing-box";

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
    direct_domains: Vec<String>,
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
            });
        let direct_domains: Vec<String> = std::fs::read(proxy_dir.join(RULES_FILE))
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();

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
                kernel: state.kernel,
                subs,
                nodes,
                direct_domains,
            }),
            restored_last_run: RwLock::new(false),
            #[cfg(test)]
            test_drivers: RwLock::new(Vec::new()),
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
        let manifest = sidecar::read_manifest(&self.bin_dir());
        let running_id = inner.handle.as_ref().map(|h| h.driver_id().to_string());
        let kernels: Vec<KernelInfoDto> = KERNEL_IDS
            .iter()
            .filter_map(|id| driver_for(&self.bin_dir(), id).ok())
            .map(|d| KernelInfoDto {
                id: d.id().to_string(),
                display_name: d.display_name().to_string(),
                installed: d.exe_path().is_file(),
                // 旧单 manifest 仅 sing-box 有版本记录（per-kernel manifest 归 T-B2-4）
                version: manifest
                    .as_ref()
                    .filter(|m| m.kernel_id == d.id())
                    .map(|m| m.kernel_version.clone()),
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
        }
    }

    /// 内核安装（PR2）：官方 Release 直链下载 → zip 校验解压 → manifest。网络路径 async。
    pub async fn kernel_install(&self, version: Option<String>) -> Result<sidecar::Manifest> {
        let version = version.unwrap_or_else(|| sidecar::DEFAULT_SINGBOX_VERSION.to_string());
        let url = sidecar::singbox_download_url(&version);
        let bytes = self.http_get(&url).await?;
        let manifest = sidecar::install_singbox_from_zip(&self.bin_dir(), &bytes, &version)?;
        tracing::info!(version = %version, sha256 = %manifest.sha256, "sing-box 内核安装完成");
        self.publish_state();
        Ok(manifest)
    }

    /// wintun.dll 安装（PR5 TUN 前置）
    pub async fn wintun_install(&self) -> Result<()> {
        let url = sidecar::wintun_download_url(sidecar::DEFAULT_WINTUN_VERSION);
        let bytes = self.http_get(&url).await?;
        sidecar::install_wintun_from_zip(&self.bin_dir(), &bytes)?;
        self.publish_state();
        Ok(())
    }

    /// 统一 HTTP 拉取（rustls + 显式 UA；订阅重试上限 3 次）
    async fn http_get(&self, url: &str) -> Result<Vec<u8>> {
        let client = reqwest::Client::builder()
            .user_agent(APP_UA)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| ProxyError::Download(format!("HTTP 客户端构建失败: {e}")))?;
        let mut last_err = String::new();
        for attempt in 0..FETCH_RETRIES {
            match client.get(url).send().await {
                Ok(resp) => match resp.bytes().await {
                    Ok(b) => return Ok(b.to_vec()),
                    Err(e) => last_err = format!("读取响应体失败: {e}"),
                },
                Err(e) => last_err = format!("请求失败: {e}"),
            }
            if attempt + 1 < FETCH_RETRIES {
                tokio::time::sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
            }
        }
        Err(ProxyError::Download(format!(
            "下载失败（已重试 {FETCH_RETRIES} 次）: {last_err}"
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
        self.publish_nodes();
        Ok(removed)
    }

    /// 拉取并解析订阅（PR3）；内容持久化到 subs/（敏感，不进日志）
    pub async fn sub_update(&self, id: &str) -> Result<Sub> {
        let url = {
            let inner = self.inner.read();
            inner
                .subs
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.url.clone())
                .ok_or_else(|| ProxyError::NotFound(format!("订阅 {id} 不存在")))?
        };
        let raw = self.http_get(&url).await?;
        let content = String::from_utf8_lossy(&raw[..]);
        let nodes = crate::sub::parse_subscription(&content, id)?;
        let node_count = nodes.len();

        // 持久化节点文件 + 更新元数据
        std::fs::write(self.sub_nodes_path(id), serde_json::to_vec(&nodes)?)?;
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
            sub.clone()
        };
        self.save_subs(&self.inner.read())?;
        self.publish_nodes();
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
            })
            .collect()
    }

    // ---------------- 直连规则 ----------------

    pub fn direct_rules(&self) -> Vec<String> {
        self.inner.read().direct_domains.clone()
    }

    pub fn set_direct_rules(&self, rules: Vec<String>) -> Result<()> {
        let mut cleaned: Vec<String> = rules
            .into_iter()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        cleaned.dedup();
        std::fs::write(
            self.proxy_dir.join(RULES_FILE),
            serde_json::to_vec(&cleaned)?,
        )?;
        self.inner.write().direct_domains = cleaned;
        Ok(())
    }

    /// mixed 端口（设置中心可改；改后需重新切模式生效）
    pub fn set_mixed_port(&self, port: u16) -> Result<()> {
        if port == 0 {
            return Err(ProxyError::BadState("端口不能为 0".into()));
        }
        let mut inner = self.inner.write();
        inner.mixed_port = port;
        self.persist_state(&inner)
    }

    /// 持久化状态文件唯一写点（双字段整包写，杜绝 set_mixed_port 曾有的 kernel 覆盖）
    fn persist_state(&self, inner: &Inner) -> Result<()> {
        std::fs::write(
            self.proxy_dir.join(STATE_FILE),
            serde_json::to_vec(&PersistState {
                mixed_port: inner.mixed_port,
                kernel: inner.kernel.clone(),
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
        let (nodes, direct, port, kernel) = {
            let inner = self.inner.read();
            (
                inner.nodes.clone(),
                inner.direct_domains.clone(),
                inner.mixed_port,
                inner.kernel.clone(),
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
        // v1 路由：私有地址 + 用户直连域名恒直连，其余走代理（ir::build 内实现）
        let ir_cfg = ir::build(
            port,
            tun,
            &nodes,
            &[] as &[ir::IrRule],
            ir::TAG_PROXY,
            &direct,
        )?;
        let rendered = driver.config_render(&ir_cfg)?;
        let cfg_path = self.proxy_dir.join(driver.cfg_name());
        std::fs::write(&cfg_path, rendered.as_bytes())?;

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

        // 健康探活：mixed 模式等端口可连；TUN 无本地端口 → 等进程存活即认为就绪
        let healthy = if tun {
            std::thread::sleep(Duration::from_millis(300));
            handle.alive()
        } else {
            let deadline = std::time::Instant::now() + Duration::from_millis(HEALTH_WAIT_MS);
            loop {
                if handle.health_check(port) {
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

    /// TCP 连接延迟（v1 轻量方案；每节点并发、3s 超时）
    pub async fn delay_test(&self) -> Vec<NodeDelayDto> {
        let nodes = self.inner.read().nodes.clone();
        let mut tasks = Vec::with_capacity(nodes.len());
        for n in nodes {
            tasks.push(tokio::spawn(async move {
                let addr = std::net::SocketAddr::new(
                    resolve_host(&n.server)
                        .await
                        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
                    n.port,
                );
                let start = std::time::Instant::now();
                let ms = match tokio::time::timeout(
                    Duration::from_secs(3),
                    tokio::net::TcpStream::connect(addr),
                )
                .await
                {
                    Ok(Ok(_)) => Some(start.elapsed().as_millis() as u64),
                    _ => None,
                };
                NodeDelayDto {
                    tag: n.tag,
                    sub_id: n.sub_id,
                    ms,
                }
            }));
        }
        let mut out = Vec::with_capacity(tasks.len());
        for t in tasks {
            if let Ok(dto) = t.await {
                out.push(dto);
            }
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
        std::fs::write(
            self.proxy_dir.join(SUBS_FILE),
            serde_json::to_vec(&inner.subs)?,
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

    fn publish_nodes(&self) {
        let inner = self.inner.read();
        self.bus
            .publish(Event::new(
                "proxy.nodes_changed",
                "proxy",
                serde_json::json!({ "sub_id": serde_json::Value::Null, "total": inner.nodes.len() }),
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
    }

    impl TestKernelDriver {
        fn new(id: &'static str, cfg: &'static str) -> StdArc<Self> {
            Self::with(id, cfg, true)
        }
        /// 起核必败驱动（换核回滚红线专用）
        fn failing(id: &'static str, cfg: &'static str) -> StdArc<Self> {
            Self::with(id, cfg, false)
        }
        fn with(id: &'static str, cfg: &'static str, succeeds: bool) -> StdArc<Self> {
            StdArc::new(Self {
                id,
                cfg,
                succeeds,
                start_attempts: AtomicUsize::new(0),
                fail_next: AtomicBool::new(false),
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
                tun: true,
                policy_groups: false,
                external_controller: false,
            }
        }
        fn config_render(&self, _ir: &IrConfig) -> Result<String> {
            Ok("{}\n".to_string())
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
}
