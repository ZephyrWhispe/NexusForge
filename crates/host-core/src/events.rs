//! 事件总线（docs/impl/01 S4，依据 docs/DESIGN.md §6.2 与 §2.2 O8）
//!
//! 规则：
//! - 主题必须先经 [`EventBus::register_topic`] 注册且命中 [`TOPIC_REGISTRY`]，运行期禁止动态造主题；
//! - 每主题独立 broadcast 通道（容量 [`EVENT_CHANNEL_CAP`]），消费端处理慢会收到 `Lagged`，
//!   因此消费端必须"收到事件后拉取最新状态"，不得依赖逐条回放；
//! - 背压策略作为元数据登记在 [`TOPIC_REGISTRY`]（D-03），阈值全仓唯一来源：
//!   - `Merged`：生产端必须走 [`EventBus::publish_merged`]，同 (topic, key) 窗口内末条胜出；
//!     终态用 [`EventBus::flush_merged`] 立即冲刷；
//!   - `Throttled`：消费端可用 [`EventBus::subscribe_throttled`]，首条立即投递、
//!     其后每间隔至多一条（末随取最新）；
//!   - `Batched`：字节流批窗口（term.output），数据是累计拼接而非丢弃式合并，
//!     窗口与单批上限从注册表读取；
//! - 去抖订阅（[`EventBus::subscribe_debounced`]）：同 (topic, payload.key) 在窗口期内
//!   只投递最后一条，UI 消费用。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc};

use crate::codes;
use crate::error::AppError;
use crate::util::now_ms;

/// 每主题通道容量；满后消费端收到 Lagged（背压语义见模块注释）
pub const EVENT_CHANNEL_CAP: usize = 1024;

/// 主题背压策略（D-03）：在 [`TOPIC_REGISTRY`] 声明，作为全仓阈值唯一来源
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackpressurePolicy {
    /// 无背压：逐条发布与投递
    None,
    /// 生产端合并：同 key 在窗口内末条胜出（通知型事件，消费端收事件后拉最新状态）
    Merged { window_ms: u64 },
    /// 消费端节流：每间隔至多投递一条（首条立即）
    Throttled { interval_ms: u64 },
    /// 字节流批合并：window_ms 批窗口 + 单批 max_bytes 上限（数据累计，非丢弃式合并）
    Batched { window_ms: u64, max_bytes: usize },
}

