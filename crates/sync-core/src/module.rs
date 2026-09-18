//! SyncModule（docs/impl/07 SYNC）：Module trait 实现。
//!
//! - SYNC1 拓扑：局域网 P2P（独立 TCP 监听 49820；中继 v1 不做，"仅局域网"即默认形态）
//! - SYNC2 协议：op_log 变更流（交换游标 → 拉取缺失 → 本地应用）
//! - SYNC3 冲突：LWW（engine.rs）；sync.conflict 事件通知
//! - SYNC4 加密：复用 K2 配对信任根的端到端加密通道（transport.rs）
//! - 数据集 v1 = note（订阅 notes.changed 记录本地变更；applier 由宿主注入写穿 NoteLibrary）
//! - 红线：密码库条目**永不**自动同步（数据集白名单硬编码，无 vault 通路）
//!
//! 防死环：远端应用走 NoteLibrary 直调（不发 notes.changed——该事件由 IPC 层发布），
//! 本地订阅仅记录用户操作产生的变更。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use host_core::error::ModuleError;
use host_core::events::{topic, Event, EventBus};
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};
use kvm_core::{DeviceIdentity, PairStore};

use crate::engine::{ApplyOutcome, ChangeApplier, SyncEngine};
use crate::error::SyncError;
use crate::oplog::{OpLog, DELETED_KEY};
use crate::transport::{
    handshake_client, handshake_server, read_hello_frame, read_msg, write_msg, SyncMsg, SyncSession, BATCH_LIMIT,
};

type R<T> = std::result::Result<T, SyncError>;

/// SYNC1 默认监听端口（KVM 占 49800/49801；回环测试 49810–49812 之外）
pub const DEFAULT_SYNC_PORT: u16 = 49820;

/// 数据集实体（v1 仅 note；密码库永不入白名单）
const ENTITY: &str = "note";

/// 同步结果摘要（IPC 返回 + 状态事件）
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct SyncSummary {
    pub pushed: u32,
    pub pulled_applied: u32,
    pub pulled_lost: u32,
    pub conflicts: u32,
}

/// 会话/记录上下文（Arc 化供 tokio 任务持有；Module 方法 &self 无法直接 Arc）
pub struct SyncCtx {
    pub identity: Arc<DeviceIdentity>,
    pub store: Arc<PairStore>,
    pub log: Arc<OpLog>,
    pub applier: Option<Arc<dyn ChangeApplier>>,
    pub bus: Option<Arc<EventBus>>,
}

impl SyncCtx {
    fn applier(&self) -> R<Arc<dyn ChangeApplier>> {
        self.applier
            .clone()
            .ok_or_else(|| SyncError::NotReady("变更应用器未注入".into()))
    }

    /// 发 sync.conflict 事件（LWW 通知）
    fn notify_conflict(&self, op: &crate::oplog::OpEntry) {
        let Some(bus) = &self.bus else { return };
        if let Some(t) = topic("sync.conflict") {
            let _ = bus.publish(Event::new(
                t,
                "sync",
                serde_json::json!({
                    "entity": op.entity,
                    "entity_id": op.entity_id,
                    "winner_device": op.device,
                    "ts": op.ts,
                }),
            ));
        }
    }

    fn publish_state(&self, summary: &SyncSummary) {
        let Some(bus) = &self.bus else { return };
        if let Some(t) = topic("sync.state_changed") {
            let _ = bus.publish(Event::new(t, "sync", serde_json::to_value(summary).unwrap_or_default()));
        }
    }

    /// 应用一批远端变更（统计 + 游标推进；initiator 与 responder 共用）
    fn apply_ops(
        &self,
        ops: &[crate::oplog::OpEntry],
        peer_key: &str,
    ) -> (u32, u32, u32) {
        let Ok(applier) = self.applier() else { return (0, 0, 0) };
        let mut applied = 0u32;
        let mut lost = 0u32;
        let mut conflicts = 0u32;
        let mut max_ts = 0i64;
        for op in ops {
            max_ts = max_ts.max(op.ts);
            match SyncEngine::apply_remote(&self.log, applier.as_ref(), op) {
                Ok(ApplyOutcome::Applied) => applied += 1,
                Ok(ApplyOutcome::LostLww) => {
                    lost += 1;
                    conflicts += 1;
                    self.notify_conflict(op);
                }
                Ok(ApplyOutcome::Noop) => {}
                Err(e) => tracing::warn!(op_id = %op.op_id, error = %e, "远端变更应用失败（跳过）"),
            }
        }
        if max_ts > 0 {
            let _ = self.log.set_cursor(peer_key, max_ts);
        }
        (applied, lost, conflicts)
    }
}

