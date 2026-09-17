//! 事件总线（docs/impl/01 S4，依据 docs/DESIGN.md §6.2）
//!
//! 规则：
//! - 主题必须先经 [`EventBus::register_topic`] 注册且命中 [`TOPIC_REGISTRY`]，运行期禁止动态造主题；
//! - 每主题独立 broadcast 通道（容量 [`EVENT_CHANNEL_CAP`]），消费端处理慢会收到 `Lagged`，
//!   因此消费端必须"收到事件后拉取最新状态"，不得依赖逐条回放；
//! - 去抖订阅：同 (topic, payload.key) 在窗口期内只投递最后一条，UI 消费用。

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::{broadcast, mpsc};

use crate::codes;
use crate::error::AppError;

/// 每主题通道容量；满后消费端收到 Lagged（背压语义见模块注释）
pub const EVENT_CHANNEL_CAP: usize = 1024;

/// 全部主题的编译期注册表：`(topic, 说明)`
pub const TOPIC_REGISTRY: &[(&str, &str)] = &[
    ("host.module_crashed", "模块 panic 被隔离。payload: {module, message}"),
    ("host.config_changed", "模块配置变更。payload: {module}"),
    ("host.module_state", "模块状态机迁移。payload: {module, state}"),
    ("clipboard.captured", "剪贴板捕获入库。payload: {entry}"),
    ("clipboard.deleted", "剪贴板条目删除。payload: {id}"),
    ("clipboard.cleared", "剪贴板清空。payload: {removed}"),
    ("clipboard.quick_panel_toggled", "快速面板呼出/隐藏请求（全局快捷键触发）。payload: {}"),
    ("screenshot.overlay_requested", "请求呼出截图选区覆盖层（快捷键/OCR 触发）。payload: {mode: shot|ocr}"),
    ("screenshot.taken", "截图任务完成。payload: {task_id, file?}"),
    ("screenshot.ocr_requested", "截图模块请求 OCR。payload: {task_id, frame_ref}"),
    ("ocr.completed", "OCR 完成。payload: {source_task_id?, result}"),
    ("ocr.failed", "OCR 失败。payload: {reason}"),
    ("operation.conflict", "文件操作同名冲突，等待 UI 应答。payload: {op_id, target}"),
    ("operation.progress", "文件操作进度（200ms 节流）。payload: {key: op_id, op_id, kind, state, current, files_done, files_total, bytes_done, bytes_total}"),
    ("operation.done", "文件操作完成。payload: {op_id, kind}"),
    ("operation.failed", "文件操作失败/取消。payload: {op_id, kind, state, error?}"),
    ("kvm.peer_online", "键鼠共享发现新设备。payload: {peer}"),
    ("kvm.peer_offline", "键鼠共享设备离线。payload: {device_id}"),
    ("kvm.session_state", "键鼠共享会话状态迁移。payload: {device_id, state}"),
    ("kvm.paired", "键鼠共享配对变更。payload: {peer?|device_id?, paired}"),
    ("kvm.clip_received", "键鼠共享收到对端剪贴板。payload: {device_id, content}"),
    ("kvm.file_incoming", "键鼠共享文件传输建档。payload: {device_id, transfer_id, name, size, received, total_chunks}"),
    ("kvm.file_received", "键鼠共享文件接收完成（SHA256 校验通过）。payload: {device_id, transfer_id, name, path}"),
    ("kvm.file_progress", "键鼠共享文件发送进度。payload: {transfer_id, sent_chunks, total_chunks}"),
    ("kvm.transfer_ack", "键鼠共享传输回执。payload: {transfer_id, ok, error?}"),
    ("kvm.control_state", "键鼠共享控制权迁移。payload: {role, device_id?, by?, reason?}"),
    ("vault.state_changed", "密码库锁定状态迁移。payload: {state: uninitialized|locked|unlocked}"),
    ("vault.entries_changed", "密码库条目/文件夹变更。payload: {action, id?}"),
    ("proxy.state_changed", "代理模式/内核状态迁移。payload: {mode, kernel_running, kernel_id?, inbound_port?}"),
    ("proxy.log_line", "代理内核日志行。payload: {line}"),
    ("proxy.nodes_changed", "订阅/节点列表变更。payload: {sub_id?, total}"),
    ("desktop.launcher_toggled", "快速启动器呼出/隐藏请求（全局快捷键触发）。payload: {}"),
    ("desktop.note_quick", "快速速记条呼出请求（全局快捷键触发）。payload: {}"),
    ("desktop.remind_due", "随记提醒到期。payload: {id, content, remind_at, tags}"),
    ("notes.changed", "笔记库变更（创建/写入/删除/重命名/索引同步/卡片/画布）。payload: {action, path?}"),
    ("term.output", "终端输出批处理（8ms 窗口合并，单批 ≤ 64KB；内容敏感不进日志）。payload: {session_id, data, seq}"),
    ("term.exit", "终端会话结束。payload: {session_id, code?}"),
    ("sys.metrics", "系统资源采样（1s 节流）。payload: {ts_ms, cpu, mem_used, mem_total, net_bps, disks}"),
    ("sys.pkg_line", "包管理器命令输出行。payload: {source, action, line}"),
];

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

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub struct EventBus {
    channels: RwLock<HashMap<&'static str, broadcast::Sender<Event>>>,
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
        };
        // 全部编译期登记主题一次性注册：修复此前仅注册 host.* 导致
        // 模块事件（clipboard.* / screenshot.* / ocr.*）无法发布与转发的问题
        for (topic, _) in TOPIC_REGISTRY {
            let _ = bus.register_topic(topic);
        }
        bus
    }

    /// 注册主题；必须命中 [`TOPIC_REGISTRY`] 且未重复注册
    pub fn register_topic(&self, topic: &'static str) -> Result<(), AppError> {
        if !TOPIC_REGISTRY.iter().any(|(t, _)| *t == topic) {
            return Err(AppError::module(
                codes::host::HOST_EVENT_001,
                format!("主题 {topic} 未在 TOPIC_REGISTRY 登记"),
                Some("在 host-core/src/events.rs 的 TOPIC_REGISTRY 中补充该主题"),
            ));
        }
        let mut channels = self.channels.write().expect("事件总线写锁");
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
            let guard = self.channels.read().expect("事件总线读锁");
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

    /// 普通订阅：逐条投递；消费端必须处理 `RecvError::Lagged`（重新拉取最新状态）
    pub fn subscribe(&self, topic: &str) -> Result<broadcast::Receiver<Event>, AppError> {
        let tx = {
            let guard = self.channels.read().expect("事件总线读锁");
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
    pub fn subscribe_debounced(&self, topic: &str, window: std::time::Duration) -> Result<DebouncedReceiver, AppError> {
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

/// 订阅句柄守卫（预留：drop 时移除订阅登记；当前 broadcast 自行清理）
pub type Subscription = broadcast::Receiver<Event>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

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
        bus.publish(Event::new("host.module_state", "host", json!({"key": "m1", "state": "Running"})))
            .unwrap();
        let ev = rx.recv().await.expect("单条事件应透传");
        assert_eq!(ev.payload["state"], "Running");
    }
}