/// 全部主题的编译期注册表：`(topic, 说明, 背压策略)`
pub const TOPIC_REGISTRY: &[(&str, &str, BackpressurePolicy)] = &[
    ("host.module_crashed", "模块 panic 被隔离。payload: {module, message}", BackpressurePolicy::None),
    ("host.config_changed", "模块配置变更。payload: {module}", BackpressurePolicy::None),
    ("host.module_state", "模块状态机迁移。payload: {module, state}", BackpressurePolicy::None),
    ("clipboard.captured", "剪贴板捕获入库（300ms 合并通知，UI 收事件后拉最新列表）。payload: {id, secret, kind?}", BackpressurePolicy::Merged { window_ms: 300 }),
    ("clipboard.deleted", "剪贴板条目删除。payload: {id}", BackpressurePolicy::None),
    ("clipboard.cleared", "剪贴板清空。payload: {removed}", BackpressurePolicy::None),
    ("clipboard.quick_panel_toggled", "快速面板呼出/隐藏请求（全局快捷键触发）。payload: {}", BackpressurePolicy::None),
    ("clipboard.capture_state", "捕获暂停态迁移（托盘/命令切换后重建标签与横幅）。payload: {paused}", BackpressurePolicy::None),
    ("clipboard.stack_changed", "粘贴堆栈成员变更。payload: {depth}", BackpressurePolicy::None),
    ("clipboard.groups_changed", "分组写口成功后的计数刷新。payload: 分组计数对象（与 clipboard_group_counts 同形状）", BackpressurePolicy::Merged { window_ms: 300 }),
    ("screenshot.overlay_requested", "请求呼出截图选区覆盖层（快捷键/OCR 触发）。payload: {mode: shot|ocr}", BackpressurePolicy::None),
    ("screenshot.taken", "截图任务完成。payload: {task_id, file?}", BackpressurePolicy::None),
    ("screenshot.ocr_requested", "截图模块请求 OCR。payload: {task_id, frame_ref}", BackpressurePolicy::None),
    ("ocr.completed", "OCR 完成。payload: {source_task_id?, result}", BackpressurePolicy::None),
    ("ocr.failed", "OCR 失败。payload: {reason}", BackpressurePolicy::None),
    ("operation.conflict", "文件操作同名冲突，等待 UI 应答。payload: {op_id, target}", BackpressurePolicy::None),
    ("operation.progress", "文件操作进度（200ms 合并，key=op_id；终态经 flush_merged 立即冲刷）。payload: {key: op_id, op_id, kind, state, current, files_done, files_total, bytes_done, bytes_total}", BackpressurePolicy::Merged { window_ms: 200 }),
    ("operation.done", "文件操作完成。payload: {op_id, kind}", BackpressurePolicy::None),
    ("operation.failed", "文件操作失败/取消。payload: {op_id, kind, state, error?}", BackpressurePolicy::None),
    ("kvm.peer_online", "键鼠共享发现新设备。payload: {peer}", BackpressurePolicy::None),
    ("kvm.peer_offline", "键鼠共享设备离线。payload: {device_id}", BackpressurePolicy::None),
    ("kvm.session_state", "键鼠共享会话状态迁移。payload: {device_id, state}", BackpressurePolicy::None),
    ("kvm.paired", "键鼠共享配对变更。payload: {peer?|device_id?, paired}", BackpressurePolicy::None),
    ("kvm.clip_received", "键鼠共享收到对端剪贴板。payload: {device_id, content}", BackpressurePolicy::None),
    ("kvm.file_incoming", "键鼠共享文件传输建档。payload: {device_id, transfer_id, name, size, received, total_chunks}", BackpressurePolicy::None),
    ("kvm.file_received", "键鼠共享文件接收完成（SHA256 校验通过）。payload: {device_id, transfer_id, name, path}", BackpressurePolicy::None),
    ("kvm.file_progress", "键鼠共享文件发送进度（200ms 合并，key=transfer_id）。payload: {transfer_id, sent_chunks, total_chunks}", BackpressurePolicy::Merged { window_ms: 200 }),
    ("kvm.transfer_ack", "键鼠共享传输回执。payload: {transfer_id, ok, error?}", BackpressurePolicy::None),
    ("kvm.control_state", "键鼠共享控制权迁移。payload: {role, device_id?, by?, reason?}", BackpressurePolicy::None),
    ("vault.state_changed", "密码库锁定状态迁移。payload: {state: uninitialized|locked|unlocked}", BackpressurePolicy::None),
    ("vault.entries_changed", "密码库条目/文件夹变更。payload: {action, id?}", BackpressurePolicy::None),
    ("vault.auto_lock_warning", "自动锁定预警（D-24 V5：锁定前 30s）。payload: {lock_in_secs}", BackpressurePolicy::None),
    ("proxy.state_changed", "代理模式/内核状态迁移。payload: {mode, kernel_running, kernel_id?, inbound_port?}", BackpressurePolicy::None),
    ("proxy.log_line", "代理内核日志行。payload: {line}", BackpressurePolicy::None),
    ("proxy.nodes_changed", "订阅/节点列表变更。payload: {sub_id?, total}", BackpressurePolicy::None),
    ("desktop.launcher_toggled", "快速启动器呼出/隐藏请求（全局快捷键触发）。payload: {}", BackpressurePolicy::None),
    ("desktop.note_quick", "快速速记条呼出请求（全局快捷键触发）。payload: {}", BackpressurePolicy::None),
    ("desktop.remind_due", "随记提醒到期。payload: {id, content, remind_at, tags}", BackpressurePolicy::None),
    ("notes.changed", "笔记库变更（创建/写入/删除/重命名/索引同步/卡片/画布）。payload: {action, path?}", BackpressurePolicy::None),
    ("term.output", "终端输出批处理（8ms 窗口合并，单批 ≤ 64KB；内容敏感不进日志）。payload: {session_id, data, seq}", BackpressurePolicy::Batched { window_ms: 8, max_bytes: 64 * 1024 }),
    ("term.exit", "终端会话结束。payload: {session_id, code?}", BackpressurePolicy::None),
    ("sys.metrics", "系统资源采样（1s 合并节流）。payload: {ts_ms, cpu, mem_used, mem_total, net_bps, disks}", BackpressurePolicy::Merged { window_ms: 1000 }),
    ("sys.pkg_line", "包管理器命令输出行。payload: {source, action, line}", BackpressurePolicy::None),
    ("sys.verify_result", "WinOps 回归检测（WUB 式防自愈，模块 start 时比对备份原值）。payload: {regressed: [tweak_id]}", BackpressurePolicy::None),
    ("automation.notify", "自动化规则前端通知。payload: {rule_id, title, body}", BackpressurePolicy::None),
    ("automation.rule_fired", "规则已触发。payload: {rule_id, rule_name}", BackpressurePolicy::None),
    ("sync.state_changed", "跨设备同步状态。payload: {pushed, pulled_applied, pulled_lost, conflicts}", BackpressurePolicy::None),
    ("sync.conflict", "同步冲突（LWW 本地胜出被覆盖/对端胜出）。payload: {entity, entity_id, winner_device, ts}", BackpressurePolicy::None),
];

