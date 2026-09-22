//! SyncModule（docs/impl/07 SYNC）：Module trait 实现。
//!
//! - SYNC1 拓扑：局域网 P2P（独立 TCP 监听 49820；中继 v1 不做，"仅局域网"即默认形态）
//! - SYNC2 协议：op_log 变更流（交换游标 → 拉取缺失 → 本地应用）
//! - SYNC3 冲突：LWW（engine.rs）；败方快照落 `conflict_log` 可查可恢复（T-B5-2），
//!   sync.conflict 事件只作提示（事件即焚不是数据源）
//! - SYNC4 加密：复用 K2 配对信任根的端到端加密通道（transport.rs）
//! - 会话流水：每轮同步（含失败）落一行 `sync_run`（T-B5-3：摘要过去只活在 tracing 里，
//!   阅后即焚 ⇒ 面板答不出"上次到底同步了没"）
//! - 状态读面：`status()` 返回类型化 `SyncStatus`（T-B5-4：监听真态 + 每 peer 两维游标与
//!   pending，全部现读自表；无类型 `json!` 时代"面板说的"与"内核做的"可以各说各话）
//! - 数据集注册表（T-B5-5）：`SYNC_ENTITIES` 编译期白名单 + 按 entity 分派的应用器映射；
//!   新增数据集＝往常量数组加一行 + 宿主多装配一个 applier，`engine.rs`/`transport.rs` 零改
//! - 红线：密码库条目**永不**自动同步（白名单硬编码，`attach_applier("vault", …)` 运行期亦拒，
//!   见 `syncEntity_vaultNeverAdmitted` 三层可否例）
//!
//! 防死环：远端应用走 NoteLibrary 直调（不发 notes.changed——该事件由 IPC 层发布），
//! 本地订阅仅记录用户操作产生的变更。

use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;

use host_core::device::{DeviceIdentity, PairStore, PairedPeer};
use host_core::error::ModuleError;
use host_core::events::{topic, Event, EventBus};
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};

use crate::engine::{ApplyOutcome, ChangeApplier, PushBatch, SyncEngine};
use crate::error::SyncError;
use crate::oplog::{ConflictEntry, OpLog, SyncRun, DELETED_KEY, ROLE_INITIATOR, ROLE_RESPONDER};
use crate::transport::{
    handshake_client, handshake_server, read_hello_frame, read_msg, write_msg, SyncMsg,
    SyncSession, BATCH_LIMIT,
};

type R<T> = std::result::Result<T, SyncError>;

/// SYNC1 默认监听端口（KVM 占 49800/49801；回环测试 49810–49812 之外）
pub const DEFAULT_SYNC_PORT: u16 = 49820;

/// 笔记库数据集 id
pub const ENTITY_NOTE: &str = "note";

/// 可同步数据集声明（14-sync §5-1 的落地形态）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntitySpec {
    pub id: &'static str,
    pub label: &'static str,
}

/// **编译期白名单**：同步数据集的唯一准入名单。
///
/// "密码库永不自动同步"从文案升格为结构事实：这里没有 `vault` 条目，
/// `attach_applier` 因此拒绝任何 `vault` 应用器入注册表（运行期第二层），
/// 而线格式/存储层从来不带数据集枚举（`op_log.entity` 只是文本列）。
/// 新增一个数据集 = 此处加一行 + 宿主装配一个 applier；
/// `engine.rs` / `transport.rs` / `oplog.rs` 零改动（`syncRegistry_secondEntityNeedsNoEngineChange` 钉住）。
pub const SYNC_ENTITIES: &[EntitySpec] = &[EntitySpec {
    id: ENTITY_NOTE,
    label: "笔记库",
}];

/// 白名单查表（准入判定唯一入口：调用方不得再自拼 `entity == "note"` 字面量）
pub fn is_sync_entity(entity: &str) -> bool {
    SYNC_ENTITIES.iter().any(|s| s.id == entity)
}

/// 白名单成员 id 列表（错误消息与面板点名用）
pub fn sync_entity_ids() -> Vec<&'static str> {
    SYNC_ENTITIES.iter().map(|s| s.id).collect()
}

/// 准入守卫：两道**门**（宿主装配 applier、事件层认领数据集）过这一条查表。
///
/// 会话内部的分派（`SyncCtx::applier_for`）刻意不看白名单、只看注册表：
/// 白名单是"谁被允许进门"，注册表是"此刻谁在服"。分清两层，
/// `syncRegistry_secondEntityNeedsNoEngineChange` 才能用注册表接缝装上假数据集，
/// 证明真加一个数据集只需要往 `SYNC_ENTITIES` 加一行，引擎与线格式零改。
pub fn require_sync_entity(entity: &str) -> R<()> {
    if is_sync_entity(entity) {
        return Ok(());
    }
    Err(SyncError::Entity(format!(
        "数据集 {entity} 不在同步白名单（当前仅 {:?}）：拒绝装配/拒绝认领",
        sync_entity_ids()
    )))
}

/// 内容变更动作白名单：`create`/`write`/`delete` 是终态快照，`rename` 展开为
/// "旧路径删除 + 新路径写入"两笔（T-B5-5：过去 rename 完全不入流 ⇒ 对端既留旧文件
/// 又拿不到新文件，等于一次改名在远端变成一次凭空新增）。
/// 白名单外的动作（`sync`/`reindex`/`cards`/`canvas` 等索引与视图事件）零入流。
pub const CHANGE_ACTIONS: &[&str] = &["create", "write", "delete", "rename"];

/// 一条应入 op_log 的变更记录（事件 → 变更流的中间产物，纯函数出口）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeRecord {
    pub entity: String,
    pub path: String,
    pub action: String,
}

/// 同步结果摘要（IPC 返回 + 状态事件）
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct SyncSummary {
    pub pushed: u32,
    pub pulled_applied: u32,
    pub pulled_lost: u32,
    pub conflicts: u32,
}

/// 单个配对设备在本机的同步进度（T-B5-4：面板"这台落后多少 / 上次到底成没成"的读面）
///
/// 两半制：三个数字全部现读自 op_log 三表（`cursors` / `push_cursors` / `sync_run`），
/// 本结构不另立事实源、不缓存 ⇒ 协议跑的与面板看的同一份数。
/// `sync_addr` / `online` 随形状先落位而**恒为 None/false**：其真值分属 T-B5-7（心跳宣告
/// sync 端口 + 地址解析），今天没有任何事实源，前端因此也不据这两列渲染任何东西——
/// 无事实源就无文案，宁可空着也不写"离线"（那同样是断言）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct PeerStatus {
    pub device_id: String,
    pub device_name: String,
    pub fingerprint: String,
    /// 该对端推到我这边的进度（入站游标）
    pub inbound_cursor: i64,
    /// 我把自产变更推到那台的进度（出站游标）
    pub push_cursor: i64,
    /// 本机自产且尚未推给这台的条数（pending 唯一算式：`count_ops_after(本机, 出站游标)`）
    pub pending_ops: u64,
    /// 与这台最近一轮会话的**开始**时刻（0 = 本机从没跟它同步过）
    pub last_sync_ms: i64,
    /// 最近一轮的失败原因（None = 那轮成功；与 `sync_run.error` 逐字相同）
    pub last_error: Option<String>,
    /// 对端 sync 地址（T-B5-7 前恒 None）
    pub sync_addr: Option<String>,
    /// 发现层在线（T-B5-7 前恒 false，且不进任何 UI 文案）
    pub online: bool,
}

/// 同步模块状态（T-B5-4 起**类型化**：过去是 `serde_json::Value` 二键，
/// 命令层加键、前端少读，两边静默漂移无人报警——类型是这件事唯一的编译器级防线）
#[derive(Clone, Debug, serde::Serialize)]
pub struct SyncStatus {
    pub op_count: u64,
    pub port: u16,
    /// 监听真态：只有 `accept_loop` 真的 bind 成功才 true。
    /// 过去 `start()` 无条件置 Running、面板直接渲染 `监听 :端口` ⇒ 端口被占时仍报"在听"。
    pub listening: bool,
    /// 最近一次 bind 失败的原因（成功或还没试过则 None；面板据此出红条）
    pub last_bind_error: Option<String>,
    pub self_device_id: String,
    pub self_name: String,
    /// 手动暂停同步（T-B5-6 落地前恒 false）
    pub paused: bool,
    pub peers: Vec<PeerStatus>,
}

