//! kvm-core 模块本体（docs/impl/05 K1–K7 宿主侧集成）。
//!
//! 生命周期：init 装载身份 + 构造发现服务；start 在专属 tokio Runtime 上
//! 运行心跳三任务（不依赖宿主是否已有 tokio 上下文）；stop 收割全部任务。

use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;

use host_core::config::ConfigStore;
use host_core::device::DeviceIdentity;
use host_core::error::ModuleError;
use host_core::events::Event;
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};
use host_core::ports::{
    ClipContent, CryptoPort, InputHookPort, InputInjectPort, KeyboardLedPort, RawInput,
    ScreenInfoPort, ScreenRect,
};

use crate::config::{EdgeMapConfig, KvmConfig};
use crate::discovery::{
    DiscoveryConfig, DiscoveryHandle, DiscoveryService, OwnIdentity, PeerEvent,
};
use crate::edge::{ControlReleasePayload, ControlTakePayload, Decision, Edge, EdgeSwitch};
use crate::locksync::sync_toward;
use crate::pairing::{PairCodeManager, PairStore, PairedPeer, PairingService};
use crate::session::{MsgType, SessionEvent, SessionHandle, SessionManager, SessionServeHandle};
use crate::transfer::{self, AckPayload, ChunkOutcome, MetaOutcome, TransferManager};

/// 本端主动发起（客户端角色）的会话句柄表：connect_to 登记入内，
/// Closed 事件时移除。服务端接入会话由 SessionManager 注册表持有。
type OutboundSessions = Arc<Mutex<HashMap<String, SessionHandle>>>;

/// 输入 worker 命令（钩子回调只决策不 IO，转发/切换由 worker 异步执行）
enum Cmd {
    /// 转发输入事件到受控设备
    Forward(RawInput, String),
    /// 切换控制权（发 ControlTake）
    Switch(String),
    /// 释放控制权（发 ControlRelease）
    Release(String),
}

// 句柄字段故意不读：存续即保活，drop 会 abort 对应任务（DiscoveryHandle/
// SessionServeHandle 的 Drop 兜底）
#[allow(dead_code)]
struct Running {
    _rt: tokio::runtime::Runtime,
    discovery: DiscoveryHandle,
    sessions: SessionServeHandle,
}

pub struct KvmModule {
    state: ModuleStateCell,
    bus: RwLock<Option<Arc<host_core::events::EventBus>>>,
    running: RwLock<Option<Running>>,
    discovery: RwLock<Option<Arc<DiscoveryService>>>,
    pairing: RwLock<Option<Arc<PairingService>>>,
    codes: RwLock<Option<Arc<PairCodeManager>>>,
    identity: RwLock<Option<Arc<DeviceIdentity>>>,
    store: RwLock<Option<Arc<PairStore>>>,
    /// K3 会话管理器（服务端会话注册表宿主）
    sessions: RwLock<Option<Arc<SessionManager>>>,
    /// 客户端角色会话句柄表
    outbound: RwLock<Option<OutboundSessions>>,
    /// K6 接收端状态机
    transfers: RwLock<Option<Arc<TransferManager>>>,
    /// 会话事件通道发送端（connect_to 登记客户端会话用）
    session_events: RwLock<Option<tokio::sync::mpsc::UnboundedSender<SessionEvent>>>,
    /// start 时的 runtime 句柄（send_file 后台任务 spawn 用）
    rt_handle: RwLock<Option<tokio::runtime::Handle>>,
    // ---- K7 输入端口与边缘切换状态 ----
    hook: RwLock<Option<Arc<dyn InputHookPort>>>,
    inject: RwLock<Option<Arc<dyn InputInjectPort>>>,
    screen: RwLock<Option<Arc<dyn ScreenInfoPort>>>,
    /// 锁键灯态端口（T-B7-9 修饰键同步；缺失=无同步面，两时机诚实 no-op）
    led: RwLock<Option<Arc<dyn KeyboardLedPort>>>,
    /// 边缘切换状态机（钩子线程锁内决策；worker 释放判定亦经此锁）
    edge_switch: Arc<Mutex<EdgeSwitch>>,
    /// 受控端标志（收到 ControlTake 置位）
    controlled: Arc<AtomicBool>,
    /// 本机虚拟桌面矩形（start 时测定）
    own_screen: RwLock<Option<ScreenRect>>,
    /// 输入 worker 命令通道（Release IPC / 钩子决策投递）
    cmd_tx: RwLock<Option<tokio::sync::mpsc::UnboundedSender<Cmd>>>,
    /// TCP 会话监听端口（默认 SESSION_PORT；同机多实例/环回测试可注入，须在 init 前设置）
    tcp_port: AtomicU16,
    /// UDP 组播发现端口（默认 discovery::DEFAULT_PORT；同上）
    discovery_port: AtomicU16,
    /// 本机 SYNC 监听端口（随心跳宣告；**0 = 本端不供同步**，由宿主在 init 前注入真值）
    /// ——默认 0 而不是 sync-core 的默认端口：kvm 不知道 sync 是否真的听了、听了哪个口，
    /// 填个"看起来对"的默认值就是让邻居拨一个本机可能不存在的端口（09 §10.2 T-B5-7）
    sync_port: AtomicU16,
    /// kvm 段 ConfigStore 句柄（T-B7-8 写侧持久，与 clipboard/vault 同形；
    /// `new()` 裸构造为 None = 纯运行态，同机环回验收用）
    config_store: RwLock<Option<Arc<ConfigStore>>>,
    /// 最近一次生效的 kvm 段配置（merged 派发基底，盘上镜像；边缘真源在 EdgeSwitch）
    cfg: RwLock<KvmConfig>,
}

impl Default for KvmModule {
    fn default() -> Self {
        Self::new()
    }
}

impl KvmModule {
    pub fn new() -> Self {
        Self::with_store(None)
    }

    /// 带 ConfigStore 句柄构造（T-B7-8）：set_edge_map 即时生效之外另落
    /// kvm 段持久值（读-改-写，与 clipboard `write_capture_paused` 同形）。
    pub fn new_with_config(config_store: Arc<ConfigStore>) -> Self {
        Self::with_store(Some(config_store))
    }