/// 按名取静态主题（仅编译期登记主题可发布——总线契约；模块层发布入口）
pub fn topic(name: &str) -> Option<&'static str> {
    TOPIC_REGISTRY
        .iter()
        .find(|(t, ..)| *t == name)
        .map(|(t, ..)| *t)
}

/// 查询主题背压策略（未登记主题按 [`BackpressurePolicy::None`]）
pub fn backpressure_policy(topic: &str) -> BackpressurePolicy {
    TOPIC_REGISTRY
        .iter()
        .find(|(t, ..)| *t == topic)
        .map(|(.., p)| *p)
        .unwrap_or(BackpressurePolicy::None)
}

/// 主题登记的合并/批窗口时长（`Merged`/`Batched` 策略）；非合并策略返回 ZERO
pub fn merged_window(topic: &str) -> std::time::Duration {
    match backpressure_policy(topic) {
        BackpressurePolicy::Merged { window_ms }
        | BackpressurePolicy::Batched { window_ms, .. } => {
            std::time::Duration::from_millis(window_ms)
        }
        _ => std::time::Duration::ZERO,
    }
}

/// 统一事件信封（前端收到格式与此一致，M1 冻结契约）
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub topic: &'static str,
    pub source: &'static str,
    pub payload: serde_json::Value,
    /// 毫秒时间戳
    pub ts: i64,
}

impl Event {
    pub fn new(topic: &'static str, source: &'static str, payload: serde_json::Value) -> Self {
        Self {
            topic,
            source,
            payload,
            ts: now_ms(),
        }
    }
}

/// publish_merged 的每主题待冲刷槽位：key → 窗口内最新事件
struct MergeSlot {
    pending: Mutex<HashMap<String, Event>>,
}

pub struct EventBus {
    channels: RwLock<HashMap<&'static str, broadcast::Sender<Event>>>,
    /// 合并发布守护（D-03）：每主题一个，窗口节拍冲刷 last-wins 快照；
    /// 线程持 Weak 引用总线，总线释放后自行退出
    mergers: RwLock<HashMap<&'static str, Arc<MergeSlot>>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// 创建总线并自动注册全部 `host.*` 宿主主题
    pub fn new() -> Self {
        let bus = Self {
            channels: RwLock::new(HashMap::new()),
            mergers: RwLock::new(HashMap::new()),
        };
        // 全部编译期登记主题一次性注册：修复此前仅注册 host.* 导致
        // 模块事件（clipboard.* / screenshot.* / ocr.*）无法发布与转发的问题
        for (topic, ..) in TOPIC_REGISTRY {
            let _ = bus.register_topic(topic);
        }
        bus
    }

