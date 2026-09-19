//! C3 捕获管线（docs/impl/02 C3）
//!
//! Port 回调线程只入队；worker（spawn_blocking 常驻）执行：
//! 过滤(黑名单) → secret 检测 → 分类 → 加密/入库 → 发事件。
//! 回写窗口（500ms）内到达的读取事件直接丢弃（防剪贴板循环）。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use host_core::error::AppError;
use host_core::events::{merged_window, Event, EventBus};
use host_core::ports::{ClipboardPort, CryptoPort};
use tokio::sync::Mutex as AsyncMutex;

use crate::classifier::Classifier;
use crate::secrets::{global as secrets, SecretKind};
use crate::store::ClipStore;
use crate::types::ClipboardConfig;

/// 回写防循环窗口（docs/impl/02 C3 ③）
pub const WRITE_BACK_WINDOW: Duration = Duration::from_millis(500);

/// worker 停机检查节拍：无事件时每 100ms 复查取消标志
const WORKER_TICK: Duration = Duration::from_millis(100);

/// 入库批窗口（docs/impl/02 C3 ⑦）：合并 DB 写入批次的调度参数，
/// 与事件背压无关（事件合并统一走 EventBus::publish_merged，D-03）
const DB_BATCH_WINDOW: Duration = Duration::from_millis(50);

pub struct CapturePipeline {
    port: Arc<dyn ClipboardPort>,
    store: Arc<ClipStore>,
    bus: Arc<EventBus>,
    crypto: Arc<dyn CryptoPort>,
    config: Arc<AsyncMutex<ClipboardConfig>>,
    write_back_at: Arc<parking_lot::Mutex<Option<Instant>>>,
    insert_counter: std::sync::atomic::AtomicU32,
}

/// 管线运行句柄（S3）：`shutdown` 置取消位使 worker 线程退出并释放 store，
/// `live_workers` 供停机/重启回归测试观测存活 worker 数不随重启累积。
pub struct PipelineHandle {
    cancel: Arc<AtomicBool>,
    live_workers: Arc<AtomicUsize>,
    pipeline: Arc<CapturePipeline>,
}