/// 会话/记录上下文（Arc 化供 tokio 任务持有；Module 方法 &self 无法直接 Arc）
pub struct SyncCtx {
    pub identity: Arc<DeviceIdentity>,
    pub store: Arc<PairStore>,
    pub log: Arc<OpLog>,
    /// entity → 应用器（T-B5-5：过去是单槽 `Option<Arc<dyn ChangeApplier>>`，
    /// 于是"泛化"在结构上无处落脚——第二实体没有地方注册，未知 entity 只能被
    /// 唯一的 note 应用器收下或整批静默计数 0）。
    ///
    /// 两层防线各自独立：**准入**在 `attach_applier`（查 `SYNC_ENTITIES` 编译期白名单），
    /// **分派**在此处查表（谁注册过就谁来收）。测试因此能装一个白名单外的假数据集
    /// 验证"第二实体不需要动引擎"，而生产路径没有任何口子把 `vault` 塞进这张表。
    pub appliers: Arc<HashMap<String, Arc<dyn ChangeApplier>>>,
    pub bus: Option<Arc<EventBus>>,
}

impl SyncCtx {
    /// 按 entity 取应用器（**分派**口：只看注册表，白名单守卫在两道门上，见 `require_sync_entity`）
    fn applier_for(&self, entity: &str) -> R<Arc<dyn ChangeApplier>> {
        self.appliers.get(entity).cloned().ok_or_else(|| {
            SyncError::Entity(format!(
                "数据集 {entity} 无应用器（白名单 {:?}；密码库等未注册数据集在此永久拒收）",
                sync_entity_ids()
            ))
        })
    }

    /// 会话准入：至少有一个数据集在服（`run_initiator` 开会话前的粗门）
    fn require_appliers(&self) -> R<()> {
        if self.appliers.is_empty() {
            return Err(SyncError::NotReady("变更应用器未注入".into()));
        }
        Ok(())
    }

    /// 发 sync.conflict 事件（LWW 通知）
    ///
    /// `conflict_id` 供视图按 id 回查落盘行——事件本身不再是唯一事实源（承重⑥根因）。
    /// 载荷刻意不带 `lost_value`：内容只经 `sync_conflicts_get` 按需读取，不广播进每个窗口。
    fn notify_conflict(&self, op: &crate::oplog::OpEntry, conflict_id: &str) {
        let Some(bus) = &self.bus else { return };
        if let Some(t) = topic("sync.conflict") {
            let _ = bus.publish(Event::new(
                t,
                "sync",
                serde_json::json!({
                    "conflict_id": conflict_id,
                    "entity": op.entity,
                    "entity_id": op.entity_id,
                    "loser_device": op.device,
                    "ts": op.ts,
                }),
            ));
        }
    }

    fn publish_state(&self, summary: &SyncSummary) {
        let Some(bus) = &self.bus else { return };
        if let Some(t) = topic("sync.state_changed") {
            let _ = bus.publish(Event::new(
                t,
                "sync",
                serde_json::to_value(summary).unwrap_or_default(),
            ));
        }
    }

    /// 应用一批远端变更（统计 + 游标推进 + 冲突落盘；initiator 与 responder 共用）
    ///
    /// **可失败**是 T-B5-5 的红线收口：过去应用器不在位/未知 entity 走 `(0,0,0)` 静默返回，
    /// 而游标照样 `set_cursor` 前进 ⇒ 对端那批变更在本机既没落地又被记成"已同步"，
    /// 是"谎报进度"最便宜的一种写法。现在整会话 Err，游标停在原地，下轮重来。
    fn apply_ops(&self, ops: &[crate::oplog::OpEntry], peer_key: &str) -> R<(u32, u32, u32)> {
        let mut applied = 0u32;
        let mut lost = 0u32;
        let mut conflicts = 0u32;
        let mut max_ts = 0i64;
        for op in ops {
            // 每条 op 按自己的 entity 取应用器：没有应用器 ⇒ 拒收到此为止（不跳过后继续）
            let applier = self.applier_for(&op.entity)?;
            max_ts = max_ts.max(op.ts);
            match SyncEngine::apply_remote(&self.log, applier.as_ref(), op) {
                Ok(ApplyOutcome::Applied) => applied += 1,
                Ok(ApplyOutcome::LostLww) => {
                    lost += 1;
                    conflicts += 1;
                    // 落盘先于事件：这一刻之前败方原文只活在这条待广播的 op 里（承重⑤）
                    let winner = self
                        .log
                        .latest_for(&op.entity, &op.entity_id)
                        .ok()
                        .flatten();
                    let entry = SyncEngine::conflict_of(op, winner.as_ref(), now_ms());
                    if let Err(e) = self.log.record_conflict(&entry) {
                        tracing::warn!(
                            conflict_id = %entry.conflict_id,
                            error = %e,
                            "冲突快照落盘失败（事件仍发，冲突视图会缺这一行）"
                        );
                    }
                    self.notify_conflict(op, &entry.conflict_id);
                }
                Ok(ApplyOutcome::Noop) => {}
                Err(e) => tracing::warn!(op_id = %op.op_id, error = %e, "远端变更应用失败（跳过）"),
            }
        }
        if max_ts > 0 {
            self.log.set_cursor(peer_key, max_ts)?;
        }
        Ok((applied, lost, conflicts))
    }
}

/// 发起方会话：按出站游标分批 Push（逐批等 Ack 对账）→ Pull 对端 → 应用 → 回执
///
/// 计数累加进调用方给的 `summary`（失败前已完成的部分也在里面），落流水由
/// `sync_with` 单点负责——会话函数本身不记账，才不会出现"两条错误路径只记了一条"。
async fn run_initiator(
    ctx: &SyncCtx,
    session: &mut SyncSession,
    summary: &mut SyncSummary,
) -> R<()> {
    ctx.require_appliers()?; // 应用器一个都不在位就别开会话（分派失败是逐条 Err，此处是配置期粗门）
    let self_device = ctx.identity.device_id.clone();
    let peer_key = session.peer.device_id.clone();

    // ---- Push 自产变更（出站游标驱动批循环；对端回执 until_ts 供对账）----
    let mut cursor = ctx.log.push_cursor(&peer_key);
    loop {
        let raw = ctx.log.ops_of_device(&self_device, cursor, BATCH_LIMIT)?;
        let PushBatch { ops, more } = SyncEngine::plan_push(raw, BATCH_LIMIT);
        summary.pushed += ops.len() as u32;
        write_msg(session, &SyncMsg::Push { ops, more }).await?;
        let until_ts = match read_msg(session).await? {
            SyncMsg::Ack { until_ts, .. } => until_ts,
            SyncMsg::Err { msg } => return Err(SyncError::Proto(msg)),
            m => return Err(SyncError::Proto(format!("Push 后非 Ack: {m:?}"))),
        };
        ctx.log.set_push_cursor(&peer_key, until_ts)?;
        // 对端游标未前进（旧版本恒回 0 / 空批回执）即停：宁可少推一轮，不可原地重放空转
        if until_ts <= cursor || !more {
            break;
        }
        cursor = ctx.log.push_cursor(&peer_key);
    }

    // ---- Pull 对端变更 ----
    let since = ctx.log.cursor(&peer_key);
    write_msg(session, &SyncMsg::Pull { since_ts: since }).await?;
    match read_msg(session).await? {
        SyncMsg::Push { ops, .. } => {
            let (applied, lost, conflicts) = ctx.apply_ops(&ops, &peer_key)?;
            summary.pulled_applied += applied;
            summary.pulled_lost += lost;
            summary.conflicts += conflicts;
            // 回执对称（T-B5-1 落地补记④ 的修正处）：Push→Ack 是全协议唯一的回执形状，
            // 发起方过去只收不回 ⇒ 响应方那句"等 Ack"恒以 EOF 报错，入站会话在流水里
            // 永远记成失败。两向都发回执，会话才能双向干净收口。
            // Pull 的应答侧 `more` 恒 false（单批应答，余量下一轮续），故此处无批循环。
            let until_ts = ops.iter().map(|o| o.ts).max().unwrap_or(0);
            write_msg(
                session,
                &SyncMsg::Ack {
                    until_ts,
                    applied,
                    lost,
                },
            )
            .await?;
        }
        SyncMsg::Err { msg } => return Err(SyncError::Proto(msg)),
        m => return Err(SyncError::Proto(format!("Pull 后非 Push: {m:?}"))),
    }
    Ok(())
}