    /// 注册主题；必须命中 [`TOPIC_REGISTRY`] 且未重复注册
    pub fn register_topic(&self, topic: &'static str) -> Result<(), AppError> {
        if !TOPIC_REGISTRY.iter().any(|(t, ..)| *t == topic) {
            return Err(AppError::module(
                codes::host::HOST_EVENT_001,
                format!("主题 {topic} 未在 TOPIC_REGISTRY 登记"),
                Some("在 host-core/src/events.rs 的 TOPIC_REGISTRY 中补充该主题"),
            ));
        }
        let mut channels = self.channels.write();
        if channels.contains_key(topic) {
            return Ok(()); // 幂等
        }
        let (tx, _rx) = broadcast::channel(EVENT_CHANNEL_CAP);
        channels.insert(topic, tx);
        Ok(())
    }

    /// 发布事件。无订阅者时静默成功；通道满由消费端以 Lagged 感知（背压策略）
    pub fn publish(&self, event: Event) -> Result<(), AppError> {
        let tx = {
            let guard = self.channels.read();
            guard.get(event.topic).cloned()
        }
        .ok_or_else(|| {
            AppError::module(
                codes::host::HOST_EVENT_001,
                format!("主题 {} 未注册，拒绝发布", event.topic),
                None,
            )
        })?;
        // SendError 仅表示无订阅者，属正常情况
        let _ = tx.send(event);
        Ok(())
    }

    /// 合并发布（D-03，生产端背压统一入口）：同 (topic, key) 在 `window` 内
    /// 末条胜出，后台合并线程每窗口冲刷一次（首条至多延迟一个窗口）。
    /// 普通线程即可调用（不依赖 tokio 运行时）；window 通常取
    /// [`merged_window(topic)`]，使阈值保持注册表唯一来源；window 为零退化为直发。
    pub fn publish_merged(
        self: &Arc<Self>,
        event: Event,
        key: &str,
        window: std::time::Duration,
    ) -> Result<(), AppError> {
        if window.is_zero() {
            return self.publish(event);
        }
        let slot = self.ensure_merger(event.topic, window)?;
        slot.pending.lock().insert(key.to_owned(), event);
        Ok(())
    }

    /// 立即冲刷 (topic, key) 的待合并事件（终态语义：状态迁移不得等窗口，
    /// 冲刷后同一窗口内不会再有旧快照迟到）
    pub fn flush_merged(&self, topic: &str, key: &str) {
        let slot = self.mergers.read().get(topic).cloned();
        let Some(slot) = slot else { return };
        let event = slot.pending.lock().remove(key);
        if let Some(event) = event {
            let _ = self.publish(event);
        }
    }