    fn with_store(store: Option<Arc<ConfigStore>>) -> Self {
        Self {
            state: ModuleStateCell::new(),
            bus: RwLock::new(None),
            running: RwLock::new(None),
            discovery: RwLock::new(None),
            pairing: RwLock::new(None),
            codes: RwLock::new(None),
            identity: RwLock::new(None),
            store: RwLock::new(None),
            sessions: RwLock::new(None),
            outbound: RwLock::new(None),
            transfers: RwLock::new(None),
            session_events: RwLock::new(None),
            rt_handle: RwLock::new(None),
            hook: RwLock::new(None),
            inject: RwLock::new(None),
            led: RwLock::new(None),
            screen: RwLock::new(None),
            edge_switch: Arc::new(Mutex::new(EdgeSwitch::new(HashMap::new()))),
            controlled: Arc::new(AtomicBool::new(false)),
            own_screen: RwLock::new(None),
            cmd_tx: RwLock::new(None),
            tcp_port: AtomicU16::new(crate::session::SESSION_PORT),
            discovery_port: AtomicU16::new(crate::discovery::DEFAULT_PORT),
            sync_port: AtomicU16::new(0),
            config_store: RwLock::new(store),
            cfg: RwLock::new(KvmConfig::default()),
        }
    }

    /// 同机多实例/环回验收（K9）：注入非默认端口；必须在 init 之前调用
    pub fn set_tcp_port(&self, port: u16) {
        self.tcp_port.store(port, Ordering::SeqCst);
    }

    pub fn set_discovery_port(&self, port: u16) {
        self.discovery_port.store(port, Ordering::SeqCst);
    }

    /// 注入本机 SYNC 监听端口供心跳宣告（09 §10.2 T-B5-7）。
    /// **方向：sync 端口的事实源在 sync-core，kvm 只是搬运**——本模块不认识 sync-core，
    /// 端口值由 `src-tauri/src/state.rs` 装配时读 `SyncModule::port()` 注入。
    /// 须在 init（发现服务装配）之前调用；发现服务在 init 时取一次快照，后改不重播。
    pub fn set_sync_port(&self, port: u16) {
        self.sync_port.store(port, Ordering::SeqCst);
    }

    fn tcp_port_now(&self) -> u16 {
        self.tcp_port.load(Ordering::SeqCst)
    }

    fn data_dir(ctx: &ModuleContext) -> std::path::PathBuf {
        ctx.app_data_dir.join("kvm")
    }

    /// 会话句柄查找：客户端会话（outbound）优先，其次服务端注册表
    fn session_to(&self, device_id: &str) -> Option<SessionHandle> {
        let outbound = self.outbound.read().clone();
        let sessions = self.sessions.read().clone();
        session_lookup(sessions.as_ref(), outbound.as_ref(), device_id)
    }

    /// IPC：签发一次性配对码，返回 (码, 有效期毫秒)
    pub fn issue_pair_code(&self) -> Result<(String, u64), ModuleError> {
        let codes = self.codes.read().clone().ok_or(ModuleError::NotReady)?;
        Ok(codes.issue())
    }

    /// IPC：向已发现设备发起配对（addr 由发现层解析）
    pub fn pair_with(
        &self,
        addr: std::net::SocketAddr,
        code: &str,
    ) -> Result<PairedPeer, ModuleError> {
        let pairing = self.pairing.read().clone().ok_or(ModuleError::NotReady)?;
        // 读守卫显式绑定到函数尾（临时值不可跨语句借用——工程教训见记忆）
        let running_guard = self.running.read();
        let rt = running_guard
            .as_ref()
            .map(|r| &r._rt)
            .ok_or(ModuleError::NotReady)?;
        // 模块 runtime 专属线程池上执行；命令线程短暂阻塞（握手 ≤10s）
        let result = rt.block_on(pairing.pair_with(addr, code));
        drop(running_guard);
        result.map_err(|e| ModuleError::Start(e.to_string()))
    }

    /// IPC：解除配对
    pub fn unpair(&self, device_id: &str) -> Result<bool, ModuleError> {
        let pairing = self.pairing.read().clone().ok_or(ModuleError::NotReady)?;
        pairing
            .unpair(device_id)
            .map_err(|e| ModuleError::Stop(e.to_string()))
    }

    /// IPC：已配对设备列表
    pub fn paired_peers(&self) -> Result<Vec<PairedPeer>, ModuleError> {
        let pairing = self.pairing.read().clone().ok_or(ModuleError::NotReady)?;
        Ok(pairing.paired_peers())
    }

    /// IPC：已发现邻居列表
    pub fn discovered_peers(&self) -> Result<Vec<crate::discovery::PeerInfo>, ModuleError> {
        let discovery = self.discovery.read().clone().ok_or(ModuleError::NotReady)?;
        Ok(discovery.peers_snapshot())
    }

    /// D-40 IPC：组播发现降级原因（None＝发现正常或未初始化）。
    /// 非 None 时邻居列表恒空是降级形态而非"身边没设备"，UI 必须说清。
    pub fn discovery_degraded(&self) -> Option<String> {
        self.discovery
            .read()
            .clone()
            .and_then(|d| d.degraded_reason())
    }

    /// IPC：向已配对设备发起客户端会话（connect_to）。返回对端 device_id。
    /// 会话句柄登记入 outbound 表（Closed 事件时自动移除）。
    pub fn connect_to(&self, addr: std::net::SocketAddr) -> Result<String, ModuleError> {
        let sessions = self.sessions.read().clone().ok_or(ModuleError::NotReady)?;
        let ev_tx = self
            .session_events
            .read()
            .clone()
            .ok_or(ModuleError::NotReady)?;
        let running_guard = self.running.read();
        let rt = running_guard
            .as_ref()
            .map(|r| &r._rt)
            .ok_or(ModuleError::NotReady)?;
        let handle = rt
            .block_on(sessions.connect(addr, ev_tx))
            .map_err(|e| ModuleError::Start(e.to_string()))?;
        drop(running_guard);
        let device_id = handle.device_id.clone();
        if let Some(outbound) = self.outbound.read().as_ref() {
            let mut map = outbound.lock();
            // 同设备旧会话关断，替换为新句柄
            if let Some(old) = map.insert(device_id.clone(), handle) {
                old.close();
            }
        }
        Ok(device_id)
    }

    /// IPC：发送剪贴板内容到对端（单帧；Files 变体不支持，逐文件走 send_file）
    pub fn send_clip(&self, device_id: &str, content: ClipContent) -> Result<(), ModuleError> {
        let handle = self
            .session_to(device_id)
            .ok_or_else(|| ModuleError::Start(format!("设备 {device_id} 无活跃会话")))?;
        let running_guard = self.running.read();
        let rt = running_guard
            .as_ref()
            .map(|r| &r._rt)
            .ok_or(ModuleError::NotReady)?;
        rt.block_on(transfer::send_clip(&handle, &content))
            .map_err(|e| ModuleError::Start(e.to_string()))
    }