/// 响应方会话：收 Push（按 more 分批）→ 应用 → 逐批回 Ack（真 until_ts）；收 Pull → 回自产 Push → 等 Ack
async fn run_responder(
    ctx: &SyncCtx,
    session: &mut SyncSession,
    summary: &mut SyncSummary,
) -> R<()> {
    let self_device = ctx.identity.device_id.clone();
    let peer_key = session.peer.device_id.clone();

    // 收 Push 批循环（与发起方分批对称：more=false 才进入 Pull 阶段）
    loop {
        match read_msg(session).await? {
            SyncMsg::Push { ops, more } => {
                let (applied, lost, conflicts) = ctx.apply_ops(&ops, &peer_key)?;
                summary.pulled_applied += applied;
                summary.pulled_lost += lost;
                summary.conflicts += conflicts;
                // 本批已处理到的最大 ts（ops 按 ts 升序）——发起方据此推进出站游标
                let until_ts = ops.iter().map(|o| o.ts).max().unwrap_or(0);
                write_msg(
                    session,
                    &SyncMsg::Ack {
                        until_ts,
                        applied,
                        lost,
                    },
                )
                .await?;
                if !more {
                    break;
                }
            }
            SyncMsg::Err { msg } => return Err(SyncError::Proto(msg)),
            m => return Err(SyncError::Proto(format!("responder 首消息非 Push: {m:?}"))),
        }
    }

    match read_msg(session).await? {
        SyncMsg::Pull { since_ts } => {
            // 响应方回自产变更（initiator 的游标键 = initiator 自己，即本侧视角的 peer_key）
            let ops = ctx.log.ops_of_device(&self_device, since_ts, BATCH_LIMIT)?;
            // 这一向同样是推送：不记进 pushed 的话，被动侧流水会写出"推了 0 条"的假话
            summary.pushed += ops.len() as u32;
            write_msg(session, &SyncMsg::Push { ops, more: false }).await?;
            // 等发起方回执，并据此记"我给这台推到哪"（每 peer 进度事实源的另一半：
            // 被动供数也要推进出站游标，否则本机视角的 pending 会永久虚高）
            match read_msg(session).await? {
                SyncMsg::Ack { until_ts, .. } => ctx.log.set_push_cursor(&peer_key, until_ts)?,
                SyncMsg::Err { msg } => return Err(SyncError::Proto(msg)),
                m => return Err(SyncError::Proto(format!("Pull 应答后非 Ack: {m:?}"))),
            }
        }
        m => {
            return Err(SyncError::Proto(format!(
                "responder 第二消息非 Pull: {m:?}"
            )))
        }
    }
    Ok(())
}

/// 一轮同步尝试落一行流水（**失败也记**：红线"失败不静默"，`error` 列存展示文本）。
///
/// 两角色共用这一个记账口；会话函数只往 `summary` 里累加计数，于是半途失败时
/// "已经推出去多少条"仍是真事实，不会被抹成 0 也不会虚报成整轮。
/// 落盘本身失败只 warn：流水缺行是可观测性问题，不该把一次成功的同步改判成失败。
fn finish_run(
    ctx: &SyncCtx,
    peer: &str,
    role: &str,
    started_ms: i64,
    summary: &SyncSummary,
    error: Option<&SyncError>,
) {
    let run = SyncRun {
        id: 0, // 自增主键由 SQLite 分配
        ts_ms: started_ms,
        peer: peer.to_string(),
        role: role.to_string(),
        pushed: summary.pushed,
        pulled_applied: summary.pulled_applied,
        pulled_lost: summary.pulled_lost,
        conflicts: summary.conflicts,
        // 时钟回跳（NTP 校正）不写负数：宁可报 0，也不给面板一个不可能的耗时
        duration_ms: (now_ms() - started_ms).max(0),
        error: error.map(|e| e.to_string()),
    };
    if let Err(e) = ctx.log.record_run(&run) {
        tracing::warn!(peer = %peer, error = %e, "同步流水落盘失败（本轮结果仍如实返回）");
    }
}

/// 发起一侧：配对校验 → 连接 → 握手 → 身份核对 → 跑协议（计时与记账在 `sync_with`）
async fn initiate(ctx: &SyncCtx, device_id: &str, addr: &str, summary: &mut SyncSummary) -> R<()> {
    if !ctx.store.is_paired(device_id) {
        return Err(SyncError::Peer(format!("设备 {device_id} 未配对")));
    }
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(addr),
    )
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
    run_initiator(ctx, &mut session, summary).await
}

/// 被动一侧：读 Hello → 握手 → 跑协议。交回 `(流水里的 peer 标签, 会话结果)`。
///
/// peer 标签：握手成功后是对端 device_id，之前失败只能如实记 socket 地址——
/// 身份还没验证，拿它当 device_id 写进流水就是把猜测记成事实。
async fn respond(
    ctx: &SyncCtx,
    stream: tokio::net::TcpStream,
    peer_addr: &str,
    summary: &mut SyncSummary,
) -> (String, R<()>) {
    let mut stream = stream;
    let hello = match read_hello_frame(&mut stream).await {
        Ok(h) => h,
        Err(e) => return (peer_addr.to_string(), Err(e)),
    };
    let mut session = match handshake_server(stream, &ctx.identity, &ctx.store, hello).await {
        Ok(s) => s,
        Err(e) => return (peer_addr.to_string(), Err(e)),
    };
    let label = session.peer.device_id.clone();
    (label, run_responder(ctx, &mut session, summary).await)
}

/// 当前毫秒（op ts 与冲突落盘时刻共用一个时钟口）
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 事件 → 应入流的变更记录（**纯函数**，§9.1-④ 两半制的语义半：零 IO 可直测）
///
/// `None` = 该事件不是内容变更：动作在 `CHANGE_ACTIONS` 外（索引/卡片/画布等视图事件）、
/// 缺 `path`、或 `rename` 缺 `old_path`（少一半就无法两记齐全，宁可不记）。
/// 数据集认领：`payload["entity"]` 缺省时回落 `ENTITY_NOTE`——notes 侧事件早于本行存在，
/// 不能要求它先升级；回落这件事由 `record_change_event` 一次性 warn，不静默。
pub fn change_records_of_event(event: &Event) -> Option<Vec<ChangeRecord>> {
    let action = event.payload.get("action").and_then(|v| v.as_str())?;
    if !CHANGE_ACTIONS.contains(&action) {
        return None;
    }
    let path = event.payload.get("path").and_then(|v| v.as_str())?;
    let entity = event
        .payload
        .get("entity")
        .and_then(|v| v.as_str())
        .unwrap_or(ENTITY_NOTE);
    if action == "rename" {
        let old_path = event.payload.get("old_path").and_then(|v| v.as_str())?;
        // 顺序即语义：先旧路径删除、后新路径写入。两笔通常同毫秒 ⇒ 先后由 op_log 的
        // `ORDER BY ts, rowid` 定（rowid 随写入递增），这也是本函数产出有序 Vec 的原因。
        return Some(vec![
            ChangeRecord {
                entity: entity.to_string(),
                path: old_path.to_string(),
                action: "delete".into(),
            },
            ChangeRecord {
                entity: entity.to_string(),
                path: path.to_string(),
                action: "write".into(),
            },
        ]);
    }
    Some(vec![ChangeRecord {
        entity: entity.to_string(),
        path: path.to_string(),
        action: action.to_string(),
    }])
}