/// 发起方会话：Push 自产 → Ack → Pull 对端 → 应用 → Ack
async fn run_initiator(ctx: &SyncCtx, session: &mut SyncSession) -> R<SyncSummary> {
    let applier = ctx.applier()?;
    let self_device = ctx.identity.device_id.clone();
    let peer_key = session.peer.device_id.clone();
    let mut summary = SyncSummary::default();

    // ---- Push 自产变更（对端应用并自行推进 cursor）----
    let ops = ctx.log.ops_of_device(&self_device, 0, BATCH_LIMIT)?;
    summary.pushed = ops.len() as u32;
    write_msg(session, &SyncMsg::Push { ops, more: false }).await?;
    match read_msg(session).await? {
        SyncMsg::Ack { .. } => {}
        SyncMsg::Err { msg } => return Err(SyncError::Proto(msg)),
        m => return Err(SyncError::Proto(format!("Push 后非 Ack: {m:?}"))),
    }

    // ---- Pull 对端变更 ----
    let since = ctx.log.cursor(&peer_key);
    write_msg(session, &SyncMsg::Pull { since_ts: since }).await?;
    match read_msg(session).await? {
        SyncMsg::Push { ops, .. } => {
            let (applied, lost, conflicts) = ctx.apply_ops(&ops, &peer_key);
            summary.pulled_applied = applied;
            summary.pulled_lost = lost;
            summary.conflicts = conflicts;
        }
        SyncMsg::Err { msg } => return Err(SyncError::Proto(msg)),
        m => return Err(SyncError::Proto(format!("Pull 后非 Push: {m:?}"))),
    }
    let _ = applier; // apply_ops 内部再取（保持借用简单）
    ctx.publish_state(&summary);
    Ok(summary)
}

/// 响应方会话：收 Push → 应用 → Ack；收 Pull → 回自产 Push → 等 Ack
async fn run_responder(ctx: &SyncCtx, session: &mut SyncSession) -> R<SyncSummary> {
    let self_device = ctx.identity.device_id.clone();
    let peer_key = session.peer.device_id.clone();
    let mut summary = SyncSummary::default();

    match read_msg(session).await? {
        SyncMsg::Push { ops, .. } => {
            let (applied, lost, conflicts) = ctx.apply_ops(&ops, &peer_key);
            summary.pulled_applied = applied;
            summary.pulled_lost = lost;
            summary.conflicts = conflicts;
            write_msg(session, &SyncMsg::Ack { until_ts: 0, applied, lost }).await?;
        }
        SyncMsg::Err { msg } => return Err(SyncError::Proto(msg)),
        m => return Err(SyncError::Proto(format!("responder 首消息非 Push: {m:?}"))),
    }

    match read_msg(session).await? {
        SyncMsg::Pull { since_ts } => {
            // 响应方回自产变更（initiator 的游标键 = initiator 自己，即本侧视角的 peer_key）
            let ops = ctx.log.ops_of_device(&self_device, since_ts, BATCH_LIMIT)?;
            write_msg(session, &SyncMsg::Push { ops, more: false }).await?;
            let _ = read_msg(session).await?; // Ack（initiator 负责其游标）
        }
        m => return Err(SyncError::Proto(format!("responder 第二消息非 Pull: {m:?}"))),
    }
    ctx.publish_state(&summary);
    Ok(summary)
}