    /// IPC：发送本地文件到对端（后台任务执行；进度经 kvm.file_progress 事件，
    /// 首个事件含 transfer_id，完成/失败经 kvm.transfer_ack）。
    pub fn send_file(&self, device_id: &str, path: String) -> Result<(), ModuleError> {
        let handle = self
            .session_to(device_id)
            .ok_or_else(|| ModuleError::Start(format!("设备 {device_id} 无活跃会话")))?;
        let rt = self.rt_handle.read().clone().ok_or(ModuleError::NotReady)?;
        let bus = self.bus.read().clone().ok_or(ModuleError::NotReady)?;
        rt.spawn(async move {
            let on_progress = {
                let bus = bus.clone();
                move |p: transfer::FileProgress| {
                    // D-03 统一背压：200ms 合并，key=transfer_id（阈值取 TOPIC_REGISTRY）
                    let key = p.transfer_id.clone();
                    let _ = bus.publish_merged(
                        Event::new(
                            "kvm.file_progress",
                            "kvm",
                            serde_json::to_value(&p).unwrap_or_default(),
                        ),
                        &key,
                        host_core::events::merged_window("kvm.file_progress"),
                    );
                }
            };
            match transfer::send_file(&handle, &path, Some(Box::new(on_progress))).await {
                Ok(transfer_id) => {
                    tracing::info!(device_id = %handle.device_id, transfer_id = %transfer_id, "文件发送完成，等待对端 Ack");
                }
                Err(e) => {
                    tracing::warn!(device_id = %handle.device_id, error = %e, "文件发送失败");
                    let _ = bus.publish(Event::new(
                        "kvm.transfer_ack",
                        "kvm",
                        serde_json::json!({ "transfer_id": null, "ok": false, "error": e.to_string() }),
                    ));
                }
            }
        });
        Ok(())
    }

    /// IPC：活跃会话列表（服务端接入 + 本端发起；device_id, device_name, role）
    pub fn session_list(&self) -> Vec<serde_json::Value> {
        let mut list = Vec::new();
        if let Some(outbound) = self.outbound.read().as_ref() {
            for h in outbound.lock().values() {
                list.push(serde_json::json!({
                    "device_id": h.device_id, "device_name": h.device_name, "role": "client",
                }));
            }
        }
        if let Some(sessions) = self.sessions.read().as_ref() {
            for (device_id, device_name) in sessions.active_sessions() {
                list.push(serde_json::json!({
                    "device_id": device_id, "device_name": device_name, "role": "server",
                }));
            }
        }
        list
    }

    // ---- K7 边缘切换 IPC ----

    /// IPC：设置 [设备→共享边] 映射（值 "left" | "right" | "up" | "down"，
    /// 空串 = 该设备取消映射；T-B7-8 扩四值）。运行中即时生效；带 ConfigStore
    /// 句柄时另落 kvm 段 `edge_map`（读-改-写，广播回流 apply_config 幂等重放）。
    /// 同一边至多一设备——状态机 `find_map` 择一在此显式化为整批弹回点名，
    /// 也让持久形制（边→设备）反演全函数。
    pub fn set_edge_map(&self, map: HashMap<String, String>) -> Result<(), ModuleError> {
        let mut edges = HashMap::new();
        let mut claims: HashMap<&'static str, String> = HashMap::new();
        let mut devs: Vec<(&String, &String)> = map.iter().collect();
        devs.sort();
        for (device_id, side) in devs {
            if side.is_empty() {
                continue;
            }
            let edge = match side.as_str() {
                "left" => Edge::Left,
                "right" => Edge::Right,
                "up" => Edge::Up,
                "down" => Edge::Down,
                other => return Err(ModuleError::Config(format!("非法边标识: {other}"))),
            };
            if let Some(prev) = claims.insert(edge.as_str(), device_id.clone()) {
                return Err(ModuleError::Config(format!(
                    "边 {} 被设备 {prev} 与 {device_id} 同时声明（每边至多一设备）",
                    edge.as_str()
                )));
            }
            edges.insert(device_id.clone(), edge);
        }
        self.edge_switch.lock().set_edges(edges);
        self.persist_edge_map();
        Ok(())
    }

    /// 写侧持久（T-B7-8）：现映射反演为边→设备四键表，读-改-写 kvm 段。
    /// 无句柄（裸构造）= 纯运行态不落盘；盘写失败仅告警——运行态已生效是
    /// 事实，回滚反而制造两态分裂（clipboard 同纪律）。
    fn persist_edge_map(&self) {
        let Some(store) = self.config_store.read().clone() else {
            return;
        };
        let runtime = self.edge_switch.lock().edges().clone();
        let dev_map: HashMap<String, String> = runtime
            .iter()
            .map(|(id, e)| (id.clone(), e.as_str().to_string()))
            .collect();
        let em = serde_json::to_value(EdgeMapConfig::from_device_map(&dev_map))
            .unwrap_or_else(|_| serde_json::json!({}));
        let mut values = store
            .get_module("kvm")
            .unwrap_or_else(|_| serde_json::json!({}));
        let port = self.discovery_port.load(Ordering::SeqCst);
        let obj = values.as_object_mut();
        let Some(obj) = obj else {
            tracing::warn!("kvm 配置段不是 JSON 对象，边缘映射未持久");
            return;
        };
        obj.insert("edge_map".into(), em);
        obj.insert("port".into(), serde_json::json!(port));
        let base = self.cfg.read().clone();
        if let Ok(next) = base.merged(&values) {
            *self.cfg.write() = next;
        }
        if let Err(e) = store.set_module("kvm", values) {
            tracing::warn!(error = %e, "kvm 边缘映射持久失败（运行态已生效）");
        }
    }

    /// IPC：当前边缘映射
    pub fn edge_map(&self) -> HashMap<String, String> {
        self.edge_switch
            .lock()
            .edges()
            .iter()
            .map(|(id, e)| (id.clone(), e.as_str().to_string()))
            .collect()
    }

    /// IPC：控制状态（role: idle/controlling/controlled + 上下文）
    pub fn control_state(&self) -> serde_json::Value {
        let es = self.edge_switch.lock();
        match es.controlling_device() {
            Some(device_id) => serde_json::json!({ "role": "controlling", "device_id": device_id }),
            None if self.controlled.load(Ordering::Relaxed) => {
                serde_json::json!({ "role": "controlled" })
            }
            None => serde_json::json!({ "role": "idle" }),
        }
    }

    /// IPC：手动释放控制权（UI 切回按钮；受控中/空闲时为 no-op）
    pub fn release_control(&self) -> Result<(), ModuleError> {
        let tx = self.cmd_tx.read().clone().ok_or(ModuleError::NotReady)?;
        let _ = tx.send(Cmd::Release("manual".into()));
        Ok(())
    }
}