/// 事件缺 `entity` 键的回落只提示一次（每次进程启动一条 warn 足够；刷屏会淹掉真信号）
static ENTITY_KEY_WARNED: AtomicBool = AtomicBool::new(false);

/// 事件入流（订阅臂唯一入口）：白名单认领 → 解析 → 逐条入库
///
/// 返回入库条数（`Ok(0)` = 非内容事件，正常静默；`Err` = 该记的没记上，调用方必须出声）。
/// rename 的中间态是这里唯一的多写：第一笔已入库而第二笔失败 ⇒ 远端会"只删不建"，
/// 所以失败绝不能吞成 Ok(0)——订阅臂把它记成 warn 是最后一层，测试面直接看 Err。
pub fn record_change_event(ctx: &SyncCtx, event: &Event) -> R<usize> {
    if let Some(entity) = event.payload.get("entity").and_then(|v| v.as_str()) {
        require_sync_entity(entity)?; // 事件自己声称的数据集也要过白名单这道门
    } else if event.payload.get("path").is_some() && !ENTITY_KEY_WARNED.swap(true, Ordering::SeqCst)
    {
        tracing::warn!(
            "变更事件缺 entity 键，按 {ENTITY_NOTE} 处理（发布方应显式带 entity；本提示只出一次）"
        );
    }
    let Some(records) = change_records_of_event(event) else {
        return Ok(0);
    };
    for r in &records {
        record_change_with(ctx, &r.entity, &r.path, &r.action)?;
    }
    Ok(records.len())
}

/// 本地变更记录（自由函数：订阅任务与 IPC 包装共用；op_log 快照式入库）
///
/// `action == "delete"` 是**唯一**真删除口。其余动作读当前快照入库，读不到就 Err：
/// 旧语义把 `snapshot()` 的 `None` 直接记成 `{deleted:true}`（承重⑪红线）——
/// 于是任何 IO 抖动（文件被占用、路径基准差异、泛化后第二实体 id 拼错）都会
/// **向全部对端广播"删了"**，一次读失败放大成跨设备删除。宁可这一笔不入流、
/// 下轮用户再改一次，也不替用户做删除的决定。
pub fn record_change_with(ctx: &SyncCtx, entity: &str, path: &str, action: &str) -> R<()> {
    let applier = ctx.applier_for(entity)?;
    let value = if action == "delete" {
        serde_json::json!({ DELETED_KEY: true })
    } else {
        match applier.snapshot(entity, path)? {
            Some(v) => v,
            None => {
                return Err(SyncError::Entity(format!(
                    "快照读取失败：实体 {entity}/{path} 不存在，已拒记删除标记（action={action}）"
                )))
            }
        }
    };
    SyncEngine::record_local(
        &ctx.log,
        entity,
        path,
        value,
        &ctx.identity.device_id,
        now_ms(),
    )?;
    Ok(())
}

/// 以本地留存的败方快照重新生效（IPC sync_conflict_restore 的同形入口）
///
/// 两步缺一不可：① 写穿数据集——只往 op_log 塞新条目会让本机自己不一致
/// （日志说最新值是快照，磁盘上还是胜者那份，对端反向拉取时也拿不到恢复结果）；
/// ② `record_local` 以新 ts 入流 ⇒ 下轮同步覆盖对端。
///
/// 文案红线：一律称"以本地副本重新生效并推送"，**不称**"撤销对端/强制回滚"——
/// 本机无法保证对端在此之后不再修改，声称能撤销就是假保证。
pub fn restore_conflict_with(ctx: &SyncCtx, conflict_id: &str) -> R<crate::oplog::OpEntry> {
    let entry = ctx.log.find_conflict(conflict_id)?.ok_or_else(|| {
        SyncError::Apply(format!("冲突记录 {conflict_id} 不存在（可能已超出保留窗）"))
    })?;
    let applier = ctx.applier_for(&entry.entity)?;
    if entry.is_delete() {
        applier.apply_delete(&entry.entity, &entry.entity_id)?;
    } else {
        applier.apply_upsert(&entry.entity, &entry.entity_id, &entry.lost_value)?;
    }
    SyncEngine::record_local(
        &ctx.log,
        &entry.entity,
        &entry.entity_id,
        entry.lost_value.clone(),
        &ctx.identity.device_id,
        now_ms(),
    )
}

pub struct SyncModule {
    state: ModuleStateCell,
    bus: RwLock<Option<Arc<EventBus>>>,
    log: RwLock<Option<Arc<OpLog>>>,
    /// entity → 应用器注册表（`attach_applier` 是唯一写口，且过白名单这道门）
    appliers: RwLock<HashMap<String, Arc<dyn ChangeApplier>>>,
    identity: RwLock<Option<Arc<DeviceIdentity>>>,
    store: Arc<PairStore>,
    port: AtomicU16,
    /// 监听真态（T-B5-4）：只由 `accept_loop` 的 bind 结果写，不由 `start()` 许愿写
    listening: Arc<AtomicBool>,
    /// 最近一次 bind 失败文本（与 `listening` 同属一对事实：true 时清空，失败时留存）
    bind_error: Arc<RwLock<Option<String>>>,
    db_path: PathBuf,
    cancel: Arc<AtomicBool>,
}

impl SyncModule {
    pub fn new(app_data_dir: &std::path::Path) -> Self {
        // 信任根与 KVM 同目录（同机即同身份——K2 配对复用）
        let kvm_dir = app_data_dir.join("kvm");
        let store = PairStore::load_or_default(&kvm_dir).expect("PairStore 加载失败");
        Self {
            state: ModuleStateCell::new(),
            bus: RwLock::new(None),
            log: RwLock::new(None),
            appliers: RwLock::new(HashMap::new()),
            identity: RwLock::new(None),
            store: Arc::new(store),
            port: AtomicU16::new(DEFAULT_SYNC_PORT),
            listening: Arc::new(AtomicBool::new(false)),
            bind_error: Arc::new(RwLock::new(None)),
            db_path: app_data_dir.join("db").join("sync.db"),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 宿主注入某数据集的变更应用器（**双参**：entity 必填）
    ///
    /// 白名单外的 entity（含 `"vault"`）一律拒绝并 `tracing::error!`：这一行是
    /// "密码库永不自动同步"从注释升格为代码的地方——过去想接 vault 只需要
    /// `attach_applier(vaultApplier)` 一行，注册表连个说"不"的地方都没有。
    pub fn attach_applier(&self, entity: &str, applier: Arc<dyn ChangeApplier>) -> R<()> {
        if let Err(e) = require_sync_entity(entity) {
            tracing::error!(entity, error = %e, "拒绝装配非白名单数据集的应用器");
            return Err(e);
        }
        self.appliers.write().insert(entity.to_string(), applier);
        Ok(())
    }

    /// 监听端口注入（测试随机端口）
    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::SeqCst);
    }

    /// D-12：本模块 SQLite 库路径（宿主 O3 归位断言用）
    pub fn db_path(&self) -> &std::path::Path {
        &self.db_path
    }

