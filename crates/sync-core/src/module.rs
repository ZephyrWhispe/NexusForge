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
//! - 自动同步（T-B5-6）：`SyncConfig` 三键经 `apply_config` 活派发即改即生效（不重启）；
//!   变更入流后排静默窗，到期一轮只往"本机成功同步过"的地址出账；`auto_sync` **默认关**
//!   （不擅自开始往外发用户的笔记），暂停位与面板徽章读同一内存
//! - 红线：密码库条目**永不**自动同步（白名单硬编码，`attach_applier("vault", …)` 运行期亦拒，
//!   见 `syncEntity_vaultNeverAdmitted` 三层可否例）
//!
//! 防死环：远端应用走 NoteLibrary 直调（不发 notes.changed——该事件由 IPC 层发布），
//! 本地订阅仅记录用户操作产生的变更。

use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
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

/// 静默窗下限（毫秒）：低于这个值，"连续改 20 篇笔记"会变成 20 轮出账会话。
/// 设置中心与 `merged` 读同一个常量——两处各写一份数字，迟早有一处懒得改。
pub const QUIET_PERIOD_MIN_MS: u64 = 5_000;
/// 默认静默窗（与 `config_schema()` 的 `default` 逐值相同：盘上只写过部分键时，
/// "运行期保持现值"与"下次启动从默认值起"必须落在同一个数上）
pub const DEFAULT_QUIET_PERIOD_MS: u64 = 60_000;
/// 冲突快照默认保留天数（同上，与 schema `default` 同源）
pub const DEFAULT_CONFLICT_KEEP_DAYS: u64 = 30;
/// 一天的毫秒数（保留窗算式唯一口）
const DAY_MS: i64 = 86_400_000;

/// 同步模块配置（T-B5-6：`config_schema()` 三键的唯一读者，`apply_config` 的唯一产物）
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    /// 变更入流后静默窗到期即自动出账。**默认关**：没有任何用户装完软件就期望
    /// 自己的笔记开始往外发的行为，开关必须自己按。
    pub auto_sync: bool,
    /// 静默窗（毫秒）：窗口内的连续变更合并成一轮同步（14-sync §4 Resilio 静默期借鉴）
    pub quiet_period_ms: u64,
    /// 冲突快照保留天数（`conflict_log` 裁剪的唯一窗口来源）
    pub conflict_keep_days: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            auto_sync: false,
            quiet_period_ms: DEFAULT_QUIET_PERIOD_MS,
            conflict_keep_days: DEFAULT_CONFLICT_KEEP_DAYS,
        }
    }
}

impl SyncConfig {
    /// 现运行态 ⊕ 补丁（**缺键 = 不动运行态**，B4 既定纪律）
    ///
    /// 整份反序列化会把"用户没填这一格"与"用户填了默认值"混成同一件事，而两者
    /// 后果差得远：前者应保持现状，后者应改值。坏值一律在这里弹回（全有或全无：
    /// 半套配置在跑比配置没生效更难查），`Err` 经派发口变成 `host.config_rejected`。
    /// 未知键（含旧盘上残留的已废弃键）读侧忽略——不写"已迁移"，旧键就是不再存在。
    pub fn merged(&self, values: &serde_json::Value) -> R<SyncConfig> {
        let mut next = self.clone();
        if let Some(v) = values.get("auto_sync") {
            next.auto_sync = v
                .as_bool()
                .ok_or_else(|| SyncError::Config(format!("auto_sync 须为布尔值，收到 {v}")))?;
        }
        if let Some(v) = values.get("quiet_period_ms") {
            let n = v
                .as_u64()
                .ok_or_else(|| SyncError::Config(format!("quiet_period_ms 须为整数，收到 {v}")))?;
            if n < QUIET_PERIOD_MIN_MS {
                return Err(SyncError::Config(format!(
                    "quiet_period_ms 不得低于 {QUIET_PERIOD_MIN_MS} 毫秒（收到 {n}）：静默窗比会话本身还短会把连续变更打成不停出账"
                )));
            }
            next.quiet_period_ms = n;
        }
        if let Some(v) = values.get("conflict_keep_days") {
            let n = v.as_u64().ok_or_else(|| {
                SyncError::Config(format!("conflict_keep_days 须为整数，收到 {v}"))
            })?;
            if n == 0 {
                return Err(SyncError::Config(
                    "conflict_keep_days 不得为 0：那等于每次派发就清空全部冲突历史".into(),
                ));
            }
            next.conflict_keep_days = n;
        }
        Ok(next)
    }
}

/// 静默窗闹钟槽：`(排程序号, 任务句柄)`。号让醒来后发现已被后来者取代的旧闹钟自己退场。
type AlarmSlot = Option<(u64, tokio::task::JoinHandle<()>)>;