impl Module for KvmModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "kvm",
            name: "键鼠共享",
            version: "0.1.0",
            icon: Some("kvm"),
            priority: priority_of("kvm"),
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let crypto = ctx.ports.get::<dyn CryptoPort>();
        // K7 输入端口（缺失时边缘切换降级关闭：仅发现/配对/传输可用）
        *self.hook.write() = ctx.ports.get::<dyn InputHookPort>();
        *self.inject.write() = ctx.ports.get::<dyn InputInjectPort>();
        *self.screen.write() = ctx.ports.get::<dyn ScreenInfoPort>();
        *self.led.write() = ctx.ports.get::<dyn KeyboardLedPort>();
        let dir = Self::data_dir(&ctx);
        let identity = Arc::new(DeviceIdentity::load_or_create(&dir, crypto)?);
        tracing::info!(
            device_id = %identity.device_id,
            fingerprint = %identity.pubkey_fingerprint,
            "KVM 模块初始化完成"
        );

        let bus = ctx.event_bus.clone();

        // ① 发现服务（K1）：在线/离线 → EventBus
        let screen_port = self.screen.read().clone();
        let own_screen = screen_port
            .as_ref()
            .and_then(|s| s.virtual_desktop().ok())
            .unwrap_or_default();
        *self.own_screen.write() = Some(own_screen);
        let discovery = DiscoveryService::new(
            OwnIdentity {
                device_id: identity.device_id.clone(),
                device_name: identity.device_name.clone(),
                pubkey_fingerprint: identity.pubkey_fingerprint.clone(),
                tcp_port: self.tcp_port_now(),
                caps: vec!["input".into(), "clip".into(), "file".into()],
                screen: own_screen,
                sync_port: self.sync_port.load(Ordering::SeqCst),
            },
            DiscoveryConfig {
                port: self.discovery_port.load(Ordering::SeqCst),
                ..DiscoveryConfig::default()
            },
        );
        let bus_for_discovery = bus.clone();
        discovery.set_event_cb(Box::new(move |ev| match ev {
            PeerEvent::Online(peer) => {
                let _ = bus_for_discovery.publish(Event::new(
                    "kvm.peer_online",
                    "kvm",
                    serde_json::to_value(&peer).unwrap_or_default(),
                ));
            }
            PeerEvent::Offline(device_id) => {
                let _ = bus_for_discovery.publish(Event::new(
                    "kvm.peer_offline",
                    "kvm",
                    serde_json::json!({ "device_id": device_id }),
                ));
            }
        }));

        // ② 配对服务（K2）：一次性码 + 指纹白名单持久化
        let codes = Arc::new(PairCodeManager::new());
        let store = Arc::new(
            PairStore::load_or_default(&dir).map_err(|e| ModuleError::Init(e.to_string()))?,
        );
        let pairing = PairingService::new(identity.clone(), codes.clone(), store.clone());
        // T-B7-9 首次配对时机：LED 端口下传配对服务（None=配对帧不携带 locks）
        pairing.set_led(self.led.read().clone());

        // ③ K6 接收端状态机：incoming 根目录
        let incoming_dir = dir.join("incoming");
        let transfers = Arc::new(TransferManager::new(&incoming_dir));

        *self.bus.write() = Some(bus);
        *self.discovery.write() = Some(discovery);
        *self.codes.write() = Some(codes);
        *self.pairing.write() = Some(pairing);
        *self.identity.write() = Some(identity);
        *self.store.write() = Some(store);
        *self.transfers.write() = Some(transfers);
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        if self.status() == ModuleState::Running {
            return Ok(());
        }
        let discovery = self
            .discovery
            .read()
            .clone()
            .ok_or_else(|| ModuleError::Start("start 先于 init".into()))?;
        let pairing = self
            .pairing
            .read()
            .clone()
            .ok_or_else(|| ModuleError::Start("start 先于 init".into()))?;
        let bus = self
            .bus
            .read()
            .clone()
            .ok_or_else(|| ModuleError::Start("start 先于 init".into()))?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| ModuleError::Start(format!("tokio runtime 创建失败: {e}")))?;

        let discovery_handle = rt
            .block_on(async { discovery.clone().run().await })
            .map_err(|e| ModuleError::Start(e.to_string()))?;

        // 配对接入监听（绑定失败多为端口占用：另一实例已在运行）
        let tcp_port = self.tcp_port_now();
        let listener = rt
            .block_on(async { tokio::net::TcpListener::bind(("0.0.0.0", tcp_port)).await })
            .map_err(|e| ModuleError::Start(format!("会话端口 {tcp_port} 绑定失败: {e}")))?;
        let bus_for_pairing = bus.clone();
        let paired_cb: crate::pairing::PairedCb = Arc::new(parking_lot::Mutex::new(Some(
            Box::new(move |peer: PairedPeer| {
                let _ = bus_for_pairing.publish(Event::new(
                    "kvm.paired",
                    "kvm",
                    serde_json::json!({ "peer": peer, "paired": true }),
                ));
            }),
        )));

        // SESSION_PORT 单监听分流：PairRequest→K2 配对；Hello→K3 会话
        let identity = self
            .identity
            .read()
            .clone()
            .ok_or_else(|| ModuleError::Start("start 先于 init".into()))?;
        let store = self
            .store
            .read()
            .clone()
            .ok_or_else(|| ModuleError::Start("start 先于 init".into()))?;
        let transfers = self
            .transfers
            .read()
            .clone()
            .ok_or_else(|| ModuleError::Start("start 先于 init".into()))?;
        let own_device_id = identity.device_id.clone();
        let session_mgr = SessionManager::new(identity, pairing.clone(), store);
        let outbound: OutboundSessions = Arc::new(Mutex::new(HashMap::new()));

        // 会话事件 → EventBus：Established/Closed → kvm.session_state；
        // 传输帧（K6）→ TransferManager + 回 Ack + 事件；其余帧 debug 记录
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let bus_for_sessions = bus.clone();
        let sessions_for_ev = session_mgr.clone();
        let outbound_for_ev = outbound.clone();
        let transfers_for_ev = transfers.clone();
        // K7 受控端：注入端口 + 受控标志（InputEvent/控制帧处理用）
        let inject_for_ev = self.inject.read().clone();
        // T-B7-9：受控端收 ControlTake 携带 locks 时对齐本机灯态（None=旧对端不静默同步）
        let led_for_ev = self.led.read().clone();
        let controlled_for_ev = self.controlled.clone();
        let edge_for_ev = self.edge_switch.clone();
        rt.spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                match ev {
                    SessionEvent::Established { device_id, .. } => {
                        let _ = bus_for_sessions.publish(Event::new(
                            "kvm.session_state",
                            "kvm",
                            serde_json::json!({ "device_id": device_id, "state": "established" }),
                        ));
                    }
                    SessionEvent::Closed { device_id, reason } => {
                        // 客户端角色会话出表；服务端会话由 SessionManager 自行移除
                        outbound_for_ev.lock().remove(&device_id);
                        // K7：会话断开必须复位控制状态（控制中 → 强制释放；受控中 → 复位）
                        edge_for_ev.lock().force_release("session");
                        controlled_for_ev.store(false, Ordering::Relaxed);
                        let _ = bus_for_sessions.publish(Event::new(
                            "kvm.session_state",
                            "kvm",
                            serde_json::json!({ "device_id": device_id, "state": "closed", "reason": reason }),
                        ));
                    }
                    SessionEvent::Frame { device_id, frame } => match frame.msg_type {
                        // K6 剪贴板：直接上抛事件（写回系统剪贴板由剪贴板模块订阅
                        // kvm.clip_received 完成，D-25 已接线：回写 + origin=remote 入库）
                        crate::session::MsgType::ClipData => {
                            match serde_json::from_slice::<ClipContent>(&frame.payload) {
                                Ok(content) => {
                                    let _ = bus_for_sessions.publish(Event::new(
                                        "kvm.clip_received",
                                        "kvm",
                                        serde_json::json!({ "device_id": device_id, "content": content }),
                                    ));
                                }
                                Err(e) => tracing::warn!(error = %e, "ClipData 载荷非法"),
                            }
                        }
                        // K6 文件：FileMeta 建档 / FileChunk 落盘 / Ack 回执
                        crate::session::MsgType::FileMeta => {
                            let transfers = transfers_for_ev.clone();
                            let payload = frame.payload.clone();
                            // fs 慢操作移出事件循环
                            let res = tokio::task::spawn_blocking(move || {
                                let meta: transfer::FileMetaPayload =
                                    serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
                                let out = transfers.on_meta(meta.clone()).map_err(|e| e.to_string())?;
                                Ok::<_, String>((meta.transfer_id, meta.name, meta.size, out))
                            })
                            .await;
                            match res {
                                Ok(Ok((transfer_id, name, size, outcome))) => match outcome {
                                    // 空文件直通：直接回 Ack + 完成事件
                                    MetaOutcome::Completed { final_path } => {
                                        send_ack_to(
                                            &sessions_for_ev,
                                            &outbound_for_ev,
                                            &device_id,
                                            &AckPayload { transfer_id: transfer_id.clone(), ok: true, error: None },
                                        )
                                        .await;
                                        let _ = bus_for_sessions.publish(Event::new(
                                            "kvm.file_received",
                                            "kvm",
                                            serde_json::json!({
                                                "device_id": device_id, "transfer_id": transfer_id,
                                                "name": name, "size": size, "path": final_path,
                                            }),
                                        ));
                                    }
                                    MetaOutcome::Accepted { received, total_chunks } => {
                                        let _ = bus_for_sessions.publish(Event::new(
                                            "kvm.file_incoming",
                                            "kvm",
                                            serde_json::json!({
                                                "device_id": device_id, "transfer_id": transfer_id,
                                                "name": name, "size": size,
                                                "received": received, "total_chunks": total_chunks,
                                            }),
                                        ));
                                    }
                                },
                                Ok(Err(e)) => tracing::warn!(error = %e, "FileMeta 处理失败"),
                                Err(e) => tracing::warn!(error = %e, "FileMeta spawn_blocking 失败"),
                            }
                        }
                        crate::session::MsgType::FileChunk => {
                            // transfer_id = 块头前 32B（失败时也能定位回 Ack）
                            let transfer_id = if frame.payload.len() >= 32 {
                                transfer::hex_str(&frame.payload[..32])
                            } else {
                                String::new()
                            };
                            let transfers = transfers_for_ev.clone();
                            let payload = frame.payload.clone();
                            let res = tokio::task::spawn_blocking(move || {
                                transfers.on_chunk(&payload).map_err(|e| e.to_string())
                            })
                            .await;
                            match res {
                                Ok(Ok(outcome)) => match outcome {
                                    ChunkOutcome::InProgress { received, total_chunks } => {
                                        tracing::debug!(device_id = %device_id, received, total_chunks, "文件接收中");
                                    }
                                    ChunkOutcome::Completed { final_path } => {
                                        send_ack_to(
                                            &sessions_for_ev,
                                            &outbound_for_ev,
                                            &device_id,
                                            &AckPayload { transfer_id: transfer_id.clone(), ok: true, error: None },
                                        )
                                        .await;
                                        let name = final_path
                                            .file_name()
                                            .and_then(|n| n.to_str())
                                            .unwrap_or_default()
                                            .to_string();
                                        let _ = bus_for_sessions.publish(Event::new(
                                            "kvm.file_received",
                                            "kvm",
                                            serde_json::json!({
                                                "device_id": device_id, "transfer_id": transfer_id,
                                                "name": name, "path": final_path,
                                            }),
                                        ));
                                    }
                                },
                                Ok(Err(e)) => {
                                    tracing::warn!(device_id = %device_id, transfer_id = %transfer_id, error = %e, "FileChunk 处理失败");
                                    // 失败回执：发送方据此终止/提示
                                    send_ack_to(
                                        &sessions_for_ev,
                                        &outbound_for_ev,
                                        &device_id,
                                        &AckPayload { transfer_id, ok: false, error: Some(e) },
                                    )
                                    .await;
                                }
                                Err(e) => tracing::warn!(error = %e, "FileChunk spawn_blocking 失败"),
                            }
                        }
                        crate::session::MsgType::Ack => {
                            match serde_json::from_slice::<AckPayload>(&frame.payload) {
                                Ok(ack) => {
                                    let _ = bus_for_sessions.publish(Event::new(
                                        "kvm.transfer_ack",
                                        "kvm",
                                        serde_json::to_value(&ack).unwrap_or_default(),
                                    ));
                                }
                                Err(e) => tracing::warn!(error = %e, "Ack 载荷非法"),
                            }
                        }
                        // K7 受控端：ControlTake 置位后，InputEvent → K5 注入
                        MsgType::ControlTake => {
                            match serde_json::from_slice::<ControlTakePayload>(&frame.payload) {
                                Ok(p) => {
                                    controlled_for_ev.store(true, Ordering::Relaxed);
                                    tracing::info!(by = %p.by, edge = ?p.edge, "进入受控状态");
                                    let _ = bus_for_sessions.publish(Event::new(
                                        "kvm.control_state",
                                        "kvm",
                                        serde_json::json!({ "role": "controlled", "by": p.by, "edge": p.edge }),
                                    ));
                                    // T-B7-9 切换成功时机：向控制方灯态对齐
                                    // （keybd_event+读回重试移出事件循环，同 FileMeta 慢操作纪律）
                                    if let (Some(remote), Some(led)) = (p.locks, led_for_ev.clone())
                                    {
                                        tokio::task::spawn_blocking(move || {
                                            sync_toward(led.as_ref(), &remote);
                                        });
                                    }
                                }
                                Err(e) => tracing::warn!(error = %e, "ControlTake 载荷非法"),
                            }
                        }
                        MsgType::ControlRelease => {
                            match serde_json::from_slice::<ControlReleasePayload>(&frame.payload) {
                                Ok(p) => {
                                    controlled_for_ev.store(false, Ordering::Relaxed);
                                    tracing::info!(by = %p.by, reason = %p.reason, "控制权已归还");
                                    let _ = bus_for_sessions.publish(Event::new(
                                        "kvm.control_state",
                                        "kvm",
                                        serde_json::json!({ "role": "released", "by": p.by, "reason": p.reason }),
                                    ));
                                }
                                Err(e) => tracing::warn!(error = %e, "ControlRelease 载荷非法"),
                            }
                        }
                        MsgType::InputEvent => {
                            if controlled_for_ev.load(Ordering::Relaxed) {
                                match serde_json::from_slice::<RawInput>(&frame.payload) {
                                    Ok(input) => {
                                        if let Some(inject) = inject_for_ev.as_ref() {
                                            if let Err(e) = inject.inject(&[input]) {
                                                tracing::warn!(error = %e, "输入注入失败");
                                            }
                                        } else {
                                            tracing::warn!("收到 InputEvent 但注入端口不可用");
                                        }
                                    }
                                    Err(e) => tracing::warn!(error = %e, "InputEvent 载荷非法"),
                                }
                            }
                        }
                        other => {
                            tracing::debug!(
                                device_id = %device_id,
                                msg_type = ?other,
                                len = frame.payload.len(),
                                "KVM 会话帧"
                            );
                        }
                    },
                }
            }
        });

        let sessions_handle = rt.block_on(async {
            session_mgr
                .clone()
                .serve(listener, ev_tx.clone(), paired_cb)
                .await
        });

        // ---- K7 输入捕获 + 转发 worker（端口缺失则降级：仅发现/配对/传输）----
        let hook_port = self.hook.read().clone();
        let own_screen = self.own_screen.read().unwrap_or_default();
        if let (Some(hook), true) = (hook_port, own_screen.w > 0 && own_screen.h > 0) {
            let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Cmd>();
            // 钩子回调：仅决策（<5ms），IO 全部投递 worker
            let es_for_cb = self.edge_switch.clone();
            let tx_for_cb = cmd_tx.clone();
            let rect = own_screen;
            hook.start_capture(Box::new(move |ev: &RawInput| {
                // PERF-01：回调内**单次**取锁。决策状态机逐事件可变（按键集合/armed/
                // last_forwarded），锁不可全免；此前 Forward 臂会再取一次锁——worker
                // 正持同一锁时，低级钩子线程的阻塞被放大一倍（WH_KEYBOARD_LL 在系统
                // 输入路径上同步执行）。设备名在决策同一临界区内解析，语义与原先
                // "决策后快查"完全一致（受控态下 on_local_event 不改变 controlling_device）。
                let (decision, forward_device) = {
                    let mut es = es_for_cb.lock();
                    let d = es.on_local_event(ev, &rect);
                    let dev = if matches!(d, Decision::Forward) {
                        Some(es.controlling_device().unwrap_or_default().to_string())
                    } else {
                        None
                    };
                    (d, dev)
                };
                match decision {
                    Decision::Passthrough => true,
                    Decision::Suppress => false,
                    Decision::Forward => {
                        let _ = tx_for_cb
                            .send(Cmd::Forward(ev.clone(), forward_device.unwrap_or_default()));
                        false // 抑制本地（接管模式）
                    }
                    Decision::SwitchTo(device) => {
                        let _ = tx_for_cb.send(Cmd::Switch(device));
                        true // 触发切换的那次移动本地放行
                    }
                }
            }))
            .map_err(|e| ModuleError::Start(e.to_string()))?;

            // worker：转发 / 切换 / 释放（含边缘回移判定）
            let sessions_w = session_mgr.clone();
            let outbound_w = outbound.clone();
            let discovery_w = discovery.clone();
            let es_w = self.edge_switch.clone();
            let bus_w = bus.clone();
            let led_w = self.led.read().clone();
            let own_id = own_device_id;
            rt.spawn(async move {
                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        Cmd::Forward(ev, device) => {
                            let Some(handle) =
                                session_lookup(Some(&sessions_w), Some(&outbound_w), &device)
                            else {
                                continue;
                            };
                            let Ok(payload) = serde_json::to_vec(&ev) else { continue };
                            let frame = crate::session::Frame {
                                msg_type: MsgType::InputEvent,
                                flags: 0,
                                payload,
                            };
                            if handle.send(frame).await.is_err() {
                                continue;
                            }
                            // 边缘回移：仅鼠标移动需要判定（优先于快捷键）
                            if matches!(ev, RawInput::MouseMove { .. }) {
                                let peer_screen =
                                    discovery_w.peer(&device).map(|p| p.screen).unwrap_or_default();
                                let release = es_w.lock().should_release(&peer_screen);
                                if release {
                                    es_w.lock().force_release("edge");
                                    let payload = serde_json::to_vec(&ControlReleasePayload {
                                        by: own_id.clone(),
                                        reason: "edge".into(),
                                    })
                                    .unwrap_or_default();
                                    let _ = handle
                                        .send(crate::session::Frame {
                                            msg_type: MsgType::ControlRelease,
                                            flags: 0,
                                            payload,
                                        })
                                        .await;
                                    let _ = bus_w.publish(Event::new(
                                        "kvm.control_state",
                                        "kvm",
                                        serde_json::json!({ "role": "idle", "reason": "edge" }),
                                    ));
                                    tracing::info!(device = %device, "边缘回移 → 控制权归还");
                                }
                            }
                        }
                        Cmd::Switch(device) => {
                            match session_lookup(Some(&sessions_w), Some(&outbound_w), &device) {
                                Some(handle) => {
                                    let edge =
                                        es_w.lock().edges().get(&device).copied();
                                    let payload = serde_json::to_vec(&ControlTakePayload {
                                        by: own_id.clone(),
                                        edge: edge.unwrap_or(Edge::Right),
                                        // T-B7-9：顺带携带本机灯态，受控端据此对齐
                                        locks: led_w.as_ref().map(|p| p.read_lock_states()),
                                    })
                                    .unwrap_or_default();
                                    let frame = crate::session::Frame {
                                        msg_type: MsgType::ControlTake,
                                        flags: 0,
                                        payload,
                                    };
                                    if handle.send(frame).await.is_ok() {
                                        let _ = bus_w.publish(Event::new(
                                            "kvm.control_state",
                                            "kvm",
                                            serde_json::json!({
                                                "role": "controlling", "device_id": device,
                                            }),
                                        ));
                                    } else {
                                        es_w.lock().force_release("session");
                                    }
                                }
                                None => {
                                    // 目标无会话：立即归还本地控制（状态机已 Controlling）
                                    es_w.lock().force_release("no-session");
                                    let _ = bus_w.publish(Event::new(
                                        "kvm.control_state",
                                        "kvm",
                                        serde_json::json!({ "role": "idle", "reason": "no-session" }),
                                    ));
                                }
                            }
                        }
                        Cmd::Release(reason) => {
                            // 手动释放：找出当前受控设备发 ControlRelease
                            let device = es_w.lock().controlling_device().map(str::to_string);
                            es_w.lock().force_release(&reason);
                            if let Some(device) = device {
                                if let Some(handle) =
                                    session_lookup(Some(&sessions_w), Some(&outbound_w), &device)
                                {
                                    let payload = serde_json::to_vec(&ControlReleasePayload {
                                        by: own_id.clone(),
                                        reason: reason.clone(),
                                    })
                                    .unwrap_or_default();
                                    let _ = handle
                                        .send(crate::session::Frame {
                                            msg_type: MsgType::ControlRelease,
                                            flags: 0,
                                            payload,
                                        })
                                        .await;
                                }
                            }
                            let _ = bus_w.publish(Event::new(
                                "kvm.control_state",
                                "kvm",
                                serde_json::json!({ "role": "idle", "reason": reason }),
                            ));
                        }
                    }
                }
            });
            *self.cmd_tx.write() = Some(cmd_tx);
            tracing::info!("K7 输入捕获与边缘切换已启动");
        } else {
            tracing::warn!("输入钩子或屏幕信息端口缺失，K7 边缘切换降级关闭");
        }

        *self.sessions.write() = Some(session_mgr);
        *self.outbound.write() = Some(outbound);
        *self.session_events.write() = Some(ev_tx);
        *self.rt_handle.write() = Some(rt.handle().clone());

        *self.running.write() = Some(Running {
            _rt: rt,
            discovery: discovery_handle,
            sessions: sessions_handle,
        });
        self.state.set(ModuleState::Running);
        tracing::info!("KVM 发现、配对、会话与传输服务已启动");
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        // K7：先停输入捕获（钩子卸载），复位控制状态
        if let Some(hook) = self.hook.read().as_ref() {
            let _ = hook.stop_capture();
        }
        self.controlled.store(false, Ordering::Relaxed);
        self.edge_switch.lock().force_release("module-stop");
        // 客户端会话句柄先行关断（Drop 发 close 信号）
        *self.outbound.write() = None;
        *self.session_events.write() = None;
        *self.rt_handle.write() = None;
        *self.cmd_tx.write() = None;
        *self.sessions.write() = None;
        if let Some(running) = self.running.write().take() {
            // shutdown 需 await：abort 路径由 Drop 兜底（同步上下文）
            drop(running);
        }
        self.state.set(ModuleState::Stopped);
        tracing::info!("KVM 服务已停止");
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        // 键集 ≡ KvmConfig 字段集（死键守卫测钉死）；default 与 Default 逐值同源。
        // T-B7-8：enabled 死键除名（全局 enabled_modules 是真开关）；edge_map 入表
        serde_json::json!({
            "type": "object",
            "properties": {
                "port": { "type": "integer", "title": "发现端口", "default": crate::discovery::DEFAULT_PORT },
                "edge_map": {
                    "type": "object",
                    "title": "设备共享边缘（边→设备 id，空串=未映射）",
                    "default": { "left": "", "right": "", "up": "", "down": "" },
                    "properties": {
                        "left": { "type": "string" },
                        "right": { "type": "string" },
                        "up": { "type": "string" },
                        "down": { "type": "string" }
                    }
                }
            }
        })
    }

    /// 派发口（T-B7-8 起真消费，B6 承重⑭ merged 三件套形制）：先经
    /// [`KvmConfig::merged`] 全有或全无（缺键不动运行态；坏值整批弹回点名），
    /// 好值即刻写进 EdgeSwitch 真源（**不重启生效**，Arc::ptr_eq 判据测钉）；
    /// port 存 `discovery_port` 现值——发现服务在 start 取快照，下次 start 生效
    fn apply_config(&self, values: serde_json::Value) -> Result<(), ModuleError> {
        let base = self.cfg.read().clone();
        let next = base.merged(&values).map_err(ModuleError::Config)?;
        if next.port != base.port {
            self.discovery_port.store(next.port, Ordering::SeqCst);
            tracing::info!(port = next.port, "kvm 发现端口已存值（下次 start 生效）");
        }
        if next.edge_map != base.edge_map {
            self.edge_switch.lock().set_edges(next.edge_map.to_edges());
        }
        *self.cfg.write() = next;
        Ok(())
    }

    fn status(&self) -> ModuleState {
        self.state.get()
    }

    fn set_status(&self, state: ModuleState) {
        self.state.set(state);
    }
}