    /// D-12 一次性归位：旧路径 {appData}/sync/sync.db → {appData}/db/sync.db。
    /// 仅当新路径不存在且旧文件存在时搬移（含 WAL/SHM 伴生文件）；无正式发布版本，
    /// 搬移后旧条件永假，幂等。
    fn migrate_legacy_db_path(&self) {
        let Some(new_parent) = self.db_path.parent() else {
            return;
        };
        let Some(app_data) = new_parent.parent() else {
            return;
        };
        let legacy = app_data.join("sync").join("sync.db");
        if !legacy.exists() || self.db_path.exists() {
            return;
        }
        if let Err(e) = std::fs::create_dir_all(new_parent) {
            tracing::warn!(error = %e, "db 目录创建失败，sync.db 归位跳过");
            return;
        }
        for suffix in ["", "-wal", "-shm"] {
            let src = PathBuf::from(format!("{}{}", legacy.display(), suffix));
            if !src.exists() {
                continue;
            }
            let dst = PathBuf::from(format!("{}{}", self.db_path.display(), suffix));
            if let Err(e) = std::fs::rename(&src, &dst) {
                tracing::warn!(error = %e, "sync.db 归位搬移失败");
                return;
            }
        }
        // 旧目录此时应已为空；非空（历史杂项）则保留不误删
        let _ = std::fs::remove_dir(app_data.join("sync"));
        tracing::info!(
            "sync.db 已从 {}/sync 归位到 db/（D-12）",
            app_data.display()
        );
    }

    /// 配对设备列表（SYNC 面板）
    pub fn peers(&self) -> Vec<PairedPeer> {
        self.store.all()
    }

    /// 运行时登记配对记录（正常流程由 KVM 配对落盘、本模块重启加载；测试/对账用）
    pub fn register_peer(&self, peer: PairedPeer) {
        let _ = self.store.upsert(peer);
    }

    /// 同步模块状态快照（面板唯一读面）
    ///
    /// **可失败**是刻意的：本方法背后是四次真查询（`op_log` 计数 + 入站/出站游标 +
    /// 每 peer 最近流水）。若签名收成无 `Result`，查询失败就只能落成"0 条待同步"——
    /// 那正是本行要消灭的"面板谎报"的另一张脸。未就绪（init 前）同理如实报错。
    ///
    /// 游标与 pending 一律现读自表：这里不留任何缓存副本，协议推进游标后面板不必
    /// 等事件也知道变了（单一事实源；两处各写迟早漂移，漂移就是谎报）。
    pub fn status(&self) -> R<SyncStatus> {
        let log = self.log_arc()?;
        let identity = self
            .identity
            .read()
            .clone()
            .ok_or_else(|| SyncError::NotReady("设备身份未就绪".into()))?;
        let inbound: HashMap<String, i64> = log.all_cursors()?.into_iter().collect();
        let outbound: HashMap<String, i64> = log.all_push_cursors()?.into_iter().collect();
        let last_runs = log.last_run_per_peer()?;
        let self_device = identity.device_id.clone();
        let mut peers = Vec::new();
        for p in self.store.all() {
            let push_ts = outbound.get(&p.device_id).copied().unwrap_or(0);
            // 一次查询失败就整面失败：游标读不到时"pending=0"会被面板读成"都同步过了"
            let pending_ops = log.count_ops_after(&self_device, push_ts)?;
            let last = last_runs.get(&p.device_id);
            peers.push(PeerStatus {
                fingerprint: p.fingerprint,
                device_id: p.device_id.clone(),
                device_name: p.device_name.clone(),
                inbound_cursor: inbound.get(&p.device_id).copied().unwrap_or(0),
                push_cursor: push_ts,
                pending_ops,
                last_sync_ms: last.map(|r| r.ts_ms).unwrap_or(0),
                last_error: last.and_then(|r| r.error.clone()),
                // T-B5-7 前无事实源：发现层尚未宣告对端 sync 端口。
                // 留 None/false 且不进任何 UI 文案——"没查过"与"查了说离线"是两件事。
                sync_addr: None,
                online: false,
            });
        }
        Ok(SyncStatus {
            op_count: log.count(),
            port: self.port.load(Ordering::SeqCst),
            listening: self.listening.load(Ordering::SeqCst),
            last_bind_error: self.bind_error.read().clone(),
            self_device_id: identity.device_id.clone(),
            self_name: identity.device_name.clone(),
            // T-B5-6（自动同步 + 暂停/恢复）落地前恒 false：没有暂停开关就没有暂停态
            paused: false,
            peers,
        })
    }

    fn log_arc(&self) -> R<Arc<OpLog>> {
        self.log
            .read()
            .clone()
            .ok_or_else(|| SyncError::NotReady("op_log 未就绪".into()))
    }

    /// 冲突历史分页（IPC sync_conflicts_get；limit/offset 由读侧收口，表可无限长但视图不跟着涨）
    pub fn conflicts(&self, limit: i64, offset: i64) -> R<Vec<ConflictEntry>> {
        self.log_arc()?.conflicts(limit, offset)
    }

    /// 冲突历史保留窗裁剪（IPC/配置消费方：T-B5-6 的 conflict_keep_days 真消费点）
    pub fn prune_conflicts_before(&self, cutoff_ms: i64) -> R<u64> {
        self.log_arc()?.prune_conflicts_before(cutoff_ms)
    }

    /// 同步流水分页（IPC sync_runs_get；新行在前，limit 由读侧收口）
    pub fn runs(&self, limit: i64) -> R<Vec<SyncRun>> {
        self.log_arc()?.runs(limit)
    }

    /// 以本地留存的败方快照重新生效（IPC sync_conflict_restore）
    pub fn restore_conflict(&self, conflict_id: &str) -> R<crate::oplog::OpEntry> {
        let ctx = self.ctx()?;
        restore_conflict_with(&ctx, conflict_id)
    }

    /// 组装会话上下文（未就绪返回 Err）
    fn ctx(&self) -> R<Arc<SyncCtx>> {
        Ok(Arc::new(SyncCtx {
            identity: self
                .identity
                .read()
                .clone()
                .ok_or_else(|| SyncError::NotReady("设备身份未就绪".into()))?,
            store: self.store.clone(),
            log: self
                .log
                .read()
                .clone()
                .ok_or_else(|| SyncError::NotReady("op_log 未就绪".into()))?,
            appliers: Arc::new(self.appliers.read().clone()),
            bus: self.bus.read().clone(),
        }))
    }

    /// 主动与指定设备同步（IPC sync_now；addr 来自发现层/KVM 面板）
    ///
    /// 无论走到哪一步失败都落一行 `sync_run`（含"未配对/连不上"这类还没开会话的失败，
    /// peer 列如实记用户点的那台设备 id）。只有 `ctx` 未就绪时不记——那时连库都没有，
    /// 谈不上静默：错误本身已如实上抛。
    pub async fn sync_with(&self, device_id: &str, addr: &str) -> R<SyncSummary> {
        let ctx = self.ctx()?;
        let started = now_ms();
        let mut summary = SyncSummary::default();
        let outcome = initiate(&ctx, device_id, addr, &mut summary).await;
        finish_run(
            &ctx,
            device_id,
            ROLE_INITIATOR,
            started,
            &summary,
            outcome.as_ref().err(),
        );
        outcome?;
        ctx.publish_state(&summary);
        Ok(summary)
    }

    /// 本地变更记录入口（notes.changed 订阅回调 / IPC）
    pub fn record_change(&self, entity: &str, path: &str, action: &str) -> R<()> {
        let ctx = self.ctx()?;
        record_change_with(&ctx, entity, path, action)
    }

