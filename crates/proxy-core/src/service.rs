//! ProxyService 门面（PR1–PR6 编排）：模式状态机 / 内核生命周期 / 订阅管理 / 延迟测试。
//!
//! 高危安全语义（docs/impl/05 风险标注）：
//! - 内核**意外退出** → 立即还原系统代理 + 模式归零（死端口 = 断网最高危）
//! - TUN 与系统代理**互斥**：开 TUN 前强制还原系统代理
//! - 所有还原走 `sysproxy::restore*`（备份优先，绝不猜用户原值）

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use host_core::events::{Event, EventBus};
use host_core::ports::SysProxyPort;
use serde::{Deserialize, Serialize};

use crate::config::{generate, GenOptions, RouteMode};
use crate::error::{ProxyError, Result};
use crate::kernel::{KernelDriver, KernelHandle, LogLine, SingBoxDriver};
use crate::sidecar;
use crate::sub::Node;
use crate::sysproxy;

const STATE_FILE: &str = "proxy_state.json";
const SUBS_FILE: &str = "subs.json";
const RULES_FILE: &str = "rules.json";
const CONFIG_FILE: &str = "config.json";
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
}

impl ProxyService {
    /// 打开服务：加载持久化 + 启动扫描还原 kill -9 残留（验收项）
    pub fn open(app_data_dir: &Path, bus: Arc<EventBus>, sp: Arc<dyn SysProxyPort>) -> Result<Arc<Self>> {
        let proxy_dir = app_data_dir.join("proxy");
        std::fs::create_dir_all(proxy_dir.join(SUBS_DIR))?;
        std::fs::create_dir_all(proxy_dir.join(BIN_DIR))?;

        let state: PersistState = std::fs::read(proxy_dir.join(STATE_FILE))
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or(PersistState { mixed_port: 7890 });
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
                subs,
                nodes,
                direct_domains,
            }),
            restored_last_run: RwLock::new(false),
        });

        // 启动扫描：上次运行残留的系统代理 → 自动还原（验收「kill -9 → 重启恢复」）
        let restored = sysproxy::restore_if_ours(&svc.proxy_dir, svc.sp.as_ref(), state.mixed_port)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "启动扫描还原系统代理失败");
                false
            });
        *svc.restored_last_run.write().expect("还原标志写锁") = restored;

        Ok(svc)
    }

    pub fn status(&self) -> StatusDto {
        let inner = self.inner.read().expect("代理内部状态读锁");
        let manifest = sidecar::read_manifest(&self.bin_dir());
        StatusDto {
            mode: inner.mode.as_str().into(),
            kernel_running: inner.handle.as_ref().map(|h| h.alive()).unwrap_or(false),
            kernel_id: inner.handle.as_ref().map(|h| h.driver_id().into()),
            inbound_port: inner.mixed_port,
            nodes_total: inner.nodes.len(),
            subs_total: inner.subs.len(),
            admin: self.sp.is_admin(),
            wintun_installed: sidecar::wintun_installed(&self.bin_dir()),
            kernel_installed: self.bin_dir().join("sing-box.exe").is_file(),
            kernel_version: manifest.map(|m| m.kernel_version),
            has_backup: sysproxy::has_backup(&self.proxy_dir),
            restored_last_run: *self.restored_last_run.read().expect("还原标志读锁"),
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
        Err(ProxyError::Download(format!("下载失败（已重试 {FETCH_RETRIES} 次）: {last_err}")))
    }

    // ---------------- 订阅管理（PR3） ----------------

    pub fn subs(&self) -> Vec<Sub> {
        self.inner.read().expect("代理内部状态读锁").subs.clone()
    }

    pub fn sub_add(&self, name: &str, url: &str) -> Result<Sub> {
        if url.trim().is_empty() {
            return Err(ProxyError::Subscription("订阅 URL 不能为空".into()));
        }
        let sub = Sub {
            id: uuid::Uuid::now_v7().to_string(),
            name: if name.trim().is_empty() { "订阅".into() } else { name.trim().to_string() },
            url: url.trim().to_string(),
            updated_ms: 0,
            node_count: 0,
        };
        let mut inner = self.inner.write().expect("代理内部状态写锁");
        inner.subs.push(sub.clone());
        self.save_subs(&inner)?;
        Ok(sub)
    }

    pub fn sub_remove(&self, id: &str) -> Result<bool> {
        let mut inner = self.inner.write().expect("代理内部状态写锁");
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
            let inner = self.inner.read().expect("代理内部状态读锁");
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
            let mut inner = self.inner.write().expect("代理内部状态写锁");
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
        self.save_subs(&self.inner.read().expect("代理内部状态读锁"))?;
        self.publish_nodes();
        Ok(sub)
    }

    pub fn nodes(&self) -> Vec<NodeDto> {
        let inner = self.inner.read().expect("代理内部状态读锁");
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
        self.inner.read().expect("代理内部状态读锁").direct_domains.clone()
    }

    pub fn set_direct_rules(&self, rules: Vec<String>) -> Result<()> {
        let mut cleaned: Vec<String> = rules
            .into_iter()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        cleaned.dedup();
        std::fs::write(self.proxy_dir.join(RULES_FILE), serde_json::to_vec(&cleaned)?)?;
        self.inner.write().expect("代理内部状态写锁").direct_domains = cleaned;
        Ok(())
    }

    /// mixed 端口（设置中心可改；改后需重新切模式生效）
    pub fn set_mixed_port(&self, port: u16) -> Result<()> {
        if port == 0 {
            return Err(ProxyError::BadState("端口不能为 0".into()));
        }
        let mut inner = self.inner.write().expect("代理内部状态写锁");
        inner.mixed_port = port;
        std::fs::write(
            self.proxy_dir.join(STATE_FILE),
            serde_json::to_vec(&PersistState { mixed_port: port })?,
        )?;
        Ok(())
    }

    // ---------------- 模式状态机（PR4/PR5 核心） ----------------

    /// 切换模式。同步方法（内核 spawn / 注册表写均为毫秒级），IPC 层 spawn_blocking。
    /// `self: &Arc<Self>`：on_exit 回调需要 Weak 引用避免 Service↔Handle 循环持有。
    pub fn set_mode(self: &Arc<Self>, mode: Mode) -> Result<()> {
        match mode {
            Mode::Off => self.stop_kernel_and_restore(),
            Mode::System => self.enter_system(),
            Mode::Tun => self.enter_tun(),
        }
    }

    fn enter_system(self: &Arc<Self>) -> Result<()> {
        // TUN → System 切换：先停旧内核
        self.restart_with_config(false)?;
        // 系统代理：备份原值 → 写入我们的 mixed 入站
        let port = self.inner.read().expect("代理内部状态读锁").mixed_port;
        sysproxy::enable(&self.proxy_dir, self.sp.as_ref(), port)?;
        {
            let mut inner = self.inner.write().expect("代理内部状态写锁");
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
        self.restart_with_config(true)?;
        let mut inner = self.inner.write().expect("代理内部状态写锁");
        inner.mode = Mode::Tun;
        drop(inner);
        self.publish_state();
        tracing::info!("TUN 模式已启用（系统代理已强制关闭）");
        Ok(())
    }

    fn stop_kernel_and_restore(&self) -> Result<()> {
        {
            let mut inner = self.inner.write().expect("代理内部状态写锁");
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

    /// 用当前节点重新生成配置并（重）启内核；起后健康探活 3s
    fn restart_with_config(self: &Arc<Self>, tun: bool) -> Result<()> {
        let (nodes, direct, port) = {
            let inner = self.inner.read().expect("代理内部状态读锁");
            (inner.nodes.clone(), inner.direct_domains.clone(), inner.mixed_port)
        };
        // 停旧内核（模式切换）
        {
            let mut inner = self.inner.write().expect("代理内部状态写锁");
            if let Some(h) = inner.handle.take() {
                h.stop();
            }
        }
        let cfg = generate(&GenOptions {
            mixed_port: port,
            // v1 路由：私有地址 + 用户直连域名恒直连，其余走代理（generate 内实现）
            mode: RouteMode::Rule,
            direct_domains: &direct,
            tun,
            nodes: &nodes,
        })?;
        let cfg_path = self.proxy_dir.join(CONFIG_FILE);
        std::fs::write(&cfg_path, serde_json::to_vec_pretty(&cfg)?)?;

        let weak = Arc::downgrade(self);
        let on_exit: Arc<dyn Fn(i32) + Send + Sync> = Arc::new(move |code| {
            // 内核意外退出：立即还原系统代理 + 模式归零（断网最高危场景兜底）
            tracing::error!(code, "代理内核意外退出");
            if let Some(svc) = weak.upgrade() {
                let _ = svc.stop_kernel_and_restore();
            }
        });

        let driver = SingBoxDriver::new(self.bin_dir().join("sing-box.exe"));
        let handle = driver.start(&cfg_path, on_exit)?;
        handle.set_log_cb({
            let bus = self.bus.clone();
            Arc::new(move |line: &str| {
                bus.publish(Event::new("proxy.log_line", "proxy", serde_json::json!({ "line": line })))
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
        self.inner.write().expect("代理内部状态写锁").handle = Some(handle);
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
        let nodes = self.inner.read().expect("代理内部状态读锁").nodes.clone();
        let mut tasks = Vec::with_capacity(nodes.len());
        for n in nodes {
            tasks.push(tokio::spawn(async move {
                let addr = std::net::SocketAddr::new(
                    resolve_host(&n.server).await.unwrap_or_else(|| std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
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
                NodeDelayDto { tag: n.tag, sub_id: n.sub_id, ms }
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
        let inner = self.inner.read().expect("代理内部状态读锁");
        inner.handle.as_ref().map(|h| h.logs_snapshot(limit)).unwrap_or_default()
    }

    // ---------------- 内部 ----------------

    fn bin_dir(&self) -> PathBuf {
        self.proxy_dir.join(BIN_DIR)
    }

    fn sub_nodes_path(&self, id: &str) -> PathBuf {
        self.proxy_dir.join(SUBS_DIR).join(format!("{id}.nodes.json"))
    }

    fn save_subs(&self, inner: &Inner) -> Result<()> {
        std::fs::write(self.proxy_dir.join(SUBS_FILE), serde_json::to_vec(&inner.subs)?)?;
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
                }),
            ))
            .ok();
    }

    fn publish_nodes(&self) {
        let inner = self.inner.read().expect("代理内部状态读锁");
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
        if let Ok(list) = std::fs::read(proxy_dir.join(SUBS_DIR).join(format!("{}.nodes.json", sub.id)))
            .and_then(|raw| serde_json::from_slice::<Vec<Node>>(&raw).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
        {
            sub.node_count = list.len();
            nodes.extend(list);
        }
    }
    Ok((subs, nodes))
}

async fn resolve_host(host: &str) -> Option<std::net::IpAddr> {
    use std::net::ToSocketAddrs;
    // server:port → 第一个地址的 IP（v1 用同步解析；DNS 失败返回 None → 连接必失败 → None 延迟）
    (host, 1u16)
        .to_socket_addrs()
        .ok()?
        .next()
        .map(|a| a.ip())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::ports::SysProxyState;
    use std::sync::{Arc as StdArc, Mutex};

    #[derive(Clone)]
    struct MockSp {
        state: StdArc<Mutex<SysProxyState>>,
    }

    impl MockSp {
        fn new() -> Self {
            Self { state: StdArc::new(Mutex::new(SysProxyState::default())) }
        }
    }

    impl SysProxyPort for MockSp {
        fn read(&self) -> std::result::Result<SysProxyState, host_core::error::AppError> {
            Ok(self.state.lock().unwrap().clone())
        }
        fn write(&self, state: &SysProxyState) -> std::result::Result<(), host_core::error::AppError> {
            *self.state.lock().unwrap() = state.clone();
            Ok(())
        }
        fn refresh(&self) -> std::result::Result<(), host_core::error::AppError> {
            Ok(())
        }
        fn is_admin(&self) -> bool {
            false
        }
    }

    fn open_service(tag: &str) -> (Arc<ProxyService>, StdArc<MockSp>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nf_proxy_svc_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sp = StdArc::new(MockSp::new());
        let svc = ProxyService::open(&dir, Arc::new(host_core::events::EventBus::new()), sp.clone()).unwrap();
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
        let svc2 = ProxyService::open(&dir, Arc::new(host_core::events::EventBus::new()), sp).unwrap();
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
        assert!(matches!(arc_svc.set_mode(Mode::Tun), Err(ProxyError::Permission(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn direct_rules_roundtrip() {
        let (svc, _sp, dir) = open_service("rules");
        svc.set_direct_rules(vec![".corp.example.com".into(), " internal.local ".into(), String::new()]).unwrap();
        assert_eq!(svc.direct_rules(), vec![".corp.example.com", "internal.local"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn b64_helper_still_works_for_sub_content() {
        use base64::Engine;
        // http_get 返回的字节可能整体 base64 —— 引擎能力自检
        let s = base64::engine::general_purpose::STANDARD.encode("hello");
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(&s).unwrap(), b"hello");
    }
}