impl PipelineHandle {
    /// 请求停机：worker 在一个 tick 内退出并释放其持有的 Arc<ClipStore>
    pub fn shutdown(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// 当前存活 worker 线程数
    pub fn live_workers(&self) -> usize {
        self.live_workers.load(Ordering::SeqCst)
    }

    /// 等待 worker 全部退出（用于停机回归断言）；超时返回 false。
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.live_workers() > 0 {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    /// 管线共享句柄（D-25：kvm.clip_received 消费协程经此调 ingest_remote）
    pub fn pipeline(&self) -> Arc<CapturePipeline> {
        self.pipeline.clone()
    }
}

impl CapturePipeline {
    /// 启动管线。`write_back` 标志由调用方（模块）创建并持有——
    /// 模块在回写剪贴板前置位，管线回调据此丢弃自回写事件（防循环）。
    /// 返回运行句柄（S3）：stop 时 shutdown 使 worker 线程退出并释放 store，
    /// 修复旧实现每次重启泄漏一个常驻线程 + 一份 DB 连接。
    pub fn start(
        port: Arc<dyn ClipboardPort>,
        store: Arc<ClipStore>,
        bus: Arc<EventBus>,
        crypto: Arc<dyn CryptoPort>,
        config: Arc<AsyncMutex<ClipboardConfig>>,
        write_back: Arc<parking_lot::Mutex<Option<Instant>>>,
    ) -> Result<PipelineHandle, AppError> {
        let (tx, rx) = mpsc::channel::<(host_core::ports::ClipContent, Option<String>)>();
        let pipeline = Arc::new(Self {
            port: port.clone(),
            store,
            bus,
            crypto,
            config,
            write_back_at: write_back,
            insert_counter: std::sync::atomic::AtomicU32::new(0),
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let live = Arc::new(AtomicUsize::new(0));

        // Port 回调：只入队（消息循环线程禁长阻塞）
        let tx_cb = tx.clone();
        let wb = pipeline.write_back_at.clone();
        let cancel_cb = cancel.clone();
        port.start_listener(Box::new(move |content, source_app| {
            // 停机后到达的事件直接丢弃（端口收尾会释放回调，此为在途兜底）
            if cancel_cb.load(Ordering::SeqCst) {
                return;
            }
            // 回写窗口内的事件丢弃（自回写会再次触发 WM_CLIPBOARDUPDATE）
            let in_window = (*wb.lock())
                .map(|t| t.elapsed() < WRITE_BACK_WINDOW)
                .unwrap_or(false);
            if in_window {
                return;
            }
            let _ = tx_cb.send((content, source_app));
        })
            as Box<dyn Fn(host_core::ports::ClipContent, Option<String>) + Send + Sync>)?;

        // worker：批量落库；cancel 置位后一个 WORKER_TICK 内退出，释放持有的 Arc<ClipStore>
        std::thread::Builder::new()
            .name("clipboard-pipeline".into())
            .spawn({
                let pipeline = pipeline.clone();
                let cancel = cancel.clone();
                let live = live.clone();
                move || {
                    live.fetch_add(1, Ordering::SeqCst);
                    pipeline.run_worker(rx, &cancel);
                    live.fetch_sub(1, Ordering::SeqCst);
                }
            })
            .map_err(|e| AppError::module("CLIPBOARD_PIPELINE_001", e.to_string(), None))?;
        Ok(PipelineHandle {
            cancel,
            live_workers: live,
            pipeline,
        })
    }

    /// write 前调用：记录回写窗口起点
    pub fn mark_write_back(&self) {
        {
            let mut g = self.write_back_at.lock();
            *g = Some(Instant::now());
        }
    }

    fn run_worker(
        self: Arc<Self>,
        rx: mpsc::Receiver<(host_core::ports::ClipContent, Option<String>)>,
        cancel: &AtomicBool,
    ) {
        loop {
            // 取首条改为 tick 轮询以协作响应取消（跨批次入库合并，docs/impl/02 C3 ⑦）
            let first = loop {
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                match rx.recv_timeout(WORKER_TICK) {
                    Ok(item) => break item,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            };
            let mut batch = vec![first];
            let deadline = Instant::now() + DB_BATCH_WINDOW;
            while batch.len() < 20 && Instant::now() < deadline {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                match rx.recv_timeout(deadline - Instant::now()) {
                    Ok(item) => batch.push(item),
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            for (content, source_app) in batch {
                self.process(content, source_app);
            }
        }
    }

    fn process(&self, content: host_core::ports::ClipContent, source_app: Option<String>) {
        self.ingest(content, source_app, "local", true);
    }

    /// D-25：远端剪贴板入库（kvm.clip_received 消费协程调用）。
    /// 先标记回写窗口（自触发 WM_CLIPBOARDUPDATE 被回调丢弃，防双写），
    /// 再写系统剪贴板（失败仅 warn 不阻断入历史），最后走与本地捕获同一
    /// 分类/加密/门控路径显式入库，origin=remote、source_app=kvm:<device_id>。
    /// 黑名单（本机应用隐私语义）不适用于已配对加密会话。
    pub fn ingest_remote(&self, content: host_core::ports::ClipContent, device_id: &str) {
        self.mark_write_back();
        if let Err(e) = self.port.write(&content) {
            tracing::warn!(error = %e, "远端剪贴板写系统剪贴板失败（历史入库继续）");
        }
        let source = format!("kvm:{device_id}");
        self.ingest(content, Some(source), "remote", false);
    }

    fn ingest(
        &self,
        content: host_core::ports::ClipContent,
        source_app: Option<String>,
        origin: &'static str,
        apply_blacklist: bool,
    ) {
        let config = futures_now(&self.config);

        // ① 黑名单过滤（excluded_apps：进程名小写比对）
        if apply_blacklist {
            if let Some(app) = &source_app {
                if config
                    .excluded_apps
                    .iter()
                    .any(|x| x.eq_ignore_ascii_case(app))
                {
                    tracing::debug!(app = %app, "来源应用在永不记录黑名单，丢弃");
                    return;
                }
            }
        }

        // ② 按类型分流（图片/文件受开关控制）
        match content {
            host_core::ports::ClipContent::Text { text, .. } => {
                self.process_text(text, source_app, origin, &config)
            }
            host_core::ports::ClipContent::Image {
                format,
                width,
                height,
                bytes,
            } => {
                if !config.capture_images {
                    return;
                }
                self.process_image(&format, width, height, &bytes, source_app, origin);
            }
            host_core::ports::ClipContent::Files { paths } => {
                if !config.capture_files {
                    return;
                }
                self.process_files(&paths, source_app, origin);
            }
        }
    }

    fn process_text(
        &self,
        text: String,
        source_app: Option<String>,
        origin: &'static str,
        config: &ClipboardConfig,
    ) {
        if text.trim().is_empty() {
            return;
        }
        // ③ 敏感检测（配置开关）
        let kind: Option<SecretKind> = if config.sensitive_filter {
            secrets().inspect(&text)
        } else {
            None
        };
        // ④ 分类
        let group = if config.auto_group {
            Classifier::classify(&text).map(|(g, _)| g)
        } else {
            None
        };

        // ⑤ 入库
        let result = match kind {
            Some(_k) => {
                let cipher = match self.crypto.protect(text.as_bytes()) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(error = %e, "敏感条目加密失败，丢弃本条");
                        return;
                    }
                };
                let b64 = base64::engine::general_purpose::STANDARD.encode(&cipher);
                self.store
                    .insert_encrypted(&b64, group, source_app.as_deref(), origin)
            }
            None => self
                .store
                .insert(&text, group, false, source_app.as_deref(), origin),
        };
        let Ok(id) = result else {
            return;
        };

        // ⑥ 清理：每 50 条插入执行一次保留期/上限淘汰（避免逐条 DELETE）
        if self
            .insert_counter
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(50)
        {
            let _ = self.store.purge(config.retention_days, config.max_entries);
        }

        // ⑦ 事件（D-03 统一背压：O8 通知型 300ms 合并发布，阈值取 TOPIC_REGISTRY；
        // 前端收到后按需 IPC 拉最新列表；secret 只带布尔标记，不外泄类别特征）
        self.bus
            .publish_merged(
                Event::new(
                    "clipboard.captured",
                    "clipboard",
                    serde_json::json!({ "id": id, "secret": kind.is_some() }),
                ),
                "captured",
                merged_window("clipboard.captured"),
            )
            .ok();
    }

    fn process_image(
        &self,
        format: &str,
        width: u32,
        height: u32,
        bytes: &[u8],
        source_app: Option<String>,
        origin: &'static str,
    ) {
        let Ok(id) =
            self.store
                .insert_image(format, width, height, bytes, source_app.as_deref(), origin)
        else {
            return;
        };
        self.bus
            .publish_merged(
                Event::new(
                    "clipboard.captured",
                    "clipboard",
                    serde_json::json!({ "id": id, "secret": false, "kind": "image" }),
                ),
                "captured",
                merged_window("clipboard.captured"),
            )
            .ok();
    }

    fn process_files(
        &self,
        paths: &[std::path::PathBuf],
        source_app: Option<String>,
        origin: &'static str,
    ) {
        if paths.is_empty() {
            return;
        }
        let Ok(id) = self
            .store
            .insert_files(paths, source_app.as_deref(), origin)
        else {
            return;
        };
        self.bus
            .publish_merged(
                Event::new(
                    "clipboard.captured",
                    "clipboard",
                    serde_json::json!({ "id": id, "secret": false, "kind": "files" }),
                ),
                "captured",
                merged_window("clipboard.captured"),
            )
            .ok();
    }
}

/// 同步读取 AsyncMutex 配置（管线 worker 为阻塞线程，不进 async 上下文）
fn futures_now(cfg: &AsyncMutex<ClipboardConfig>) -> ClipboardConfig {
    // AsyncMutex 在无竞争时 try_lock 几乎必成功；失败则退回默认值（一次清理周期内的偏差可接受）
    cfg.try_lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| ClipboardConfig::default())
}

// ---- 测试替身（crate 内共享：pipeline 单测与 module 消费协程测试都用）----

#[cfg(test)]
use host_core::ports::ClipContent;

#[cfg(test)]
pub(crate) type FakeCb =
    Arc<parking_lot::Mutex<Option<Box<dyn Fn(ClipContent, Option<String>) + Send + Sync>>>>;

/// 替身端口：start_listener 仅捕获回调（stop_listener 沿用默认 no-op，
/// 因此回调及其持有的 Sender 全程存活——正是旧实现 worker 永不退出的根因场景）；
/// write 记录文本载荷，fail_write = true 时一律返回 Err（D-25 写失败负例）。
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct FakeClipboard {
    pub(crate) cb: FakeCb,
    pub(crate) writes: Arc<parking_lot::Mutex<Vec<String>>>,
    pub(crate) fail_write: bool,
}

#[cfg(test)]
impl ClipboardPort for FakeClipboard {
    fn start_listener(
        &self,
        cb: Box<dyn Fn(ClipContent, Option<String>) + Send + Sync>,
    ) -> Result<(), AppError> {
        *self.cb.lock() = Some(cb);
        Ok(())
    }
    fn write(&self, content: &ClipContent) -> Result<(), AppError> {
        if self.fail_write {
            return Err(AppError::module("FAKE_CLIP_001", "write disabled", None));
        }
        if let ClipContent::Text { text, .. } = content {
            self.writes.lock().push(text.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) struct FakeCrypto;
#[cfg(test)]
impl CryptoPort for FakeCrypto {
    fn protect(&self, p: &[u8]) -> Result<Vec<u8>, AppError> {
        Ok(p.to_vec())
    }
    fn unprotect(&self, c: &[u8]) -> Result<Vec<u8>, AppError> {
        Ok(c.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    fn temp_store(tag: &str) -> Arc<ClipStore> {
        let dir = std::env::temp_dir().join(format!("nf_clip_pipe_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Arc::new(ClipStore::open(&dir.join("clipboard.db"), dir.join("blobs")).unwrap())
    }

    fn wait_until(mut pred: impl FnMut() -> bool, dur: Duration) -> bool {
        let deadline = Instant::now() + dur;
        while Instant::now() < deadline {
            if pred() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        pred()
    }

    fn start_with_fake(
        tag: &str,
    ) -> (
        PipelineHandle,
        Arc<ClipStore>,
        Arc<FakeClipboard>,
        Arc<EventBus>,
    ) {
        let store = temp_store(tag);
        let bus = Arc::new(EventBus::new());
        let port = Arc::new(FakeClipboard {
            cb: Arc::new(Mutex::new(None)),
            writes: Arc::new(Mutex::new(Vec::new())),
            fail_write: false,
        });
        let crypto = Arc::new(FakeCrypto);
        let config = Arc::new(AsyncMutex::new(ClipboardConfig::default()));
        let write_back = Arc::new(Mutex::new(None));
        let handle = CapturePipeline::start(
            port.clone(),
            store.clone(),
            bus.clone(),
            crypto,
            config,
            write_back,
        )
        .unwrap();
        (handle, store, port, bus)
    }

    fn fire(port: &FakeClipboard, text: &str) {
        let cb = port.cb.lock();
        let Some(f) = cb.as_ref() else { return };
        f(
            ClipContent::Text {
                text: text.into(),
                html: None,
            },
            Some("tester".into()),
        );
    }

    #[test]
    fn worker_processes_then_shuts_down_despite_live_sender() {
        // 关键：FakeClipboard 的 stop_listener 是 no-op，回调（含 Sender）始终存活，
        // 通道永不 Disconnected —— worker 仍须凭 cancel 标志在 shutdown 后退出（旧实现会泄漏）。
        let (handle, store, port, _bus) = start_with_fake("proc");
        fire(&port, "hello-s3-pipeline");
        assert!(
            wait_until(
                || store
                    .search(&crate::types::SearchQuery::default())
                    .unwrap()
                    .items
                    .len()
                    == 1,
                Duration::from_secs(2)
            ),
            "worker 应在运行期间处理入队事件"
        );
        assert_eq!(handle.live_workers(), 1, "运行期间应恰有 1 个 worker");
        handle.shutdown();
        assert!(
            handle.wait_idle(Duration::from_secs(3)),
            "shutdown 后 worker 必须退出（即使端口回调 Sender 仍存活）"
        );
        assert_eq!(handle.live_workers(), 0);
    }

    #[test]
    fn restart_does_not_accumulate_workers() {
        // 回归 S3：模拟 registry.restart（stop→init→start）多轮，存活 worker 数不得累积。
        let mut handles = Vec::new();
        for round in 0..5 {
            let (handle, _store, _port, _bus) = start_with_fake(&format!("restart{round}"));
            // 未 shutdown 前，允许存在 1 个 worker
            handles.push(handle);
        }
        // 全部停机后应无残留
        for h in &handles {
            h.shutdown();
        }
        for h in &handles {
            assert!(
                h.wait_idle(Duration::from_secs(3)),
                "存在未退出的 worker 线程"
            );
        }
        let total: usize = handles.iter().map(|h| h.live_workers()).sum();
        assert_eq!(total, 0, "5 轮启停后存活 worker 总数应为 0，实际 {total}");
    }

    #[test]
    fn write_back_window_suppresses_self_capture() {
        // 回归 D-10：回写窗口（500ms）内的写入（OCR"复制全部"、历史粘贴）不得
        // 生成新历史条目；窗口过期后的正常捕获不受影响。
        let store = temp_store("wb");
        let bus = Arc::new(EventBus::new());
        let port = Arc::new(FakeClipboard {
            cb: Arc::new(Mutex::new(None)),
            writes: Arc::new(Mutex::new(Vec::new())),
            fail_write: false,
        });
        let crypto = Arc::new(FakeCrypto);
        let config = Arc::new(AsyncMutex::new(ClipboardConfig::default()));
        let write_back = Arc::new(Mutex::new(None));
        let handle = CapturePipeline::start(
            port.clone(),
            store.clone(),
            bus,
            crypto,
            config,
            write_back.clone(),
        )
        .unwrap();

        *write_back.lock() = Some(Instant::now());
        fire(&port, "in-window-must-be-dropped");
        std::thread::sleep(WRITE_BACK_WINDOW + Duration::from_millis(150));
        fire(&port, "out-of-window-captured");

        assert!(
            wait_until(
                || store
                    .search(&crate::types::SearchQuery::default())
                    .unwrap()
                    .items
                    .len()
                    == 1,
                Duration::from_secs(2)
            ),
            "窗口外事件应恰生成 1 条记录"
        );
        std::thread::sleep(WRITE_BACK_WINDOW + Duration::from_millis(300));
        assert_eq!(
            store
                .search(&crate::types::SearchQuery::default())
                .unwrap()
                .items
                .len(),
            1,
            "窗口内事件不得入库（OCR 复制全部产生重复记录的根因）"
        );
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }

    #[test]
    fn burst_captures_merge_into_fewer_events_than_entries() {
        // D-03 回归：clipboard.captured 生产端走 EventBus::publish_merged
        // （300ms 窗口、阈值取 TOPIC_REGISTRY）。突发 8 条捕获在窗口内应合并为
        // 极少数通知（≤2 容忍真实时钟抖动），但 8 条数据全部照常入库——
        // 合并只削减通知频率，不丢数据。
        let (handle, store, port, bus) = start_with_fake("merge");
        let mut rx = bus.subscribe("clipboard.captured").unwrap();
        for i in 0..8 {
            fire(&port, &format!("merge-burst-{i}"));
        }
        assert!(
            wait_until(
                || store
                    .search(&crate::types::SearchQuery::default())
                    .unwrap()
                    .items
                    .len()
                    == 8,
                Duration::from_secs(3)
            ),
            "8 条捕获应全部入库"
        );
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(900);
        while Instant::now() < deadline {
            while let Ok(ev) = rx.try_recv() {
                events.push(ev);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !events.is_empty() && events.len() <= 2,
            "300ms 合并窗口内 8 条捕获应至多 2 条通知，实际 {}",
            events.len()
        );
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }

    // ---- D-25 远端入库（kvm.clip_received 落地路径，含负例） ----

    fn first_row(store: &Arc<ClipStore>) -> Option<crate::types::ClipEntry> {
        store
            .search(&crate::types::SearchQuery::default())
            .unwrap()
            .items
            .into_iter()
            .next()
    }

    fn remote_text(text: &str) -> ClipContent {
        ClipContent::Text {
            text: text.into(),
            html: None,
        }
    }

    #[test]
    fn remote_ingest_writes_clipboard_records_remote_row_and_suppresses_recapture() {
        // 验收①：远端内容 → 写系统剪贴板一次 + 恰 1 条 origin=remote 历史；
        // 回写窗口内的自捕获回调被丢弃（若被处理，去重晋升会立刻翻成 local）；
        // 验收④：窗口过期后本地重拷同内容 → 去重晋升 origin=local，仍 1 条。
        let (handle, store, port, _bus) = start_with_fake("remote1");
        handle
            .pipeline()
            .ingest_remote(remote_text("kvm-payload-1"), "dev-a");
        assert_eq!(
            *port.writes.lock(),
            vec!["kvm-payload-1".to_string()],
            "远端内容应写系统剪贴板一次"
        );
        assert!(
            wait_until(
                || store
                    .search(&crate::types::SearchQuery::default())
                    .unwrap()
                    .items
                    .len()
                    == 1,
                Duration::from_secs(2)
            ),
            "远端入库应恰生成 1 条历史"
        );
        let row = first_row(&store).unwrap();
        assert_eq!(row.origin, "remote");
        assert_eq!(row.source_app.as_deref(), Some("kvm:dev-a"));

        // 窗口内自回调：必须被丢弃（origin 不得翻回 local）
        fire(&port, "kvm-payload-1");
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(first_row(&store).unwrap().origin, "remote");

        // 窗口过期后本地重拷：去重晋升为 local，仍恰 1 条
        std::thread::sleep(WRITE_BACK_WINDOW);
        fire(&port, "kvm-payload-1");
        assert!(
            wait_until(
                || first_row(&store)
                    .map(|r| r.origin == "local")
                    .unwrap_or(false),
                Duration::from_secs(2)
            ),
            "本地重拷应将 origin 晋升为 local"
        );
        assert_eq!(
            store
                .search(&crate::types::SearchQuery::default())
                .unwrap()
                .items
                .len(),
            1,
            "晋升只更新既有行，不新增条目"
        );
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }

    #[test]
    fn remote_second_arrival_does_not_overwrite_local_origin() {
        // 验收④负例半边：本地先入库，远端后到同一内容只走去重累加，origin 保持 local
        let (handle, store, port, _bus) = start_with_fake("remote2");
        fire(&port, "shared-text");
        assert!(
            wait_until(
                || first_row(&store)
                    .map(|r| r.origin == "local")
                    .unwrap_or(false),
                Duration::from_secs(2)
            ),
            "本地捕获应先入库 origin=local"
        );
        handle
            .pipeline()
            .ingest_remote(remote_text("shared-text"), "dev-b");
        let row = first_row(&store).unwrap();
        assert_eq!(row.origin, "local", "远端后到不得覆写既有 local 来源");
        assert_eq!(row.usage_count, 1, "去重命中应累加 usage");
        assert_eq!(
            store
                .search(&crate::types::SearchQuery::default())
                .unwrap()
                .items
                .len(),
            1
        );
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }

    #[test]
    fn remote_history_survives_clipboard_write_failure() {
        // 验收③：port.write 失败仅 warn，历史入库继续（origin=remote）
        let store = temp_store("remote3");
        let bus = Arc::new(EventBus::new());
        let port = Arc::new(FakeClipboard {
            cb: Arc::new(Mutex::new(None)),
            writes: Arc::new(Mutex::new(Vec::new())),
            fail_write: true,
        });
        let config = Arc::new(AsyncMutex::new(ClipboardConfig::default()));
        let write_back = Arc::new(Mutex::new(None));
        let handle = CapturePipeline::start(
            port.clone(),
            store.clone(),
            bus,
            Arc::new(FakeCrypto),
            config,
            write_back,
        )
        .unwrap();
        handle
            .pipeline()
            .ingest_remote(remote_text("write-must-fail"), "dev-c");
        assert!(port.writes.lock().is_empty(), "fail_write 不应记录成功写入");
        assert!(
            wait_until(
                || first_row(&store)
                    .map(|r| r.origin == "remote" && r.source_app.as_deref() == Some("kvm:dev-c"))
                    .unwrap_or(false),
                Duration::from_secs(2)
            ),
            "写剪贴板失败不得阻断远端历史入库"
        );
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }
}