    /// accept 循环（start 内 tokio spawn）
    ///
    /// `listening` / `bind_error` 是监听真态的唯一写口：bind 成功才 true（并清旧错），
    /// 失败则存下含端口号的原文（面板拿它出红条）。过去 bind 失败只 warn 后 return，
    /// 而 `start()` 无条件置 Running ⇒ 端口被占时面板依旧写"监听 :49820"，是假绿位。
    async fn accept_loop(
        ctx: Arc<SyncCtx>,
        port: u16,
        cancel: Arc<AtomicBool>,
        listening: Arc<AtomicBool>,
        bind_error: Arc<RwLock<Option<String>>>,
    ) {
        let listener = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
            Ok(l) => l,
            Err(e) => {
                // 文本必须带端口号：占用者往往是别的进程，用户要知道抢的是哪个口
                let msg = format!("端口 {port} 监听失败：{e}");
                *bind_error.write() = Some(msg.clone());
                listening.store(false, Ordering::SeqCst);
                tracing::warn!(port, error = %e, "SYNC 监听失败（仅可发起同步）");
                return;
            }
        };
        *bind_error.write() = None;
        listening.store(true, Ordering::SeqCst);
        tracing::info!(port, "SYNC 监听就绪");
        loop {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let Ok((stream, peer_addr)) = listener.accept().await else {
                continue;
            };
            let ctx = ctx.clone();
            let peer_addr = peer_addr.to_string();
            tokio::spawn(async move {
                let started = now_ms();
                let mut summary = SyncSummary::default();
                let (peer_label, outcome) = respond(&ctx, stream, &peer_addr, &mut summary).await;
                finish_run(
                    &ctx,
                    &peer_label,
                    ROLE_RESPONDER,
                    started,
                    &summary,
                    outcome.as_ref().err(),
                );
                match &outcome {
                    Ok(()) => {
                        tracing::info!(peer = %peer_label, ?summary, "SYNC 入站会话完成");
                        ctx.publish_state(&summary);
                    }
                    Err(e) => tracing::warn!(peer = %peer_label, error = %e, "SYNC 入站会话失败"),
                }
            });
        }
        // 退出循环即监听套接字已被丢弃：真态跟着翻回 false，不留"上次启动时确实在听"的余温
        listening.store(false, Ordering::SeqCst);
    }
}