/// 本地变更记录（自由函数：订阅任务与 IPC 包装共用；op_log 快照式入库）
pub fn record_change_with(ctx: &SyncCtx, path: &str, action: &str) -> R<()> {
    let value = if action == "delete" {
        serde_json::json!({ DELETED_KEY: true })
    } else {
        match ctx.applier()?.snapshot(ENTITY, path)? {
            Some(v) => v,
            // 实体已不存在（rename 后旧 path 事件等）——记删除
            None => serde_json::json!({ DELETED_KEY: true }),
        }
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    SyncEngine::record_local(&ctx.log, ENTITY, path, value, &ctx.identity.device_id, now)?;
    Ok(())
}

pub struct SyncModule {
    state: AtomicU8,
    bus: RwLock<Option<Arc<EventBus>>>,
    log: RwLock<Option<Arc<OpLog>>>,
    applier: RwLock<Option<Arc<dyn ChangeApplier>>>,
    identity: RwLock<Option<Arc<DeviceIdentity>>>,
    store: Arc<PairStore>,
    port: AtomicU16,
    db_path: PathBuf,
    cancel: Arc<AtomicBool>,
}

impl SyncModule {
    pub fn new(app_data_dir: &std::path::Path) -> Self {
        // 信任根与 KVM 同目录（同机即同身份——K2 配对复用）
        let kvm_dir = app_data_dir.join("kvm");
        let store = PairStore::load_or_default(&kvm_dir).expect("PairStore 加载失败");
        Self {
            state: AtomicU8::new(0),
            bus: RwLock::new(None),
            log: RwLock::new(None),
            applier: RwLock::new(None),
            identity: RwLock::new(None),
            store: Arc::new(store),
            port: AtomicU16::new(DEFAULT_SYNC_PORT),
            db_path: app_data_dir.join("sync").join("sync.db"),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 宿主注入变更应用器（src-tauri：NoteLibrary 投影）
    pub fn attach_applier(&self, applier: Arc<dyn ChangeApplier>) {
        *self.applier.write().expect("applier 锁污染") = Some(applier);
    }

    /// 监听端口注入（测试随机端口）
    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::SeqCst);
    }

    /// 配对设备列表（SYNC 面板）
    pub fn peers(&self) -> Vec<kvm_core::PairedPeer> {
        self.store.all()
    }

    /// 运行时登记配对记录（正常流程由 KVM 配对落盘、本模块重启加载；测试/对账用）
    pub fn register_peer(&self, peer: kvm_core::PairedPeer) {
        let _ = self.store.upsert(peer);
    }

    /// op_log 状态（面板统计）
    pub fn status(&self) -> serde_json::Value {
        let count = self.log.read().ok().and_then(|g| g.clone()).map(|l| l.count()).unwrap_or(0);
        serde_json::json!({ "op_count": count, "port": self.port.load(Ordering::SeqCst) })
    }

    /// 组装会话上下文（未就绪返回 Err）
    fn ctx(&self) -> R<Arc<SyncCtx>> {
        Ok(Arc::new(SyncCtx {
            identity: self
                .identity
                .read()
                .expect("identity 锁污染")
                .clone()
                .ok_or_else(|| SyncError::NotReady("设备身份未就绪".into()))?,
            store: self.store.clone(),
            log: self
                .log
                .read()
                .expect("log 锁污染")
                .clone()
                .ok_or_else(|| SyncError::NotReady("op_log 未就绪".into()))?,
            applier: self.applier.read().expect("applier 锁污染").clone(),
            bus: self.bus.read().expect("bus 锁污染").clone(),
        }))
    }

    /// 主动与指定设备同步（IPC sync_now；addr 来自发现层/KVM 面板）
    pub async fn sync_with(&self, device_id: &str, addr: &str) -> R<SyncSummary> {
        let ctx = self.ctx()?;
        if !ctx.store.is_paired(device_id) {
            return Err(SyncError::Peer(format!("设备 {device_id} 未配对")));
        }
        let stream = tokio::time::timeout(std::time::Duration::from_secs(5), tokio::net::TcpStream::connect(addr))
            .await
            .map_err(|_| SyncError::Net("连接超时".into()))?
            .map_err(|e| SyncError::Net(e.to_string()))?;
        let mut session = handshake_client(stream, &ctx.identity, &ctx.store).await?;
        if session.peer.device_id != device_id {
            return Err(SyncError::Peer(format!(
                "对端身份不符（期望 {device_id} 实得 {}）",
                session.peer.device_id
            )));
        }
        run_initiator(&ctx, &mut session).await
    }

    /// 本地变更记录入口（notes.changed 订阅回调 / IPC）
    pub fn record_change(&self, path: &str, action: &str) -> R<()> {
        let ctx = self.ctx()?;
        record_change_with(&ctx, path, action)
    }

    /// accept 循环（start 内 tokio spawn）
    async fn accept_loop(ctx: Arc<SyncCtx>, port: u16, cancel: Arc<AtomicBool>) {
        let listener = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(port, error = %e, "SYNC 监听失败（仅可发起同步）");
                return;
            }
        };
        tracing::info!(port, "SYNC 监听就绪");
        loop {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let Ok((stream, _)) = listener.accept().await else { continue };
            let ctx = ctx.clone();
            tokio::spawn(async move {
                let mut stream = stream;
                let Ok(hello) = read_hello_frame(&mut stream).await else { return };
                let Ok(mut session) = handshake_server(stream, &ctx.identity, &ctx.store, hello).await else {
                    return;
                };
                let peer_id = session.peer.device_id.clone();
                match run_responder(&ctx, &mut session).await {
                    Ok(s) => tracing::info!(peer = %peer_id, ?s, "SYNC 入站会话完成"),
                    Err(e) => tracing::warn!(peer = %peer_id, error = %e, "SYNC 入站会话失败"),
                }
            });
        }
    }
}

