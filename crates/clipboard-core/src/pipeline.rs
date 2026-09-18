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
use host_core::events::{Event, EventBus};
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

/// 管线运行句柄（S3）：`shutdown` 置取消位使 worker 线程退出并释放 store，
/// `live_workers` 供停机/重启回归测试观测存活 worker 数不随重启累积。
pub struct PipelineHandle {
    cancel: Arc<AtomicBool>,
    live_workers: Arc<AtomicUsize>,
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
}

pub struct CapturePipeline {
    store: Arc<ClipStore>,
    bus: Arc<EventBus>,
    crypto: Arc<dyn CryptoPort>,
    config: Arc<AsyncMutex<ClipboardConfig>>,
    write_back_at: Arc<std::sync::Mutex<Option<Instant>>>,
    insert_counter: std::sync::atomic::AtomicU32,
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
        write_back: Arc<std::sync::Mutex<Option<Instant>>>,
    ) -> Result<PipelineHandle, AppError> {
        let (tx, rx) = mpsc::channel::<(host_core::ports::ClipContent, Option<String>)>();
        let pipeline = Arc::new(Self {
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
            let in_window = wb
                .lock()
                .ok()
                .and_then(|g| *g)
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
        // 启动期临时引用就地释放；worker 那份随线程结束 drop → store 连接随之回收
        drop(pipeline);
        Ok(PipelineHandle {
            cancel,
            live_workers: live,
        })
    }

    /// write 前调用：记录回写窗口起点
    pub fn mark_write_back(&self) {
        if let Ok(mut g) = self.write_back_at.lock() {
            *g = Some(Instant::now());
        }
    }

    fn run_worker(
        self: Arc<Self>,
        rx: mpsc::Receiver<(host_core::ports::ClipContent, Option<String>)>,
        cancel: &AtomicBool,
    ) {
        loop {
            // 取首条改为 tick 轮询以协作响应取消（跨批次 50ms 窗口合并，docs/impl/02 C3 ⑦）
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
            let deadline = Instant::now() + Duration::from_millis(50);
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
        let config = futures_now(&self.config);

        // ① 黑名单过滤（excluded_apps：进程名小写比对）
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

        // ② 按类型分流（图片/文件受开关控制）
        match content {
            host_core::ports::ClipContent::Text { text, .. } => {
                self.process_text(text, source_app, &config)
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
                self.process_image(&format, width, height, &bytes, source_app);
            }
            host_core::ports::ClipContent::Files { paths } => {
                if !config.capture_files {
                    return;
                }
                self.process_files(&paths, source_app);
            }
        }
    }

    fn process_text(&self, text: String, source_app: Option<String>, config: &ClipboardConfig) {
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
                    .insert_encrypted(&b64, group, source_app.as_deref())
            }
            None => self
                .store
                .insert(&text, group, false, source_app.as_deref()),
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

        // ⑦ 事件（前端收到后按需 IPC 拉详情；secret 只带布尔标记，不外泄类别特征）
        self.bus
            .publish(Event::new(
                "clipboard.captured",
                "clipboard",
                serde_json::json!({ "id": id, "secret": kind.is_some() }),
            ))
            .ok();
    }

    fn process_image(
        &self,
        format: &str,
        width: u32,
        height: u32,
        bytes: &[u8],
        source_app: Option<String>,
    ) {
        let Ok(id) = self
            .store
            .insert_image(format, width, height, bytes, source_app.as_deref())
        else {
            return;
        };
        self.bus
            .publish(Event::new(
                "clipboard.captured",
                "clipboard",
                serde_json::json!({ "id": id, "secret": false, "kind": "image" }),
            ))
            .ok();
    }

    fn process_files(&self, paths: &[std::path::PathBuf], source_app: Option<String>) {
        if paths.is_empty() {
            return;
        }
        let Ok(id) = self.store.insert_files(paths, source_app.as_deref()) else {
            return;
        };
        self.bus
            .publish(Event::new(
                "clipboard.captured",
                "clipboard",
                serde_json::json!({ "id": id, "secret": false, "kind": "files" }),
            ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use host_core::ports::ClipContent;

    type FakeCb = Arc<Mutex<Option<Box<dyn Fn(ClipContent, Option<String>) + Send + Sync>>>>;

    /// 替身端口：start_listener 仅捕获回调（stop_listener 沿用默认 no-op，
    /// 因此回调及其持有的 Sender 全程存活——正是旧实现 worker 永不退出的根因场景）
    struct FakeClipboard {
        cb: FakeCb,
    }
    impl ClipboardPort for FakeClipboard {
        fn start_listener(
            &self,
            cb: Box<dyn Fn(ClipContent, Option<String>) + Send + Sync>,
        ) -> Result<(), AppError> {
            *self.cb.lock().unwrap() = Some(cb);
            Ok(())
        }
        fn write(&self, _content: &ClipContent) -> Result<(), AppError> {
            Ok(())
        }
    }

    struct FakeCrypto;
    impl CryptoPort for FakeCrypto {
        fn protect(&self, p: &[u8]) -> Result<Vec<u8>, AppError> {
            Ok(p.to_vec())
        }
        fn unprotect(&self, c: &[u8]) -> Result<Vec<u8>, AppError> {
            Ok(c.to_vec())
        }
    }

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

    fn start_with_fake(tag: &str) -> (PipelineHandle, Arc<ClipStore>, Arc<FakeClipboard>) {
        let store = temp_store(tag);
        let bus = Arc::new(EventBus::new());
        let port = Arc::new(FakeClipboard {
            cb: Arc::new(Mutex::new(None)),
        });
        let crypto = Arc::new(FakeCrypto);
        let config = Arc::new(AsyncMutex::new(ClipboardConfig::default()));
        let write_back = Arc::new(Mutex::new(None));
        let handle =
            CapturePipeline::start(port.clone(), store.clone(), bus, crypto, config, write_back)
                .unwrap();
        (handle, store, port)
    }

    fn fire(port: &FakeClipboard, text: &str) {
        let cb = port.cb.lock().unwrap();
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
        let (handle, store, port) = start_with_fake("proc");
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
            let (handle, _store, _port) = start_with_fake(&format!("restart{round}"));
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

        *write_back.lock().unwrap() = Some(Instant::now());
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
}
