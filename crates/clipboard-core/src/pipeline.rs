//! C3 捕获管线（docs/impl/02 C3）
//!
//! Port 回调线程只入队；worker（spawn_blocking 常驻）执行：
//! 过滤(黑名单) → secret 检测 → 分类 → 加密/入库 → 发事件。
//! 回写窗口（500ms）内到达的读取事件直接丢弃（防剪贴板循环）。

use std::sync::atomic::Ordering;
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
    pub fn start(
        port: Arc<dyn ClipboardPort>,
        store: Arc<ClipStore>,
        bus: Arc<EventBus>,
        crypto: Arc<dyn CryptoPort>,
        config: Arc<AsyncMutex<ClipboardConfig>>,
        write_back: Arc<std::sync::Mutex<Option<Instant>>>,
    ) -> Result<(), AppError> {
        let (tx, rx) = mpsc::channel::<(host_core::ports::ClipContent, Option<String>)>();
        let pipeline = Arc::new(Self {
            store,
            bus,
            crypto,
            config,
            write_back_at: write_back,
            insert_counter: std::sync::atomic::AtomicU32::new(0),
        });

        // Port 回调：只入队（消息循环线程禁长阻塞）
        let tx_cb = tx.clone();
        let wb = pipeline.write_back_at.clone();
        port.start_listener(Box::new(move |content, source_app| {
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
        }) as Box<dyn Fn(host_core::ports::ClipContent, Option<String>) + Send + Sync>)?;

        // worker：批量落库
        std::thread::Builder::new()
            .name("clipboard-pipeline".into())
            .spawn(move || pipeline.run_worker(rx))
            .map_err(|e| AppError::module("CLIPBOARD_PIPELINE_001", e.to_string(), None))?;
        Ok(())
    }

    /// write 前调用：记录回写窗口起点
    pub fn mark_write_back(&self) {
        if let Ok(mut g) = self.write_back_at.lock() {
            *g = Some(Instant::now());
        }
    }

    fn run_worker(self: Arc<Self>, rx: mpsc::Receiver<(host_core::ports::ClipContent, Option<String>)>) {
        loop {
            // 阻塞取首条（跨批次 50ms 窗口合并，docs/impl/02 C3 ⑦）
            let Ok(first) = rx.recv() else { return };
            let mut batch = vec![first];
            let deadline = Instant::now() + Duration::from_millis(50);
            while batch.len() < 20 && Instant::now() < deadline {
                match rx.recv_timeout(deadline - Instant::now()) {
                    Ok(item) => batch.push(item),
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
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
            host_core::ports::ClipContent::Text { text, .. } => self.process_text(text, source_app, &config),
            host_core::ports::ClipContent::Image { format, width, height, bytes } => {
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
                self.store.insert_encrypted(&b64, group, source_app.as_deref())
            }
            None => self.store.insert(&text, group, false, source_app.as_deref()),
        };
        let Ok(id) = result else {
            return;
        };

        // ⑥ 清理：每 50 条插入执行一次保留期/上限淘汰（避免逐条 DELETE）
        if self.insert_counter.fetch_add(1, Ordering::Relaxed) % 50 == 0 {
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

    fn process_image(&self, format: &str, width: u32, height: u32, bytes: &[u8], source_app: Option<String>) {
        let Ok(id) = self.store.insert_image(format, width, height, bytes, source_app.as_deref()) else {
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