    /// 取（或惰性建立）主题合并守护；首建者决定窗口，后续调用沿用
    fn ensure_merger(
        self: &Arc<Self>,
        topic: &'static str,
        window: std::time::Duration,
    ) -> Result<Arc<MergeSlot>, AppError> {
        if let Some(slot) = self.mergers.read().get(topic) {
            return Ok(slot.clone());
        }
        let mut guard = self.mergers.write();
        if let Some(slot) = guard.get(topic) {
            return Ok(slot.clone());
        }
        if !self.channels.read().contains_key(topic) {
            return Err(AppError::module(
                codes::host::HOST_EVENT_001,
                format!("主题 {topic} 未注册，拒绝合并发布"),
                None,
            ));
        }
        let slot = Arc::new(MergeSlot {
            pending: Mutex::new(HashMap::new()),
        });
        guard.insert(topic, slot.clone());
        drop(guard);
        let weak = Arc::downgrade(self);
        let daemon_slot = slot.clone();
        std::thread::Builder::new()
            .name(format!("nf-bus-merge-{topic}"))
            .spawn(move || loop {
                std::thread::sleep(window);
                let drained: Vec<Event> = {
                    let mut g = daemon_slot.pending.lock();
                    if g.is_empty() {
                        continue;
                    }
                    g.drain().map(|(_, ev)| ev).collect()
                };
                let Some(bus) = weak.upgrade() else {
                    return; // 总线已释放：进程收尾，丢弃在途快照
                };
                let mut drained = drained;
                drained.sort_by_key(|ev| ev.ts);
                for ev in drained {
                    let _ = bus.publish(ev);
                }
            })
            .map_err(|e| {
                AppError::module(
                    codes::host::HOST_EVENT_001,
                    format!("合并守护线程启动失败: {e}"),
                    None,
                )
            })?;
        Ok(slot)
    }

    /// 普通订阅：逐条投递；消费端必须处理 `RecvError::Lagged`（重新拉取最新状态）
    pub fn subscribe(&self, topic: &str) -> Result<broadcast::Receiver<Event>, AppError> {
        let tx = {
            let guard = self.channels.read();
            guard.get(topic).cloned()
        }
        .ok_or_else(|| {
            AppError::module(
                codes::host::HOST_EVENT_001,
                format!("主题 {topic} 未注册，无法订阅"),
                None,
            )
        })?;
        Ok(tx.subscribe())
    }

    /// 去抖订阅：同 (topic, payload["key"]) 在 window 内只投递最后一条。
    /// payload 无 "key" 字段时使用固定键 "default"。
    /// 必须在 tokio 运行时内调用；返回的接收器供单消费者使用。
    pub fn subscribe_debounced(
        &self,
        topic: &str,
        window: std::time::Duration,
    ) -> Result<DebouncedReceiver, AppError> {
        let mut rx = self.subscribe(topic)?;
        let (tx, out) = mpsc::channel::<Event>(16);
        tokio::spawn(async move {
            let mut pending: HashMap<String, (Event, tokio::time::Instant)> = HashMap::new();
            loop {
                if pending.is_empty() {
                    match rx.recv().await {
                        Ok(ev) => insert_pending(&mut pending, ev, window),
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    }
                    continue;
                }
                // 计算最近的截止时刻
                let next_deadline = pending
                    .values()
                    .map(|(_, dl)| *dl)
                    .min()
                    .expect("非空集合必有最小值");
                tokio::select! {
                    ev = rx.recv() => {
                        match ev {
                            Ok(ev) => insert_pending(&mut pending, ev, window),
                            Err(broadcast::error::RecvError::Closed) => break,
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        }
                    }
                    _ = tokio::time::sleep_until(next_deadline) => {
                        let now = tokio::time::Instant::now();
                        let expired: Vec<String> = pending
                            .iter()
                            .filter(|(_, (_, dl))| *dl <= now)
                            .map(|(k, _)| k.clone())
                            .collect();
                        for k in expired {
                            if let Some((ev, _)) = pending.remove(&k) {
                                if tx.send(ev).await.is_err() {
                                    return; // 消费端已丢弃，结束去抖任务
                                }
                            }
                        }
                    }
                }
            }
        });
        Ok(DebouncedReceiver { rx: out })
    }