/// 会话句柄查找（独立函数供 worker 闭包使用）：客户端会话（outbound）优先，
/// 其次服务端注册表。
fn session_lookup(
    sessions: Option<&Arc<SessionManager>>,
    outbound: Option<&OutboundSessions>,
    device_id: &str,
) -> Option<SessionHandle> {
    if let Some(outbound) = outbound {
        if let Some(h) = outbound.lock().get(device_id).cloned() {
            return Some(h);
        }
    }
    sessions.and_then(|s| s.get_handle(device_id))
}

/// 发送 Ack 帧到对端：客户端角色会话查 outbound 表，服务端接入会话查
/// SessionManager 注册表；两侧都查不到（会话刚断）时静默放弃。
async fn send_ack_to(
    sessions: &Arc<SessionManager>,
    outbound: &OutboundSessions,
    device_id: &str,
    ack: &AckPayload,
) {
    let handle = outbound
        .lock()
        .get(device_id)
        .cloned()
        .or_else(|| sessions.get_handle(device_id));
    let Some(handle) = handle else {
        tracing::debug!(device_id, "回 Ack 时会话已断开，放弃");
        return;
    };
    match transfer::ack_frame(ack) {
        Ok(frame) => {
            if let Err(e) = handle.send(frame).await {
                tracing::warn!(device_id, error = %e, "回 Ack 发送失败");
            }
        }
        Err(e) => tracing::warn!(error = %e, "Ack 帧编码失败"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::events::EventBus;

    /// 自闭合夹具目录（T-B7-7 门禁事故同款纪律：pid 会被复用，补纳秒盐并先清场）
    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let salt = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!("nf-kvm-{tag}-{}-{salt}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store_with_schema(dir: &std::path::Path, bus: &Arc<EventBus>) -> Arc<ConfigStore> {
        let store = Arc::new(ConfigStore::new(dir.join("config"), bus.clone()));
        store.register_schema("kvm", KvmModule::new().config_schema());
        store
    }

    fn four_edge_map() -> HashMap<String, String> {
        HashMap::from([
            ("dev-a".to_string(), "left".to_string()),
            ("dev-b".to_string(), "right".to_string()),
            ("dev-c".to_string(), "up".to_string()),
            ("dev-d".to_string(), "down".to_string()),
        ])
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-8）字面测试名优先于 rustc 命名惯例
    fn edgeMap_fourEdges_roundTripViaConfigStore() {
        let dir = tmp_dir("rt");
        let bus = Arc::new(EventBus::new());
        let store = store_with_schema(&dir, &bus);
        let m = KvmModule::new_with_config(store.clone());
        let es0 = m.edge_switch.clone();
        let map = four_edge_map();
        m.set_edge_map(map.clone()).unwrap();
        // ① 运行态即刻生效（不重启）
        assert_eq!(m.edge_map(), map);
        // ② 盘写：四键恒全写的 边→设备 形制
        let disk = store.get_module("kvm").unwrap();
        assert_eq!(
            disk["edge_map"],
            serde_json::json!({
                "left": "dev-a", "right": "dev-b", "up": "dev-c", "down": "dev-d"
            })
        );
        // ③ 广播回流派发口重放（run_config_feed 的等价步）：内存态三方一致
        m.apply_config(disk.clone()).unwrap();
        assert_eq!(m.edge_map(), map);
        assert!(
            Arc::ptr_eq(&es0, &m.edge_switch),
            "配置生效不得以换代 EdgeSwitch（重启服务）为代价——派发证明，B6 T-B6-6 判据形制"
        );
        let seen: HashMap<String, String> = es0
            .lock()
            .edges()
            .iter()
            .map(|(id, e)| (id.clone(), e.as_str().to_string()))
            .collect();
        assert_eq!(seen, map, "同一 Arc 同一 Mutex：worker 侧立即可见新映射");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-8）字面测试名优先于 rustc 命名惯例
    fn edgeMap_legacyTwoEdgeJson_stillLoads() {
        let m = KvmModule::new();
        // 手写旧格式字节夹具（B6 纪律）：T-B7-8 前盘上只有 left/right 两键
        let legacy: serde_json::Value =
            serde_json::from_str(r#"{"port":49801,"edge_map":{"left":"dev-a","right":"dev-b"}}"#)
                .unwrap();
        m.apply_config(legacy.clone()).unwrap();
        assert_eq!(
            m.edge_map(),
            HashMap::from([
                ("dev-a".to_string(), "left".to_string()),
                ("dev-b".to_string(), "right".to_string()),
            ]),
            "旧两缘 JSON 可读；up/down 默认空 = 未映射"
        );
        assert_eq!(
            m.discovery_port.load(Ordering::SeqCst),
            49801,
            "port 复活真消费"
        );
        // 更旧盘（T-B7-8 前无 edge_map 键）：缺键不动现运行态
        m.apply_config(serde_json::json!({"port": 49802})).unwrap();
        assert_eq!(
            m.edge_map(),
            HashMap::from([
                ("dev-a".to_string(), "left".to_string()),
                ("dev-b".to_string(), "right".to_string()),
            ]),
            "缺 edge_map 键不得清空已生效映射"
        );
        assert_eq!(m.discovery_port.load(Ordering::SeqCst), 49802);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-8）字面测试名优先于 rustc 命名惯例
    fn setEdgeMap_illegalName_rejectedNamingValue() {
        let m = KvmModule::new();
        let e = match m.set_edge_map(HashMap::from([(
            "dev-x".to_string(),
            "diagonal".to_string(),
        )])) {
            Err(e) => e,
            Ok(()) => panic!("集外边标识必须弹回"),
        };
        assert!(e.to_string().contains("diagonal"), "拒因点名坏值: {e}");
        // 同缘双设备：整批弹回点名两设备与坏缘
        let e = m
            .set_edge_map(HashMap::from([
                ("dev-a".to_string(), "left".to_string()),
                ("dev-b".to_string(), "left".to_string()),
            ]))
            .unwrap_err();
        let msg = e.to_string();
        assert!(
            msg.contains("dev-a") && msg.contains("dev-b") && msg.contains("left"),
            "重复占缘点名双方与边: {msg}"
        );
        // 空串 = 取消映射（前端「未设置」项的后端归宿）
        m.set_edge_map(four_edge_map()).unwrap();
        let mut unmap = four_edge_map();
        unmap.insert("dev-c".to_string(), String::new());
        m.set_edge_map(unmap).unwrap();
        let after = m.edge_map();
        assert!(!after.contains_key("dev-c"));
        assert_eq!(after.get("dev-a").map(String::as_str), Some("left"));
        // 派发口同纪律：坏边键名整批弹回点名
        let e = m
            .apply_config(serde_json::json!({"edge_map": {"diagonal": "d"}}))
            .unwrap_err();
        assert!(e.to_string().contains("diagonal"), "拒因点名: {e}");
        assert_eq!(m.edge_map(), after, "弹回批不得连坐运行态");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-8）字面测试名随批（kvm 段逐键有读者）
    fn dead_config_keys_are_revived_or_removed() {
        // file-core 同形制：module.rs 是声明现场，读者必须在别处（注释行不算）
        let src_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut reader_lines: Vec<Vec<String>> = Vec::new();
        for entry in walkdir::WalkDir::new(&src_root).into_iter() {
            let entry = entry.expect("walkdir 不应失败");
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            if path.file_name() == Some(std::ffi::OsStr::new("module.rs")) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            reader_lines.push(
                text.lines()
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .map(|l| l.to_owned())
                    .collect(),
            );
        }
        let key_has_reader = |key: &str| {
            reader_lines
                .iter()
                .any(|lines| lines.iter().any(|l| l.contains(key)))
        };

        let schema = KvmModule::new().config_schema();
        let props = schema["properties"].as_object().expect("schema 应为对象");
        let default_ser = serde_json::to_value(KvmConfig::default()).unwrap();
        let default_obj = default_ser.as_object().expect("KvmConfig 可序列化");

        let mut keys: Vec<&String> = props.keys().collect();
        keys.sort();
        let mut fields: Vec<&String> = default_obj.keys().collect();
        fields.sort();
        assert_eq!(keys, fields, "schema 键集必须恰等 KvmConfig 字段集");
        assert_eq!(keys, vec!["edge_map", "port"], "enabled 死键已除名");
        for key in &keys {
            assert!(
                key_has_reader(key),
                "死键：{key} 在 config_schema 声明却在 module.rs 之外零读者"
            );
            assert_eq!(
                props[*key].get("default").cloned(),
                default_obj.get(*key).cloned(),
                "schema default 与 Default 必须逐值同源: {key}"
            );
        }
    }
}