/// 自动同步运行态（T-B5-6：静默窗的**唯一实现点**）
///
/// 五枚状态必须住在一起，分开写就会出现"用户按了暂停、闹钟还在等"这类分叉：
/// - `config` / `paused`：与 `SyncModule` 同一份（不是副本），面板读的状态与排程判定读的是同一位；
/// - `last_addr`：自动出账的地址事实源＝**最近一次成功会话**用过的地址（T-B5-7 换心跳宣告后接管）；
/// - `gen` + `slot`：取消-重排。每次排程领一个号，闹钟醒来发现号已被后来者取代就退场；
/// - `running`：一轮 due-peers 的互斥口——会话进行到一半又来变更时，不并发开第二条
///   到同一对端的会话（游标交错的代价是重复推送与假 Ack），排队等下一轮即可。
#[derive(Clone)]
struct AutoSync {
    config: Arc<RwLock<SyncConfig>>,
    paused: Arc<AtomicBool>,
    last_addr: Arc<RwLock<HashMap<String, String>>>,
    gen: Arc<AtomicU64>,
    slot: Arc<parking_lot::Mutex<AlarmSlot>>,
    running: Arc<tokio::sync::Mutex<()>>,
}

impl AutoSync {
    fn new() -> Self {
        Self {
            config: Arc::new(RwLock::new(SyncConfig::default())),
            paused: Arc::new(AtomicBool::new(false)),
            last_addr: Arc::new(RwLock::new(HashMap::new())),
            gen: Arc::new(AtomicU64::new(0)),
            slot: Arc::new(parking_lot::Mutex::new(None)),
            running: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// 变更已入流 ⇒ 重排静默窗（连投只留最后一颗闹钟）
    ///
    /// 关着（默认态）就一个字都不做；暂停态同样不排程（面板徽章已经说明了为什么）。
    /// 没有 tokio 上下文时**如实 warn**"这次没排上"，不假装成功——变更本身已经入流，
    /// 少一轮自动出账是可恢复的，谎报"已排程"是不可恢复的（用户不会再手动同步）。
    fn note_changed(&self, ctx: Arc<SyncCtx>) {
        let quiet = {
            let cfg = self.config.read();
            if !cfg.auto_sync || self.paused.load(Ordering::SeqCst) {
                return;
            }
            cfg.quiet_period_ms
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                "无 tokio 运行时上下文，本次变更未排程自动同步（请用「立即同步」手动出账）"
            );
            return;
        };
        let my_gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        let me = self.clone();
        let handle = rt.spawn(async move { me.run_after_quiet(my_gen, quiet, ctx).await });
        // 取消-重排：摘掉上一颗闹钟（它若已醒来交还槽位，此处 take 到的就是 None，
        // 正在跑的那轮不会被掐断——半途掐一条会话比多等一轮糟得多）
        if let Some((_, old)) = self.slot.lock().replace((my_gen, handle)) {
            old.abort();
        }
    }

    async fn run_after_quiet(self, my_gen: u64, quiet: u64, ctx: Arc<SyncCtx>) {
        tokio::time::sleep(std::time::Duration::from_millis(quiet)).await;
        // 交还闹钟要在同一把锁里比对号：只认"我仍是最新"，否则摘掉的是后来者的闹钟
        {
            let mut g = self.slot.lock();
            if !g.as_ref().is_some_and(|(gen, _)| *gen == my_gen) {
                return;
            }
            g.take();
        }
        // 到期时重读两枚事实：静默窗里用户可能按了暂停，也可能把自动同步整个关掉
        if self.paused.load(Ordering::SeqCst) || !self.config.read().auto_sync {
            return;
        }
        self.run_due(ctx).await;
    }

    /// 到期一轮：配对 ∩ 有地址 ∩ 未暂停逐个同步；失败只进流水与 `last_error`，**绝不记成功**
    async fn run_due(self, ctx: Arc<SyncCtx>) {
        let _round = self.running.lock().await;
        let keep_days = self.config.read().conflict_keep_days;
        for p in ctx.store.all() {
            if self.paused.load(Ordering::SeqCst) {
                tracing::info!(peer = %p.device_id, "本轮自动同步就此收手（已暂停）");
                return;
            }
            let Some(addr) = self.last_addr.read().get(&p.device_id).cloned() else {
                // 没有地址事实源就是没有：记一行失败流水点名原因，绝不拿 127.0.0.1 凑数
                let err = SyncError::Peer(format!(
                    "设备 {} 当前无可用地址（本机还没有与它成功同步过的记录，发现层亦未宣告同步端口）；\
                     可在面板「高级」手输 host:port 后点「立即同步」",
                    p.device_name
                ));
                tracing::warn!(peer = %p.device_id, error = %err, "自动同步跳过该设备（已入账）");
                record_skipped_run(&ctx, &p.device_id, &err);
                continue;
            };
            match sync_attempt(&ctx, &p.device_id, &addr).await {
                Ok(s) => tracing::info!(
                    peer = %p.device_id,
                    pushed = s.pushed,
                    applied = s.pulled_applied,
                    "自动同步完成一轮"
                ),
                // `sync_attempt` 内已把失败落进行流水（含 Err 臂），这里只出声不再记一次
                Err(e) => tracing::warn!(peer = %p.device_id, error = %e, "自动同步失败（已入账）"),
            }
        }
        // 保留窗真消费（每轮裁一次；改小配置后下一轮即生效，不必另立触发口）
        let cutoff = now_ms() - keep_days as i64 * DAY_MS;
        if let Err(e) = ctx.log.prune_conflicts_before(cutoff) {
            tracing::warn!(error = %e, "冲突快照保留窗裁剪失败（不影响本轮同步结果）");
        }
    }
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
    /// 手动暂停同步（T-B5-6 起读 `paused` 原子位：面板/托盘两个入口共用同一位，
    /// 读的不是"面板上那个开关的样子"而是那个位本身）
    pub paused: bool,
    /// 自动同步开关现值（T-B5-6）：与 `config_schema()` 的 `auto_sync` 同一个内存态。
    /// 面板拿它当开关的初值与回读面——设置写成功但运行态没变（值被派发口拒了）时，
    /// 这里读回来的仍是旧值，谎报就此无处藏身。
    pub auto_sync: bool,
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

/// 发起一侧：配对校验 → 连接 → 握手 → 身份核对 → 跑协议（计时与记账单点在 `sync_attempt`）
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

/// 一轮主动会话的完整生命周期：计时 → 跑协议 → **无论走到哪一步都落一行流水** → 成功才发状态事件
///
/// 从 `SyncModule::sync_with` 提出来是为了让自动出账那一轮与手动「立即同步」走**同一条**
/// 会话腿（两套记账迟早漂移：漂移的结果是面板上的数与流水表里的数各说各话）。
async fn sync_attempt(ctx: &Arc<SyncCtx>, device_id: &str, addr: &str) -> R<SyncSummary> {
    let started = now_ms();
    let mut summary = SyncSummary::default();
    let outcome = initiate(ctx, device_id, addr, &mut summary).await;
    finish_run(
        ctx,
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

/// 没开会话就放弃的一轮同样要入账（T-B5-3 纪律"错误路径必须记账"的延伸）：
/// 面板上"这台压根没动过"与"这台试过但没有地址"是两件事，前者查不到行、后者查得到原因。
fn record_skipped_run(ctx: &SyncCtx, peer: &str, err: &SyncError) {
    finish_run(
        ctx,
        peer,
        ROLE_INITIATOR,
        now_ms(),
        &SyncSummary::default(),
        Some(err),
    );
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
    /// 自动同步运行态（T-B5-6）：配置内存态 + 暂停位 + 地址事实源 + 静默窗闹钟
    auto: AutoSync,
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
            auto: AutoSync::new(),
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
            // 两枚开关都读运行态本身（不是盘上的值、也不是面板上的样子）：派发被拒时
            // 这里回读的是"此刻真在跑的那套"，面板因此不可能显示一个内核没在执行的开关
            paused: self.auto.paused.load(Ordering::SeqCst),
            auto_sync: self.auto.config.read().auto_sync,
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

    /// 主动与指定设备同步（IPC sync_now；addr 来自发现层/KVM 面板/用户手输）
    ///
    /// 无论走到哪一步失败都落一行 `sync_run`（含"未配对/连不上"这类还没开会话的失败，
    /// peer 列如实记用户点的那台设备 id）。只有 `ctx` 未就绪时不记——那时连库都没有，
    /// 谈不上静默：错误本身已如实上抛。
    ///
    /// 成功一次就记下这个地址：它是"自动出账该往哪台发"目前唯一的事实源（失败过或
    /// 压根没试过的地址不算数——把猜的地址用于自动出账，等于把用户的笔记发给邻居）。
    pub async fn sync_with(&self, device_id: &str, addr: &str) -> R<SyncSummary> {
        let ctx = self.ctx()?;
        let summary = sync_attempt(&ctx, device_id, addr).await?;
        self.auto
            .last_addr
            .write()
            .insert(device_id.to_string(), addr.to_string());
        Ok(summary)
    }

    /// 暂停/恢复同步（**唯一写口**：面板与以后托盘两处入口共用，`status().paused` 读同一位）
    ///
    /// 暂停即刻掐掉在等的静默窗；**恢复不补跑**——用户点"恢复"期望的是"以后照常"，
    /// 不是"立刻往外发一批"，要立刻出账那里有「立即同步」。恢复后下一次变更重新排程。
    pub fn set_paused(&self, paused: bool) {
        self.auto.paused.store(paused, Ordering::SeqCst);
        if paused {
            if let Some((_, h)) = self.auto.slot.lock().take() {
                h.abort();
            }
        }
        tracing::info!(paused, "SYNC 暂停位已更新");
    }

    /// 当前暂停位（托盘等处读用）
    pub fn is_paused(&self) -> bool {
        self.auto.paused.load(Ordering::SeqCst)
    }

    /// 变更入流后的端上通知（静默窗排程的**公开入口**）：IPC `sync_record_change` 与宿主直调都走这里
    ///
    /// 自动同步关着时它是空操作（默认关 ⇒ 本批上线行为零变化）。订阅臂在 spawn 任务里
    /// 只持有 `AutoSync` 克隆（拿不到 `&SyncModule`），因此直呼 `auto.note_changed(ctx)`——
    /// 排程本体只有那一个实现点，本方法只是把它接到模块 API 上。
    pub fn note_change_recorded(&self) {
        let Ok(ctx) = self.ctx() else {
            return; // 未就绪：没身份没库，排程也无从谈起；真错误由变更记录那条路自己上抛
        };
        self.auto.note_changed(ctx);
    }

    /// 到期一轮自动出账（静默窗跑的就是这个；测试与"以后的一键全部同步"共用同一口）
    pub async fn sync_due_peers(&self) -> R<()> {
        let ctx = self.ctx()?;
        self.auto.clone().run_due(ctx).await;
        Ok(())
    }

    /// 本机为某设备记着的可用地址（最近一次**成功**会话用过的那个；无则 None）
    pub fn peer_addr(&self, device_id: &str) -> Option<String> {
        self.auto.last_addr.read().get(device_id).cloned()
    }

    /// 本地变更记录入口（IPC sync_record_change / 宿主直调）：入流成功即端上通知
    pub fn record_change(&self, entity: &str, path: &str, action: &str) -> R<()> {
        let ctx = self.ctx()?;
        record_change_with(&ctx, entity, path, action)?;
        // 排程挂在"这笔已经进流"之后：入流失败的东西没有可同步的内容，
        // 排一次闹钟只会让对端拉一个空批。
        self.note_change_recorded();
        Ok(())
    }

    /// 静默窗闹钟是否挂着（测试缝：负例"没排上"要能一眼看出来，
    /// 而不是靠 sleep 之后没发生什么——那种断言在真机上永远成立）
    #[cfg(test)]
    fn has_pending_timer(&self) -> bool {
        self.auto.slot.lock().is_some()
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
                let auto = self.auto.clone();
                tokio::spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(event) => {
                                if event.source == "sync" {
                                    continue; // 防自环（理论不可达：apply 直调不发事件）
                                }
                                match record_change_event(&ctx2, &event) {
                                    Ok(0) => {} // 非内容事件（索引/卡片/画布），正常静默
                                    Ok(_) => auto.note_changed(ctx2.clone()),
                                    Err(e) => {
                                        tracing::warn!(error = %e, "本地变更入 op_log 失败")
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

    /// 配置 schema（T-B5-6：三键**全有真读者**——`auto_sync`/`quiet_period_ms` 由
    /// `AutoSync` 消费，`conflict_keep_days` 由本派发口与每轮自动出账消费）
    ///
    /// 数字常量和 `SyncConfig::default()` 同源：schema 里的 `default` 是设置界面的
    /// 初始展示值，运行态默认值在 `impl Default`，两处各写一份数字迟早有一处懒得改。
    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "auto_sync": {
                    "type": "boolean",
                    "title": "自动同步",
                    "description": "本地变更入流后，静默窗到期即自动向\"本机成功同步过\"的设备出账；关闭时只手动同步（默认关）",
                    "default": false
                },
                "quiet_period_ms": {
                    "type": "integer",
                    "minimum": QUIET_PERIOD_MIN_MS,
                    "title": "静默期(毫秒)",
                    "description": "窗口内的连续变更合并为一轮同步（下限 5000：静默窗比一轮会话本身还短会把\"合并\"变成\"不停出账\"）",
                    "default": DEFAULT_QUIET_PERIOD_MS
                },
                "conflict_keep_days": {
                    "type": "integer",
                    "minimum": 1,
                    "title": "冲突快照保留(天)",
                    "description": "判负快照在本机 conflict_log 的留存天数，超窗即裁（0 不接受：那等于每次派发清空全部冲突历史）",
                    "default": DEFAULT_CONFLICT_KEEP_DAYS
                }
            }
        })
    }

    /// 活配置派发（B3 修好的通路：ConfigStore 写成功 → registry 派发到此，**不重启即生效**）
    ///
    /// 三条纪律都在这里：
    /// - **缺键＝不动运行态**（`merged`）：整份反序列化会把"用户没填这格"和"用户填了默认值"
    ///   混成一件事，前者应保持现状；
    /// - **全有或全无**：坏值在写运行态**之前**弹回，经 `host.config_rejected` 回到设置界面，
    ///   内存里永远不会出现"半个新配置在跑"；
    /// - **幂等**：同一份值连派两次结果相同（`set_module` 是整段替换，派发也可能重放）。
    fn apply_config(&self, values: serde_json::Value) -> Result<(), ModuleError> {
        let next = self
            .auto
            .config
            .read()
            .merged(&values)
            .map_err(|e| ModuleError::Config(e.to_string()))?;
        *self.auto.config.write() = next.clone();
        // 保留窗真消费：改小配置不等下一轮自动出账才说话
        let cutoff = now_ms() - next.conflict_keep_days as i64 * DAY_MS;
        match self.log_arc() {
            Ok(log) => {
                if let Err(e) = log.prune_conflicts_before(cutoff) {
                    tracing::warn!(error = %e, "冲突快照保留窗裁剪失败（配置值本身已生效）");
                }
            }
            // init 前派发（启动早期 feed）：没库就没什么可裁，配置值照常生效
            Err(e) => tracing::debug!(error = %e, "op_log 未就绪，跳过冲突快照保留窗裁剪"),
        }
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

    // ---------------- T-B5-6：自动同步 + 暂停/恢复 ----------------

    /// 有界轮询到条件成立（超时即 panic；把"等一会儿再看"当断言是假绿的温床）
    async fn poll_until(what: &str, mut f: impl FnMut() -> bool) {
        for _ in 0..100 {
            if f() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("等待超时：{what}");
    }

    /// 往 `conflict_log` 塞一条指定落盘时刻的快照（保留窗判据需要可控时钟）
    fn seed_conflict_at(log: &OpLog, id: &str, recorded_ms: i64) {
        log.record_conflict(&ConflictEntry {
            conflict_id: id.into(),
            entity: ENTITY_NOTE.into(),
            entity_id: format!("{id}.md"),
            lost_ts: recorded_ms,
            lost_device: "devB".into(),
            winner_device: "devA".into(),
            winner_ts: recorded_ms + 1,
            lost_value: serde_json::json!({ "content": "判负的那一份" }),
            recorded_ms,
        })
        .unwrap();
    }

    /// 自动同步两枚测共用装配：init + 装 note 应用器 + 一台配对设备。
    ///
    /// **不调 `start()`**：本行要证的是"排没排闹钟、到期跑没跑"，起监听只会去抢
    /// 49820（与真应用实例撞口）；地址事实源刻意留空，到期那轮如实记失败行。
    async fn autosync_module(tag: &str) -> (Arc<SyncModule>, Arc<MemStore>, OpLog, PathBuf) {
        let dir = temp_appdata(tag);
        let m = Arc::new(SyncModule::new(&dir));
        let store = Arc::new(MemStore::default());
        m.attach_applier(ENTITY_NOTE, store.clone()).unwrap();
        let ctx = Arc::new(ModuleContext {
            app_data_dir: dir.clone(),
            ports: Arc::new(host_core::ports::Ports::new()),
            event_bus: Arc::new(EventBus::new()),
        });
        m.init(ctx).unwrap();
        m.register_peer(PairedPeer {
            device_id: "devB".into(),
            device_name: "笔记本-B".into(),
            fingerprint: "aa:bb".into(),
            pubkey_b64: "AAAA".into(),
            paired_at: 0,
        });
        let log = OpLog::open(m.db_path()).unwrap();
        (m, store, log, dir)
    }

    /// 任务书（09 §10.2 T-B5-6）字面测试名：三键全链（真 ConfigStore + 真派发循环，全程不重启）
    ///
    /// 三层各测各的事实：schema 与 `Default` 同源（设置界面显示的初值就是运行态的初值）、
    /// `merged` 的缺键语义（纯函数面）、写盘→派发→`status()` 的活通路（机检判据的测试形态）。
    #[tokio::test(flavor = "multi_thread")]
    #[allow(non_snake_case)]
    async fn syncConfig_threeKeys_roundTrip() {
        let dir = temp_appdata("cfgroute");
        let bus = Arc::new(EventBus::new());
        let store_cfg = Arc::new(host_core::config::ConfigStore::new(
            dir.join("config"),
            bus.clone(),
        ));
        let module = Arc::new(SyncModule::new(&dir));
        store_cfg.register_schema("sync", module.config_schema());
        let registry = Arc::new(host_core::registry::ModuleRegistry::new(bus.clone()));
        registry.register(module.clone()).unwrap();
        // 订阅早于第一次写：派发循环只认订阅之后的事件，订阅晚了就是"写了没生效"
        let rx = bus.subscribe("host.config_changed").unwrap();
        tokio::spawn(host_core::registry::run_config_feed(
            rx,
            registry.clone(),
            store_cfg.clone(),
        ));
        let ctx = Arc::new(ModuleContext {
            app_data_dir: dir.clone(),
            ports: Arc::new(host_core::ports::Ports::new()),
            event_bus: bus.clone(),
        });
        for (_, r) in registry.init_all(ctx).await {
            r.expect("init 应成功");
        }

        // ---- schema 半边：恰三键，且 default 与运行态默认值逐值同源 ----
        let schema = module.config_schema();
        let props = schema["properties"]
            .as_object()
            .expect("schema properties 应为对象");
        let mut keys: Vec<&str> = props.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["auto_sync", "conflict_keep_days", "quiet_period_ms"],
            "三键之外不得出现无读者的键"
        );
        let d = SyncConfig::default();
        assert_eq!(
            d,
            SyncConfig {
                auto_sync: false,
                quiet_period_ms: 60_000,
                conflict_keep_days: 30,
            },
            "默认关 + 60 秒静默窗 + 30 天保留窗"
        );
        assert_eq!(props["auto_sync"]["default"].as_bool(), Some(false));
        assert_eq!(
            props["quiet_period_ms"]["default"].as_u64(),
            Some(d.quiet_period_ms)
        );
        assert_eq!(
            props["conflict_keep_days"]["default"].as_u64(),
            Some(d.conflict_keep_days)
        );
        assert_eq!(
            props["quiet_period_ms"]["minimum"].as_u64(),
            Some(QUIET_PERIOD_MIN_MS),
            "写侧下限与 `merged` 读侧下限必须同一个常量"
        );
        assert_eq!(props["conflict_keep_days"]["minimum"].as_u64(), Some(1));

        // ---- 纯语义半边：缺键＝不动运行态（B4 纪律），未知键读侧忽略 ----
        let patched = d.merged(&serde_json::json!({ "auto_sync": true })).unwrap();
        assert!(patched.auto_sync);
        assert_eq!(
            (patched.quiet_period_ms, patched.conflict_keep_days),
            (d.quiet_period_ms, d.conflict_keep_days),
            "没填的键保持现值"
        );
        assert_eq!(
            d.merged(&serde_json::json!({ "auto_sync": false, "an_old_key": 7 }))
                .unwrap(),
            d,
            "未知键（含旧盘上已废弃键的残留）零影响，也不写\"已迁移\""
        );
        // 现值≠默认值时才测得出"不动"：9000/7 现值 ⊕ 只改 auto_sync ⇒ 9000/7 仍在
        let kept = SyncConfig {
            auto_sync: false,
            quiet_period_ms: 9_000,
            conflict_keep_days: 7,
        };
        let after = kept
            .merged(&serde_json::json!({ "auto_sync": true }))
            .unwrap();
        assert_eq!(
            (after.quiet_period_ms, after.conflict_keep_days),
            (9_000, 7)
        );

        // ---- 活通路半边：写 ConfigStore → 派发循环 → status() 不重启即反映新值 ----
        assert!(
            !module.status().unwrap().auto_sync,
            "默认关 ⇒ 本批上线行为零变化"
        );
        store_cfg
            .set_module(
                "sync",
                serde_json::json!({
                    "auto_sync": true,
                    "quiet_period_ms": 9_000,
                    "conflict_keep_days": 7,
                }),
            )
            .unwrap();
        poll_until("auto_sync 未经重启即反映到状态读面", || {
            module.status().unwrap().auto_sync
        })
        .await;

        // 保留窗真消费（10 天前的快照在**缺该键**的派发下仍按运行态 7 天窗被裁：
        // 若"缺键"被当成"回默认 30 天"，这一行会活着）
        let log = OpLog::open(module.db_path()).unwrap();
        seed_conflict_at(&log, "aged10", now_ms() - 10 * DAY_MS);
        assert_eq!(
            log.conflicts(10, 0).unwrap().len(),
            1,
            "先确认它在表里（否则下面的消失说明不了任何事）"
        );
        store_cfg
            .set_module("sync", serde_json::json!({ "auto_sync": false }))
            .unwrap();
        poll_until(
            "缺键派发后旧快照仍按运行态的保留窗被裁",
            || log.conflicts(10, 0).unwrap().is_empty(),
        )
        .await;
        assert!(
            !module.status().unwrap().auto_sync,
            "整段替换语义下 auto_sync 跟着本次写入值翻回关"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 任务书（09 §10.2 T-B5-6）字面测试名：坏值弹回，运行态一个键都不动
    ///
    /// 三层证据：`merged` 逐键拒并点名（消息要自证拒的是什么）、`apply_config` 全有或全无
    /// （同批里的好值也不许偷偷进运行态，且**不得产生裁剪副作用**）、派发循环把同一句真因
    /// 原样交给 UI（`host.config_rejected`）。对齐 B4"缺键/坏值不动运行态"纪律。
    #[tokio::test(flavor = "multi_thread")]
    #[allow(non_snake_case)]
    async fn syncConfig_badValue_rejectedRuntimeKeepsOld() {
        let dir = temp_appdata("cfgbad");
        let bus = Arc::new(EventBus::new());
        let store_cfg = Arc::new(host_core::config::ConfigStore::new(
            dir.join("config"),
            bus.clone(),
        ));
        let module = Arc::new(SyncModule::new(&dir));
        store_cfg.register_schema("sync", module.config_schema());
        let registry = Arc::new(host_core::registry::ModuleRegistry::new(bus.clone()));
        registry.register(module.clone()).unwrap();
        let rx = bus.subscribe("host.config_changed").unwrap();
        tokio::spawn(host_core::registry::run_config_feed(
            rx,
            registry.clone(),
            store_cfg.clone(),
        ));
        let ctx = Arc::new(ModuleContext {
            app_data_dir: dir.clone(),
            ports: Arc::new(host_core::ports::Ports::new()),
            event_bus: bus.clone(),
        });
        for (_, r) in registry.init_all(ctx).await {
            r.expect("init 应成功");
        }
        let log = OpLog::open(module.db_path()).unwrap();
        seed_conflict_at(&log, "aged10", now_ms() - 10 * DAY_MS);

        // ---- 派发侧：一批里混一个坏值 ⇒ 整批弹回（半套配置在跑比配置没生效更难查） ----
        let err = module
            .apply_config(serde_json::json!({
                "auto_sync": true,
                "conflict_keep_days": 5,
                "quiet_period_ms": 100,
            }))
            .expect_err("低于静默窗下限的值必须弹回");
        assert!(
            matches!(err, ModuleError::Config(_)),
            "须走配置拒绝臂，实际 {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("quiet_period_ms"),
            "要点名是哪个键，实际：{msg}"
        );
        assert!(
            msg.contains(&QUIET_PERIOD_MIN_MS.to_string()),
            "要点名下限是多少，实际：{msg}"
        );
        let st = module.status().unwrap();
        assert!(
            !st.auto_sync,
            "同批的 auto_sync:true 不许偷偷进运行态（全有或全无）"
        );
        assert_eq!(
            log.conflicts(10, 0).unwrap().len(),
            1,
            "被拒的保留天数不许已经裁过盘（校验在写盘与裁剪之前）"
        );

        // 另两形坏值：类型错、0 天窗
        let err = module
            .apply_config(serde_json::json!({ "auto_sync": "yes" }))
            .expect_err("字符串不是布尔");
        assert!(err.to_string().contains("auto_sync"), "实际：{err}");
        let err = module
            .apply_config(serde_json::json!({ "conflict_keep_days": 0 }))
            .expect_err("0 天＝每次派发清空全部冲突历史");
        assert!(
            err.to_string().contains("conflict_keep_days"),
            "实际：{err}"
        );
        assert_eq!(log.conflicts(10, 0).unwrap().len(), 1, "两枚坏值之后仍在册");

        // ---- 正对照：同形好值即写即生效，且保留窗真的跟着收紧 ----
        module
            .apply_config(serde_json::json!({ "auto_sync": true, "conflict_keep_days": 5 }))
            .unwrap();
        assert!(module.status().unwrap().auto_sync);
        assert!(
            log.conflicts(10, 0).unwrap().is_empty(),
            "5 天窗下的 10 天快照该被裁（证明上面那个\"仍在册\"不是断言空洞）"
        );

        // ---- 写侧：同一坏值经 ConfigStore 也进不来（schema minimum 与 merged 同源） ----
        store_cfg
            .set_module(
                "sync",
                serde_json::json!({ "auto_sync": true, "conflict_keep_days": 5 }),
            )
            .unwrap();
        let err = store_cfg
            .set_module("sync", serde_json::json!({ "quiet_period_ms": 100 }))
            .expect_err("写侧也该拦");
        assert!(err.to_string().contains("配置校验失败"), "实际：{err}");
        assert_eq!(
            store_cfg.get_module("sync").unwrap()["conflict_keep_days"],
            serde_json::json!(5),
            "写侧拒绝不得留下半截盘（盘上还是上一次的整份好值）"
        );
        assert!(
            module.status().unwrap().auto_sync,
            "写侧拒绝 ⇒ 运行态零扰动"
        );

        // ---- 坏值来自盘（手改/旧版本残留）时，派发拒绝必须出声给 UI，运行态照旧 ----
        let mut rj = bus.subscribe("host.config_rejected").unwrap();
        std::fs::write(
            dir.join("config").join("sync.json"),
            serde_json::json!({ "quiet_period_ms": 1 }).to_string(),
        )
        .unwrap();
        bus.publish(Event::new(
            "host.config_changed",
            "host",
            serde_json::json!({ "module": "sync" }),
        ))
        .unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rj.recv())
            .await
            .expect(
                "派发拒绝必须在 host.config_rejected 里出声（只进日志＝设置看着生效了其实没有）",
            )
            .expect("订阅未关闭");
        assert_eq!(ev.payload["module"], "sync");
        assert!(
            ev.payload["error"]
                .as_str()
                .unwrap_or_default()
                .contains("quiet_period_ms"),
            "UI 拿到的必须是同一句真因，实际 {:?}",
            ev.payload
        );
        assert!(
            module.status().unwrap().auto_sync,
            "被拒的盘值不许改动运行态（此刻内存里仍是上一次的好值）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 任务书（09 §10.2 T-B5-6）字面测试名 · 红线：不擅自开始自动出账
    ///
    /// 两臂同预算、同排程口（`record_change` ⇒ `note_changed`）：关着 ⇒ 零闹钟零流水，
    /// 开着 ⇒ 一闹钟一流水。只有负臂的测试是空洞断言——"没发生"要配一个
    /// "同一套机制本来会让它发生"的对照才成证据。
    ///
    /// 静默窗在运行态上直写 150 ms：5 秒下限本身由 `merged` 与写侧 schema 两层钉住
    /// （见上两枚），本行要证的是**开关门**而不是下限，为它每臂付五秒真实时钟不值得。
    #[tokio::test(flavor = "multi_thread")]
    #[allow(non_snake_case)]
    async fn syncConfig_autoSyncFalse_neverFiresTimer() {
        let (m, store, log, dir) = autosync_module("autoclose").await;
        assert!(
            !m.status().unwrap().auto_sync,
            "新装实例的自动同步必须是关的"
        );
        assert!(!m.has_pending_timer(), "关着 ⇒ 一颗闹钟都不该有");

        // ---- 关臂：三次变更入流，但零出账 ----
        for i in 0..3 {
            let name = format!("c{i}.md");
            store.put(&name, "v");
            m.record_change(ENTITY_NOTE, &name, "write").unwrap();
        }
        assert!(!m.has_pending_timer(), "关着就不该排闹钟");
        assert_eq!(log.count(), 3, "关的是出账，不是记录：变更照样入流");
        assert!(log.runs(10).unwrap().is_empty(), "关着 ⇒ 一账不出");
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert!(
            log.runs(10).unwrap().is_empty(),
            "等过正臂的整窗口仍是零（正对照证明该预算足够显形）"
        );

        // ---- 正对照臂：同一排程口，开关按下去 ⇒ 闹钟真排、轮真跑 ----
        m.apply_config(serde_json::json!({ "auto_sync": true }))
            .unwrap();
        m.auto.config.write().quiet_period_ms = 150;
        store.put("c3.md", "v");
        m.record_change(ENTITY_NOTE, "c3.md", "write").unwrap();
        assert!(
            m.has_pending_timer(),
            "开着 ⇒ 同一排程口确实排了闹钟（否则上面的零什么都证明不了）"
        );
        poll_until("静默窗到期后留下一轮流水", || {
            !log.runs(10).unwrap().is_empty()
        })
        .await;
        assert!(
            !m.has_pending_timer(),
            "闹钟跑完应交还槽位，不留\"还在等\"的假象"
        );
        let rows = log.runs(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].peer, "devB", "流水行要点名是哪台设备");
        assert!(rows[0].error.is_some(), "无地址 ⇒ 如实记失败行，绝不记成功");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 任务书（09 §10.2 T-B5-6）字面测试名：连投只出一轮（静默期合并，14-sync §4 Resilio 借鉴）
    #[tokio::test(flavor = "multi_thread")]
    #[allow(non_snake_case)]
    async fn syncAuto_quietPeriod_coalescesStorm() {
        let (m, store, log, dir) = autosync_module("coalesce").await;
        m.apply_config(serde_json::json!({ "auto_sync": true }))
            .unwrap();
        m.auto.config.write().quiet_period_ms = 150;

        // 风暴：20 条变更跨 ~600 ms（≈4 个静默窗）；逐条排程的实现会跑出 ≥4 轮
        for i in 0..20 {
            let name = format!("s{i}.md");
            store.put(&name, "v");
            m.record_change(ENTITY_NOTE, &name, "write").unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        assert!(
            m.has_pending_timer(),
            "风暴期间始终只留最新那颗闹钟（取消-重排的另一半：旧的必须被掐掉）"
        );
        poll_until("静默窗到期后出账一轮", || {
            !log.runs(10).unwrap().is_empty()
        })
        .await;
        // 再等两个窗口长度：任何"每颗闹钟各跑一轮"的残留实现都会在此期间显形
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let rows = log.runs(10).unwrap();
        assert_eq!(
            rows.len(),
            1,
            "20 条变更应合并成一轮出账，实际 {} 轮",
            rows.len()
        );
        assert_eq!(
            log.count(),
            20,
            "合并的是出账，不是丢变更：20 条全在变更流里"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