    /// 节流订阅（D-03，消费端背压统一入口）：首条立即投递（前沿），
    /// 其后 `interval` 窗口内的后续事件合并为窗口末尾的最新一条（后随）。
    /// 必须在 tokio 运行时内调用；返回的接收器供单消费者使用。
    pub fn subscribe_throttled(
        &self,
        topic: &str,
        interval: std::time::Duration,
    ) -> Result<ThrottledReceiver, AppError> {
        let mut rx = self.subscribe(topic)?;
        let (tx, out) = mpsc::channel::<Event>(16);
        tokio::spawn(async move {
            let mut gate = tokio::time::Instant::now();
            let mut pending: Option<Event> = None;
            loop {
                tokio::select! {
                    ev = rx.recv() => match ev {
                        Ok(ev) => {
                            let now = tokio::time::Instant::now();
                            if now < gate {
                                pending = Some(ev);
                            } else if let Some(last) = pending.take() {
                                // 窗口到期且有积压：先补发积压的最新一条，本条继续排队
                                gate = now + interval;
                                if tx.send(last).await.is_err() {
                                    return;
                                }
                                pending = Some(ev);
                            } else {
                                gate = now + interval;
                                if tx.send(ev).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    },
                    _ = tokio::time::sleep_until(gate), if pending.is_some() => {
                        let ev = pending.take().expect("select 分支守卫保证有积压");
                        gate = tokio::time::Instant::now() + interval;
                        if tx.send(ev).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(ThrottledReceiver { rx: out })
    }
}

fn insert_pending(
    pending: &mut HashMap<String, (Event, tokio::time::Instant)>,
    ev: Event,
    window: std::time::Duration,
) {
    let key = ev
        .payload
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_owned();
    pending.insert(key, (ev, tokio::time::Instant::now() + window));
}

/// 去抖订阅的接收端
pub struct DebouncedReceiver {
    rx: mpsc::Receiver<Event>,
}

impl DebouncedReceiver {
    pub async fn recv(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}

/// 节流订阅的接收端
pub struct ThrottledReceiver {
    rx: mpsc::Receiver<Event>,
}

impl ThrottledReceiver {
    pub async fn recv(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}

/// 订阅句柄守卫（预留：drop 时移除订阅登记；当前 broadcast 自行清理）
pub type Subscription = broadcast::Receiver<Event>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn payload_with_key(key: &str, v: u32) -> serde_json::Value {
        json!({ "key": key, "seq": v })
    }

    #[test]
    fn register_rejects_unknown_topic() {
        let bus = EventBus::new();
        let err = bus.register_topic("evil.dynamic");
        assert!(err.is_err());
        assert_eq!(err.unwrap_err().code(), codes::host::HOST_EVENT_001);
    }

    #[test]
    fn publish_subscribe_roundtrip() {
        let bus = Arc::new(EventBus::new());
        bus.register_topic("clipboard.captured").unwrap();
        bus.register_topic("clipboard.cleared").unwrap();
        bus.subscribe("clipboard.captured").unwrap();
        bus.publish(Event::new(
            "clipboard.captured",
            "clipboard",
            json!({ "id": "e1" }),
        ))
        .unwrap();
        // 无订阅者也允许发布（广播空投）
        bus.publish(Event::new("clipboard.cleared", "clipboard", json!(3)))
            .unwrap();
    }

    #[test]
    fn publish_unregistered_fails() {
        let bus = EventBus::new();
        let err = bus.publish(Event::new("not.a.topic", "x", json!(null)));
        assert_eq!(err.unwrap_err().code(), codes::host::HOST_EVENT_001);
    }

    /// D-15 回归：std 锁持锁 panic 会 poison → 之后每次 `.write().unwrap()` 级联
    /// panic，一次偶发毒化即令全模块发布瘫痪；parking_lot 无 poison 语义，
    /// 持写锁线程panic后总线必须照常发布/订阅。
    #[test]
    fn publish_survives_panic_under_lock() {
        let bus = Arc::new(EventBus::new());
        let b = bus.clone();
        let h = std::thread::spawn(move || {
            let _g = b.channels.write();
            panic!("持锁 panic");
        });
        assert!(h.join().is_err(), "前置：线程必须真的 panic 在持锁期间");
        bus.subscribe("clipboard.captured").unwrap();
        bus.publish(Event::new(
            "clipboard.captured",
            "clipboard",
            json!({ "id": "after-poison" }),
        ))
        .unwrap();
    }

    #[tokio::test]
    async fn backpressure_lagged_not_fatal() {
        let bus = Arc::new(EventBus::new());
        bus.register_topic("clipboard.captured").unwrap();
        let mut rx = bus.subscribe("clipboard.captured").unwrap();
        // 压满通道（1024）再溢出若干条
        for i in 0..(EVENT_CHANNEL_CAP + 8) {
            bus.publish(Event::new("clipboard.captured", "clipboard", json!(i)))
                .unwrap();
        }
        // 持续接收最终遇到 Lagged，但通道仍然可用
        let mut lagged = false;
        let mut received = 0;
        loop {
            match rx.try_recv() {
                Ok(_) => received += 1,
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    lagged = true;
                    break;
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => break,
            }
        }
        assert!(lagged);
        assert!(received < EVENT_CHANNEL_CAP + 8);
    }

    #[tokio::test]
    async fn debounce_merges_burst_to_last() {
        let bus = Arc::new(EventBus::new());
        bus.register_topic("clipboard.captured").unwrap();
        let mut rx = bus
            .subscribe_debounced("clipboard.captured", std::time::Duration::from_millis(50))
            .unwrap();
        for i in 0..5 {
            bus.publish(Event::new(
                "clipboard.captured",
                "clipboard",
                payload_with_key("ui", i),
            ))
            .unwrap();
        }
        // 不同 key 不合并
        bus.publish(Event::new(
            "clipboard.captured",
            "clipboard",
            payload_with_key("bg", 100),
        ))
        .unwrap();

        // 两个键的截止时刻几乎同时（仅差发布间隔的微秒级），刷新顺序不保证——
        // 断言与顺序无关：{ui 窗口内 5 条合并为最后一条 4} + {bg 单条 100}
        let first = rx.recv().await.expect("第一波");
        let second = rx.recv().await.expect("第二波");
        let mut seqs = vec![
            first.payload["seq"].as_u64().unwrap(),
            second.payload["seq"].as_u64().unwrap(),
        ];
        seqs.sort_unstable();
        assert_eq!(seqs, vec![4, 100]);
    }

    #[tokio::test]
    async fn debounce_empty_window_passthrough() {
        let bus = Arc::new(EventBus::new());
        let mut rx = bus
            .subscribe_debounced("host.module_state", std::time::Duration::from_millis(10))
            .unwrap();
        bus.publish(Event::new(
            "host.module_state",
            "host",
            json!({"key": "m1", "state": "Running"}),
        ))
        .unwrap();
        let ev = rx.recv().await.expect("单条事件应透传");
        assert_eq!(ev.payload["state"], "Running");
    }

    // ------------------------------------------------------------------
    // D-03：统一背压 API
    // ------------------------------------------------------------------

    /// O8 规范值冻结：剪贴板 300ms 合并、监控 1s 节流、终端 8ms/64KB 批、进度 200ms
    #[test]
    fn topic_policies_match_o8_spec() {
        assert_eq!(
            backpressure_policy("clipboard.captured"),
            BackpressurePolicy::Merged { window_ms: 300 }
        );
        assert_eq!(
            backpressure_policy("sys.metrics"),
            BackpressurePolicy::Merged { window_ms: 1000 }
        );
        assert_eq!(
            backpressure_policy("operation.progress"),
            BackpressurePolicy::Merged { window_ms: 200 }
        );
        assert_eq!(
            backpressure_policy("term.output"),
            BackpressurePolicy::Batched {
                window_ms: 8,
                max_bytes: 64 * 1024
            }
        );
        assert_eq!(
            backpressure_policy("host.config_changed"),
            BackpressurePolicy::None
        );
        assert_eq!(
            merged_window("clipboard.captured"),
            Duration::from_millis(300)
        );
        assert_eq!(merged_window("host.config_changed"), Duration::ZERO);
    }

    /// D-03 验收：1s 内发布 1000 条（发布本身远快于 1s）→ 订阅者收到 1 条末值
    #[tokio::test]
    async fn publish_merged_thousand_burst_delivers_one() {
        let bus = Arc::new(EventBus::new());
        let mut rx = bus.subscribe("clipboard.captured").unwrap();
        for i in 0..1000u32 {
            bus.publish_merged(
                Event::new("clipboard.captured", "clipboard", payload_with_key("k", i)),
                "k",
                Duration::from_millis(200),
            )
            .unwrap();
        }
        let ev = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("窗口末应收到 1 条")
            .unwrap();
        assert_eq!(ev.payload["seq"], 999, "末条胜出");
        // 冲刷后无心跳、无补发
        assert!(
            tokio::time::timeout(Duration::from_millis(400), rx.recv())
                .await
                .is_err(),
            "1000 条突发应恰合并为 1 条"
        );
    }

    #[tokio::test]
    async fn publish_merged_keeps_distinct_keys() {
        let bus = Arc::new(EventBus::new());
        let mut rx = bus.subscribe("operation.progress").unwrap();
        bus.publish_merged(
            Event::new("operation.progress", "file", payload_with_key("op-a", 1)),
            "op-a",
            Duration::from_millis(100),
        )
        .unwrap();
        bus.publish_merged(
            Event::new("operation.progress", "file", payload_with_key("op-b", 2)),
            "op-b",
            Duration::from_millis(100),
        )
        .unwrap();
        let mut seqs = Vec::new();
        for _ in 0..2 {
            let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            seqs.push(ev.payload["seq"].as_u64().unwrap());
        }
        seqs.sort_unstable();
        assert_eq!(seqs, vec![1, 2]);
    }

    #[tokio::test]
    async fn flush_merged_publishes_immediately() {
        let bus = Arc::new(EventBus::new());
        let mut rx = bus.subscribe("operation.progress").unwrap();
        // 窗口取 60s：守护不可能到点，只有 flush 能送达
        bus.publish_merged(
            Event::new("operation.progress", "file", payload_with_key("op-1", 7)),
            "op-1",
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(rx.try_recv().is_err(), "未冲刷前不得投递");
        bus.flush_merged("operation.progress", "op-1");
        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("flush 应立即送达")
            .unwrap();
        assert_eq!(ev.payload["seq"], 7);
        // 冲刷后槽位已空：守护线程之后不会再补发同一快照
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn publish_merged_from_plain_thread_and_rejects_unregistered() {
        // 生产端多为普通线程（clipboard worker / sys sampler / file worker）：
        // publish_merged 不得依赖 tokio 运行时
        let bus = Arc::new(EventBus::new());
        let err = {
            let b = bus.clone();
            std::thread::spawn(move || {
                b.publish_merged(
                    Event::new("evil.dynamic", "x", json!({"key": "k"})),
                    "k",
                    Duration::from_millis(50),
                )
            })
            .join()
            .unwrap()
            .unwrap_err()
        };
        assert_eq!(err.code(), codes::host::HOST_EVENT_001);
    }

    #[tokio::test(start_paused = true)]
    async fn subscribe_throttled_leading_and_trailing() {
        let bus = Arc::new(EventBus::new());
        let mut rx = bus
            .subscribe_throttled("clipboard.captured", Duration::from_secs(1))
            .unwrap();
        bus.publish(Event::new(
            "clipboard.captured",
            "clipboard",
            payload_with_key("k", 0),
        ))
        .unwrap();
        assert_eq!(rx.recv().await.unwrap().payload["seq"], 0);
        for i in 1..50 {
            bus.publish(Event::new(
                "clipboard.captured",
                "clipboard",
                payload_with_key("k", i),
            ))
            .unwrap();
        }
        // start_paused 下 await 自动推进虚拟时间：窗口末尾应补发积压的最新一条
        let ev = rx.recv().await.expect("窗口末应补发");
        assert_eq!(ev.payload["seq"], 49);
    }
}