impl Module for SyncModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "sync",
            name: "跨设备同步",
            version: "0.1.0",
            icon: Some("sync"),
            priority: 13,
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        // 信任根：身份（与 KVM 同文件；DPAPI 保护跟随 CryptoPort 可用性）
        let crypto = ctx.ports.get::<dyn host_core::ports::CryptoPort>();
        let identity = DeviceIdentity::load_or_create(&ctx.app_data_dir.join("kvm"), crypto)
            .map_err(|e| ModuleError::Init(e.to_string()))?;
        *self.identity.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(Arc::new(identity));
        let log = OpLog::open(&self.db_path).map_err(|e| ModuleError::Init(e.to_string()))?;
        *self.log.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(Arc::new(log));
        *self.bus.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(ctx.event_bus.clone());
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        let Ok(ctx) = self.ctx() else {
            tracing::warn!("SYNC 上下文未就绪，跳过启动");
            return Ok(());
        };
        // SYNC1：accept 循环（bind 失败仅告警——仍可主动发起同步）
        let port = self.port.load(Ordering::SeqCst);
        let cancel = self.cancel.clone();
        let ctx_listen = ctx.clone();
        tokio::spawn(async move { SyncModule::accept_loop(ctx_listen, port, cancel).await });
        // SYNC2：订阅本地变更（notes.changed → op_log 快照）
        if let Some(bus) = self.bus.read().ok().and_then(|g| g.clone()) {
            if let Ok(mut rx) = bus.subscribe("notes.changed") {
                let ctx2 = ctx.clone();
                tokio::spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(event) => {
                                if event.source == "sync" {
                                    continue; // 防自环（理论不可达：apply 直调不发事件）
                                }
                                let action =
                                    event.payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
                                if let Some(path) = event.payload.get("path").and_then(|v| v.as_str()) {
                                    if matches!(action, "create" | "write" | "delete") {
                                        if let Err(e) = record_change_with(&ctx2, path, action) {
                                            tracing::warn!(path, error = %e, "本地变更入 op_log 失败");
                                        }
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(_) => break,
                        }
                    }
                });
            }
        }
        self.state.store(2, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        self.cancel.store(true, Ordering::SeqCst);
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "scope_note": {
                    "type": "string", "title": "同步范围",
                    "description": "v1 仅笔记库；密码库永不自动同步（仅手动导出加密包）",
                    "default": ""
                }
            }
        })
    }

    fn apply_config(&self, _values: serde_json::Value) -> Result<(), ModuleError> {
        Ok(())
    }

    fn status(&self) -> ModuleState {
        match self.state.load(Ordering::SeqCst) {
            0 => ModuleState::Uninitialized,
            1 => ModuleState::Stopped,
            _ => ModuleState::Running,
        }
    }
}