impl Module for SyncModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "sync",
            name: "跨设备同步",
            version: "0.1.0",
            icon: Some("sync"),
            priority: priority_of("sync"),
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        // 信任根：身份（与 KVM 同文件；DPAPI 保护跟随 CryptoPort 可用性）
        let crypto = ctx.ports.get::<dyn host_core::ports::CryptoPort>();
        let identity = DeviceIdentity::load_or_create(&ctx.app_data_dir.join("kvm"), crypto)
            .map_err(|e| ModuleError::Init(e.to_string()))?;
        *self.identity.write() = Some(Arc::new(identity));
        self.migrate_legacy_db_path();
        let log = OpLog::open(&self.db_path).map_err(|e| ModuleError::Init(e.to_string()))?;
        *self.log.write() = Some(Arc::new(log));
        *self.bus.write() = Some(ctx.event_bus.clone());
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        let Ok(ctx) = self.ctx() else {
            tracing::warn!("SYNC 上下文未就绪，跳过启动");
            return Ok(());
        };
        // SYNC1：accept 循环（bind 失败仍可主动发起同步，但监听真态如实落 listening）
        let port = self.port.load(Ordering::SeqCst);
        let cancel = self.cancel.clone();
        // 每次启动先把两枚事实清零：上一轮遗留的 true 会让面板在"这一轮其实没听上"时仍报绿
        self.listening.store(false, Ordering::SeqCst);
        *self.bind_error.write() = None;
        let ctx_listen = ctx.clone();
        let listening = self.listening.clone();
        let bind_error = self.bind_error.clone();
        tokio::spawn(async move {
            SyncModule::accept_loop(ctx_listen, port, cancel, listening, bind_error).await
        });
        // SYNC2：订阅本地变更（notes.changed → op_log 快照）
        // 主题表 v1 只有 note 一个发布方；第二数据集接入时在这里加一路订阅，
        // 事件 → 变更记录的解析与入库全在 `record_change_event` 里，不必再动这里。
        if let Some(bus) = self.bus.read().clone() {
            if let Ok(mut rx) = bus.subscribe("notes.changed") {
                let ctx2 = ctx.clone();
                tokio::spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(event) => {
                                if event.source == "sync" {
                                    continue; // 防自环（理论不可达：apply 直调不发事件）
                                }
                                if let Err(e) = record_change_event(&ctx2, &event) {
                                    tracing::warn!(error = %e, "本地变更入 op_log 失败");
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(_) => break,
                        }
                    }
                });
            }
        }
        self.state.set(ModuleState::Running);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        self.cancel.store(true, Ordering::SeqCst);
        // 取消信号发出即不再接受新会话：监听真态跟着落，不等 accept_loop 下一轮醒来
        self.listening.store(false, Ordering::SeqCst);
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    /// 配置 schema（T-B5-5 起为**空**：原先那枚字符串型"范围说明"键是全仓零读者的死键——
    /// 设置项要么有真消费方要么不出现，"有个输入框但没人读"会让用户以为它生效了。
    /// 自动同步/暂停等真键由 T-B5-6 经 `apply_config` 活通路立起来）
    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    fn apply_config(&self, _values: serde_json::Value) -> Result<(), ModuleError> {
        Ok(())
    }

    fn status(&self) -> ModuleState {
        self.state.get()
    }

    fn set_status(&self, state: ModuleState) {
        self.state.set(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_appdata(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nf_syncmod_{tag}_{}", uuid::Uuid::now_v7()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// D-12 归位断言：sync.db 父目录必须是 db/（防止后续新增模块重犯）
    #[test]
    fn db_path_is_under_db_dir() {
        let dir = temp_appdata("d12path");
        let m = SyncModule::new(&dir);
        let parent = m.db_path().parent().unwrap();
        assert!(
            parent.ends_with("db"),
            "sync.db 应位于 {{appData}}/db/ 下，实际 {parent:?}"
        );
        assert_eq!(m.db_path().file_name().unwrap(), "sync.db");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D-12 一次性搬移：旧 sync/sync.db（含 WAL 伴生）→ db/sync.db；幂等；不覆盖已有新库
    #[test]
    fn legacy_db_moved_once_and_idempotent() {
        let dir = temp_appdata("d12move");
        let legacy_dir = dir.join("sync");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("sync.db"), b"legacy-bytes").unwrap();
        std::fs::write(legacy_dir.join("sync.db-wal"), b"wal-bytes").unwrap();

        let m = SyncModule::new(&dir);
        m.migrate_legacy_db_path();
        assert_eq!(std::fs::read(m.db_path()).unwrap(), b"legacy-bytes");
        assert_eq!(
            std::fs::read(dir.join("db").join("sync.db-wal")).unwrap(),
            b"wal-bytes"
        );
        assert!(!legacy_dir.exists(), "搬移后旧空目录应被移除");

        // 幂等：再跑一次不动内容
        m.migrate_legacy_db_path();
        assert_eq!(std::fs::read(m.db_path()).unwrap(), b"legacy-bytes");

        // 新路径已存在库时绝不被旧文件覆盖
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("sync.db"), b"stale-legacy").unwrap();
        m.migrate_legacy_db_path();
        assert_eq!(
            std::fs::read(m.db_path()).unwrap(),
            b"legacy-bytes",
            "已有 db/sync.db 不应被旧路径文件覆盖"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 内存数据集（恢复路径必须真的写穿，不能只动 op_log）
    #[derive(Default)]
    struct MemStore {
        data: parking_lot::Mutex<std::collections::HashMap<String, serde_json::Value>>,
    }
    impl MemStore {
        fn put(&self, id: &str, content: &str) {
            self.data
                .lock()
                .insert(id.to_string(), serde_json::json!({ "content": content }));
        }
        fn content_of(&self, id: &str) -> Option<String> {
            self.data
                .lock()
                .get(id)
                .and_then(|v| v["content"].as_str().map(String::from))
        }
    }
    impl ChangeApplier for MemStore {
        fn snapshot(&self, _e: &str, id: &str) -> R<Option<serde_json::Value>> {
            Ok(self.data.lock().get(id).cloned())
        }
        fn apply_upsert(&self, _e: &str, id: &str, value: &serde_json::Value) -> R<()> {
            self.data.lock().insert(id.to_string(), value.clone());
            Ok(())
        }
        fn apply_delete(&self, _e: &str, id: &str) -> R<()> {
            self.data.lock().remove(id);
            Ok(())
        }
    }

    /// 一整套会话上下文（身份 + 空 PairStore + 真 op_log + 真总线）：
    /// 冲突落盘点在 `apply_ops` 里，绕开它就测不到"op 在手是唯一零成本落盘点"这件事。
    fn harness(tag: &str) -> (Arc<SyncCtx>, Arc<MemStore>) {
        let dir = temp_appdata(tag);
        let kvm = dir.join("kvm");
        let identity = Arc::new(DeviceIdentity::load_or_create(&kvm, None).unwrap());
        let store = Arc::new(PairStore::load_or_default(&kvm).unwrap());
        let log = Arc::new(OpLog::open(&dir.join("db").join("sync.db")).unwrap());
        let applier = Arc::new(MemStore::default());
        let dyn_applier: Arc<dyn ChangeApplier> = applier.clone();
        let mut appliers: HashMap<String, Arc<dyn ChangeApplier>> = HashMap::new();
        appliers.insert(ENTITY_NOTE.to_string(), dyn_applier);
        let ctx = Arc::new(SyncCtx {
            identity,
            store,
            log,
            appliers: Arc::new(appliers),
            bus: Some(Arc::new(EventBus::new())),
        });
        (ctx, applier)
    }

    fn note_op(id: &str, device: &str, ts: i64, content: &str) -> crate::oplog::OpEntry {
        crate::oplog::OpEntry {
            op_id: id.into(),
            entity: "note".into(),
            entity_id: "x.md".into(),
            ts,
            device: device.into(),
            value: serde_json::json!({ "content": content }),
        }
    }

    /// 任务书（09 §10.2 T-B5-2）字面测试名优先于 rustc 命名惯例
    #[test]
    #[allow(non_snake_case)]
    fn conflictLog_lostRemoteValue_persistedAndQueryable() {
        let (ctx, store) = harness("conflictpersist");
        let self_dev = ctx.identity.device_id.clone();
        // 本地胜者在前：ts=200 压住随后到达的 ts=100
        ctx.log
            .append(&note_op("w1", &self_dev, 200, "本地一份"))
            .unwrap();
        store.put("x.md", "本地一份");
        let mut rx = ctx
            .bus
            .clone()
            .unwrap()
            .subscribe("sync.conflict")
            .expect("sync.conflict 主题在册");

        let (applied, lost, conflicts) = ctx
            .apply_ops(&[note_op("l1", "devB", 100, "远端那份被比掉了")], "devB")
            .unwrap();
        assert_eq!((applied, lost, conflicts), (0, 1, 1));

        let rows = ctx.log.conflicts(10, 0).unwrap();
        assert_eq!(rows.len(), 1, "承重⑤：判负内容今天必须能在本机查回");
        assert_eq!(rows[0].lost_value["content"], "远端那份被比掉了");
        assert_eq!(rows[0].lost_device, "devB");
        assert_eq!(rows[0].winner_device, self_dev, "胜者是本机条目");
        assert_eq!(rows[0].winner_ts, 200);
        assert!(!rows[0].conflict_id.is_empty());

        // 事件只是提示，且带得上回查用的 id；内容不经事件广播
        let ev = rx.try_recv().expect("冲突事件应已发布");
        assert_eq!(ev.payload["conflict_id"], rows[0].conflict_id);
        assert_eq!(ev.payload["loser_device"], "devB");
        assert!(
            ev.payload.get("lost_value").is_none(),
            "笔记内容不进广播载荷（按需读 sync_conflicts_get）"
        );
    }

    /// 任务书（09 §10.2 T-B5-2）字面测试名优先于 rustc 命名惯例
    #[test]
    #[allow(non_snake_case)]
    fn conflictLog_sameLoserReplay_singleRow() {
        let (ctx, _store) = harness("conflictreplay");
        let self_dev = ctx.identity.device_id.clone();
        ctx.log
            .append(&note_op("w1", &self_dev, 200, "本地一份"))
            .unwrap();
        let loser = note_op("l1", "devB", 100, "同一份败方内容");
        // 对端没收到 Ack 而重推同一批：确定性 id ⇒ INSERT OR IGNORE 吸收
        ctx.apply_ops(std::slice::from_ref(&loser), "devB").unwrap();
        ctx.apply_ops(std::slice::from_ref(&loser), "devB").unwrap();
        assert_eq!(
            ctx.log.conflicts(10, 0).unwrap().len(),
            1,
            "重放不得把一次冲突灌成两行历史"
        );

        // 正对照：换了 ts 就是另一个败方快照，另起一行（不是 id 恒同的假去重）
        ctx.apply_ops(&[note_op("l2", "devB", 101, "另一份")], "devB")
            .unwrap();
        assert_eq!(ctx.log.conflicts(10, 0).unwrap().len(), 2);
    }

    /// 任务书（09 §10.2 T-B5-2）字面测试名优先于 rustc 命名惯例
    #[test]
    #[allow(non_snake_case)]
    fn conflictRestore_reappliesAsNewerLocalOp() {
        let (ctx, store) = harness("conflictrestore");
        let self_dev = ctx.identity.device_id.clone();
        ctx.log
            .append(&note_op("w1", &self_dev, 200, "本地新格"))
            .unwrap();
        store.put("x.md", "本地新格");
        ctx.apply_ops(&[note_op("l1", "devB", 100, "远端旧格")], "devB")
            .unwrap();
        let rows = ctx.log.conflicts(10, 0).unwrap();

        let restored = restore_conflict_with(&ctx, &rows[0].conflict_id).unwrap();
        // ① 数据集真的换回败方内容（只写 op_log 不写盘＝本机自己不一致）
        assert_eq!(store.content_of("x.md").as_deref(), Some("远端旧格"));
        // ② 以本机自产 + 更新 ts 入流 ⇒ 下轮同步覆盖对端
        assert_eq!(restored.device, self_dev);
        assert!(
            restored.ts > 200,
            "新 op 必须晚于当时胜者，否则 LWW 仍判它负"
        );
        let fetchable = ctx.log.ops_of_device(&self_dev, 200, 10).unwrap();
        assert_eq!(fetchable.len(), 1, "对端按自产游标拉得到这条恢复 op");
        assert_eq!(fetchable[0].op_id, restored.op_id);
        // ③ 本地快照随后被这条新 op 覆盖（latest 不再是当时的胜者行）
        assert_eq!(
            ctx.log
                .latest_for("note", "x.md")
                .unwrap()
                .expect("恢复后必有最新条目")
                .op_id,
            restored.op_id
        );
    }

    /// 任务书（09 §10.2 T-B5-2）字面测试名优先于 rustc 命名惯例
    #[test]
    #[allow(non_snake_case)]
    fn conflictLog_unknownId_honestErr() {
        let (ctx, store) = harness("conflictunknown");
        store.put("y.md", "保持原样");
        let err = restore_conflict_with(&ctx, "deadbeef").unwrap_err();
        assert!(matches!(err, SyncError::Apply(_)), "实际 {err:?}");
        assert!(
            err.to_string().contains("deadbeef"),
            "错误消息要点名是哪个 id 取不到，实际：{err}"
        );
        assert_eq!(ctx.log.count(), 0, "取不到快照就不该往变更流里塞东西");
        assert_eq!(store.content_of("y.md").as_deref(), Some("保持原样"));
    }

    /// 第二个内存数据集（与 note 零共享：证明泛化不是"给 note 特判加一个分支"）
    #[derive(Default)]
    struct ClipStore {
        data: parking_lot::Mutex<HashMap<String, serde_json::Value>>,
    }
    impl ChangeApplier for ClipStore {
        fn snapshot(&self, _e: &str, id: &str) -> R<Option<serde_json::Value>> {
            Ok(self.data.lock().get(id).cloned())
        }
        fn apply_upsert(&self, _e: &str, id: &str, value: &serde_json::Value) -> R<()> {
            self.data.lock().insert(id.to_string(), value.clone());
            Ok(())
        }
        fn apply_delete(&self, _e: &str, id: &str) -> R<()> {
            self.data.lock().remove(id);
            Ok(())
        }
    }

    /// 任务书（09 §10.2 T-B5-5）字面测试名：承重⑪红线——快照读不到 ≠ 已被删除
    #[test]
    #[allow(non_snake_case)]
    fn syncRegistry_missingSnapshot_neverBecomesDelete() {
        let (ctx, store) = harness("missingsnapshot");
        let err = record_change_with(&ctx, ENTITY_NOTE, "gone.md", "write").unwrap_err();
        assert!(matches!(err, SyncError::Entity(_)), "实际 {err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("快照读取失败"),
            "要自证拒的是什么，实际：{msg}"
        );
        assert!(
            msg.contains("已拒记删除标记"),
            "要点名拒的是删除标记，实际：{msg}"
        );
        assert!(msg.contains("gone.md"), "要点名是哪个路径，实际：{msg}");
        assert_eq!(
            ctx.log.count(),
            0,
            "一笔都不能入流（旧语义在此写入 deleted:true，等于把读失败广播成删除）"
        );

        // 正对照 A：文件真存在时同一调用正常入流（证明拒的是"读不到"而非 write 动作本身）
        store.put("here.md", "内容还在");
        record_change_with(&ctx, ENTITY_NOTE, "here.md", "write").unwrap();
        let op = ctx
            .log
            .latest_for(ENTITY_NOTE, "here.md")
            .unwrap()
            .expect("刚写入的快照");
        assert_eq!(op.value["content"], "内容还在");
        assert!(!op.is_delete(), "write 永远不该产出删除标记");

        // 正对照 B：真删除仍走 delete 通道（红线是"误删除"，不是"禁止删除"）
        record_change_with(&ctx, ENTITY_NOTE, "here.md", "delete").unwrap();
        let op = ctx
            .log
            .latest_for(ENTITY_NOTE, "here.md")
            .unwrap()
            .expect("删除应入流");
        assert!(op.is_delete());
        assert_eq!(ctx.log.count(), 2);

        // 未注册数据集连门都进不来（vault 的 record 侧可否例面）
        let err = record_change_with(&ctx, "vault", "here.md", "write").unwrap_err();
        assert!(matches!(err, SyncError::Entity(_)), "实际 {err:?}");
        assert_eq!(ctx.log.count(), 2, "拒收不留痕");
    }

    /// 任务书（09 §10.2 T-B5-5）字面测试名：第二个数据集只需注册表一行，引擎零改
    ///
    /// 判据的另一半在本行提交的文件清单（`engine.rs` / `transport.rs` / `oplog.rs`
    /// 未出现在 diff 里），已记入 09 §12 T-B5-5 证据段——测试能证语义，证不了"没改"。
    #[test]
    #[allow(non_snake_case)]
    fn syncRegistry_secondEntityNeedsNoEngineChange() {
        let dir = temp_appdata("secondentity");
        let kvm = dir.join("kvm");
        let identity = Arc::new(DeviceIdentity::load_or_create(&kvm, None).unwrap());
        let store = Arc::new(PairStore::load_or_default(&kvm).unwrap());
        let log = Arc::new(OpLog::open(&dir.join("db").join("sync.db")).unwrap());
        let notes = Arc::new(MemStore::default());
        let clips = Arc::new(ClipStore::default());
        let mut appliers: HashMap<String, Arc<dyn ChangeApplier>> = HashMap::new();
        appliers.insert(ENTITY_NOTE.to_string(), notes.clone());
        appliers.insert("clip".to_string(), clips.clone());
        let ctx = Arc::new(SyncCtx {
            identity,
            store,
            log,
            appliers: Arc::new(appliers),
            bus: Some(Arc::new(EventBus::new())),
        });

        // ---- record 半：假实体走同一个 record_change_with，零特判 ----
        clips
            .data
            .lock()
            .insert("c1".into(), serde_json::json!({ "text": "剪贴板一条" }));
        record_change_with(&ctx, "clip", "c1", "write").unwrap();
        let op = ctx
            .log
            .latest_for("clip", "c1")
            .unwrap()
            .expect("clip 变更入流");
        assert_eq!(op.entity, "clip", "entity 列就是分派依据，无第二套表");
        assert_eq!(op.value["text"], "剪贴板一条");

        // ---- apply 半：对端推来的 clip 变更按 entity 找到 clip 应用器 ----
        let remote = crate::oplog::OpEntry {
            op_id: "r1".into(),
            entity: "clip".into(),
            entity_id: "c2".into(),
            ts: 300,
            device: "devB".into(),
            value: serde_json::json!({ "text": "对端那条" }),
        };
        let (applied, lost, conflicts) = ctx
            .apply_ops(std::slice::from_ref(&remote), "devB")
            .unwrap();
        assert_eq!((applied, lost, conflicts), (1, 0, 0));
        assert_eq!(clips.data.lock()["c2"]["text"], "对端那条");
        assert!(
            notes.data.lock().get("c2").is_none(),
            "clip 的变更绝不落进 note 数据集（分派错就是跨数据集投毒）"
        );
        // 游标随成功推进（同一张 cursors 表，无 per-entity 分支）
        assert_eq!(ctx.log.cursor("devB"), 300);

        // 没装应用器的数据集仍然拒（第三者在场也不会把会话变成静默跳过）
        let ghost = crate::oplog::OpEntry {
            entity: "vault".into(),
            ..remote
        };
        let err = ctx
            .apply_ops(std::slice::from_ref(&ghost), "devB")
            .unwrap_err();
        assert!(matches!(err, SyncError::Entity(_)), "实际 {err:?}");
        assert_eq!(
            ctx.log.cursor("devB"),
            300,
            "拒收的一批不许推进游标（否则对端被谎报\"已同步\"）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 任务书（09 §10.2 T-B5-5）字面测试名：非内容动作零入流
    #[test]
    #[allow(non_snake_case)]
    fn syncEntity_actionNotInWhitelist_ignored() {
        // 索引/卡片/画布/同步事件不是"某篇笔记变了"——当变更入流会喂对端假删除
        for (action, path) in [
            ("sync", serde_json::Value::Null),
            ("reindex", serde_json::Value::Null),
            ("cards", serde_json::Value::Null),
            ("canvas", serde_json::json!("canvas/board1")),
        ] {
            let ev = Event::new(
                "notes.changed",
                "notes",
                serde_json::json!({ "action": action, "path": path }),
            );
            assert!(
                change_records_of_event(&ev).is_none(),
                "动作 {action} 不该解析出任何变更记录"
            );
        }
        assert_eq!(
            CHANGE_ACTIONS,
            ["create", "write", "delete", "rename"],
            "白名单成员变化必须同时带来测试与文档"
        );

        // 入库面同样零增（不是"解析为空但别处仍写"）；正对照：换动作即一条
        let (ctx, store) = harness("actionwhitelist");
        store.put("a.md", "只此一份");
        let canvas = Event::new(
            "notes.changed",
            "notes",
            serde_json::json!({ "action": "canvas", "path": "a.md" }),
        );
        assert_eq!(record_change_event(&ctx, &canvas).unwrap(), 0);
        assert_eq!(ctx.log.count(), 0);
        let write = Event::new(
            "notes.changed",
            "notes",
            serde_json::json!({ "action": "write", "path": "a.md" }),
        );
        assert_eq!(record_change_event(&ctx, &write).unwrap(), 1);
        assert_eq!(ctx.log.count(), 1);

        // rename 缺 old_path（发布方半截升级）⇒ 整条丢弃，不写成"只新建不删旧"的半改名
        let half = Event::new(
            "notes.changed",
            "notes",
            serde_json::json!({ "action": "rename", "path": "b.md" }),
        );
        assert!(change_records_of_event(&half).is_none());
        assert_eq!(record_change_event(&ctx, &half).unwrap(), 0);

        // 事件自称的数据集也要过白名单这道门（vault 的 publish 侧可否例面）
        let vault = Event::new(
            "notes.changed",
            "vault",
            serde_json::json!({ "action": "write", "path": "secret", "entity": "vault" }),
        );
        let err = record_change_event(&ctx, &vault).unwrap_err();
        assert!(matches!(err, SyncError::Entity(_)), "实际 {err:?}");
        assert!(err.to_string().contains("vault"));
        assert_eq!(ctx.log.count(), 1, "门外的数据集零入流");
    }
}
