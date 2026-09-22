//! C3 捕获管线（docs/impl/02 C3）
//!
//! Port 回调线程只入队；worker（spawn_blocking 常驻）执行：
//! 过滤(黑名单/内容屏蔽) → secret 检测 → 分类 → 加密/入库 → 发事件。
//! 回写窗口（500ms）内到达的读取事件直接丢弃（防剪贴板循环），
//! 窗口之外另有自写来源标记作第一道（01§8-1，见 should_skip_capture）。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use host_core::error::AppError;
use host_core::events::{merged_window, Event, EventBus};
use host_core::ports::{ClipboardPort, CryptoPort};
use regex::Regex;
use tokio::sync::Mutex as AsyncMutex;

use crate::classifier::Classifier;
use crate::secrets::{global as secrets, SecretKind};
use crate::store::{ClipStore, NewClip};
use crate::types::ClipboardConfig;

/// 回写防循环窗口（docs/impl/02 C3 ③）
pub const WRITE_BACK_WINDOW: Duration = Duration::from_millis(500);

/// 回调侧丢弃判定（01§8-1 两道防循环合一，便于四格真值表测试）：
/// 自写来源标记是**第一道**（跨进程可读、不受时钟抖动影响），500ms 回写窗口是**第二道**——
/// 标记活到下一次写入为止，中途可能被外部程序覆盖，故时间窗不能删。
pub fn should_skip_capture(marker_present: bool, within_window: bool) -> bool {
    marker_present || within_window
}

/// 内容屏蔽判定（01§7.2-①）：命中返回该 pattern 原文（UI 与日志据此指真因），未命中 None。
///
/// 逐条现编不缓存：规则以十计、捕获以秒计，缓存要引入失效时机而收益为零。
/// 编译失败的正则**只作废自己**并 warn 点名序号——一条手打坏的规则不得吞掉整个捕获面，
/// 那会让用户以为"屏蔽生效"而内容照旧入库（真值表见 tests）。
pub fn block_reason(cfg: &ClipboardConfig, text: &str) -> Option<String> {
    cfg.block_patterns
        .iter()
        .enumerate()
        .find_map(|(idx, pat)| match Regex::new(pat) {
            Ok(re) => {
                if re.is_match(text) {
                    Some(pat.clone())
                } else {
                    None
                }
            }
            Err(e) => {
                tracing::warn!(
                    index = idx,
                    pattern = %pat,
                    error = %e,
                    "内容屏蔽规则正则编译失败，本条跳过（其余规则照常生效）"
                );
                None
            }
        })
}

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
    /// 暂停捕获开关（§8-④）：置位后 ingest 直接丢弃，与配置内存态同源同一原子位
    paused: Arc<AtomicBool>,
    /// 暂停期间累计跳过次数（恢复后仍保留，UI 如实显示）
    skipped: AtomicU32,
    /// 命中内容屏蔽而丢弃的次数（与 skipped 分账：暂停与屏蔽是两回事）
    blocked: AtomicU32,
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
        paused: Arc<AtomicBool>,
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
            paused,
            skipped: AtomicU32::new(0),
            blocked: AtomicU32::new(0),
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let live = Arc::new(AtomicUsize::new(0));

        // Port 回调：只入队（消息循环线程禁长阻塞）
        let tx_cb = tx.clone();
        let wb = pipeline.write_back_at.clone();
        let cancel_cb = cancel.clone();
        // Weak：回调由端口持有，强引用会与本 Arc 成环（端口永不释放 → 回调 Sender 永不释放）。
        let port_weak = Arc::downgrade(&port);
        port.start_listener(Box::new(move |content, source_app| {
            // 停机后到达的事件直接丢弃（端口收尾会释放回调，此为在途兜底）
            if cancel_cb.load(Ordering::SeqCst) {
                return;
            }
            // 自写来源标记（第一道）+ 回写窗口（第二道）：任一成立即本次不入库
            let marker = port_weak
                .upgrade()
                .is_some_and(|p| p.has_self_write_marker());
            let in_window = (*wb.lock())
                .map(|t| t.elapsed() < WRITE_BACK_WINDOW)
                .unwrap_or(false);
            if should_skip_capture(marker, in_window) {
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

    /// 暂停期间已跳过的捕获次数
    pub fn skipped_while_paused(&self) -> u32 {
        self.skipped.load(Ordering::Relaxed)
    }

    /// 命中内容屏蔽而丢弃的次数
    pub fn blocked_count(&self) -> u32 {
        self.blocked.load(Ordering::Relaxed)
    }

    /// 判定序钉死（09 §8.2 T-B3-7）：**paused → 自写标记 → excluded_apps → block_patterns → 分派**。
    /// 自写标记与回写窗口在 Port 回调侧先行（should_skip_capture，够不到本函数），故此处从 paused 起算；
    /// paused 排最前是计数口径问题：暂停期间到达的内容一律算"跳过"，不得再进屏蔽计数。
    fn ingest(
        &self,
        content: host_core::ports::ClipContent,
        source_app: Option<String>,
        origin: &'static str,
        apply_blacklist: bool,
    ) {
        // ⓪ 暂停开关（早于黑名单：暂停是用户显式意图，计数口径须覆盖全部到达内容）
        if self.paused.load(Ordering::Relaxed) {
            self.skipped.fetch_add(1, Ordering::Relaxed);
            return;
        }

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

        // ①b 内容屏蔽：命中即整条丢弃——不入库因而也不发事件（红线）。
        // 仅文本面：图片/文件没有可正则比对的"内容"文本，v1 诚实收窄（文案与测试同证）。
        if let host_core::ports::ClipContent::Text { text, .. } = &content {
            if let Some(pattern) = block_reason(&config, text) {
                self.blocked.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(pattern = %pattern, "命中内容屏蔽规则，整条丢弃");
                return;
            }
        }

        // ② 按类型分流（图片/文件受开关控制）
        match content {
            host_core::ports::ClipContent::Text { text, html } => {
                self.process_text(text, html, source_app, origin, &config)
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
        html: Option<String>,
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
        // ④ 分类：建议与落库分组同源。auto_group 关时只记 suggested_*，
        // group_name 恒 NULL（01§5-1 建议制非自动改）；开时两者同时写，采纳流可复算。
        let sugg = Classifier::classify(&text);
        let group = config.auto_group.then(|| sugg.map(|(g, _)| g)).flatten();

        // ⑤ 入库
        // 敏感臂刻意不接 html：那是同一份明文的第二副本，正文化存着就把信封加密的意义清零。
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
                    .insert_encrypted(&b64, group, sugg, source_app.as_deref(), origin)
            }
            None => self.store.insert_row(&NewClip {
                text: &text,
                group,
                suggested: sugg,
                source_app: source_app.as_deref(),
                origin,
                html: html.as_deref(),
                ..NewClip::new(&text)
            }),
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
use host_core::ports::{ClipContent, InputInjectPort, RawInput};

#[cfg(test)]
pub(crate) type FakeCb =
    Arc<parking_lot::Mutex<Option<Box<dyn Fn(ClipContent, Option<String>) + Send + Sync>>>>;

/// 替身端口：start_listener 仅捕获回调（stop_listener 沿用默认 no-op，
/// 因此回调及其持有的 Sender 全程存活——正是旧实现 worker 永不退出的根因场景）；
/// write 记录文本载荷，fail_write = true 时一律返回 Err（D-25 写失败负例）。
/// log 是与注入替身共享的**有序**调用轨迹（只记成功到达剪贴板的写，
/// 故"写先于注入"可字面断言而非依赖时序巧合）。
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct FakeClipboard {
    pub(crate) cb: FakeCb,
    pub(crate) writes: Arc<parking_lot::Mutex<Vec<String>>>,
    pub(crate) fail_write: bool,
    pub(crate) log: CallLog,
    /// 自写来源标记位（01§8-1）：置真后回调侧应在入队前就丢弃本次捕获。
    /// 用 Arc 而非 bool：替身与管线持有的必须是同一个开关。
    pub(crate) self_write: Arc<AtomicBool>,
}

/// 跨替身共享的有序调用日志
#[cfg(test)]
pub(crate) type CallLog = Arc<parking_lot::Mutex<Vec<String>>>;

/// 注入替身：按 `inject:vk,scan;vk,scan...` 形状记入同一日志；
/// fail_at = Some(n) 时第 n 次起返回 Err（粘贴堆栈"首个失败即停"负例）。
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct FakeInjector {
    pub(crate) log: CallLog,
    pub(crate) fail_at: Option<usize>,
    pub(crate) calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl InputInjectPort for FakeInjector {
    fn inject(&self, events: &[RawInput]) -> Result<(), AppError> {
        let n = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        self.log
            .lock()
            .push(format!("inject:{}", render_keys(events)));
        if self.fail_at.is_some_and(|k| n >= k) {
            return Err(AppError::module("FAKE_INJECT_001", "inject disabled", None));
        }
        Ok(())
    }
}

#[cfg(test)]
fn render_keys(events: &[RawInput]) -> String {
    events
        .iter()
        .map(|e| match e {
            RawInput::KeyDown { vk, scan } => format!("{vk},{scan}"),
            RawInput::KeyUp { vk, scan } => format!("{vk},{scan},up"),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>()
        .join(";")
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
        self.log.lock().push("write".into());
        Ok(())
    }
    fn has_self_write_marker(&self) -> bool {
        self.self_write.load(Ordering::Relaxed)
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
        start_with_fake_paused(tag, Arc::new(AtomicBool::new(false)))
    }

    fn start_with_fake_paused(
        tag: &str,
        paused: Arc<AtomicBool>,
    ) -> (
        PipelineHandle,
        Arc<ClipStore>,
        Arc<FakeClipboard>,
        Arc<EventBus>,
    ) {
        start_with_fake_cfg(tag, paused, ClipboardConfig::default())
    }

    fn start_with_fake_cfg(
        tag: &str,
        paused: Arc<AtomicBool>,
        cfg: ClipboardConfig,
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
            log: Arc::new(Mutex::new(Vec::new())),
            self_write: Arc::new(AtomicBool::new(false)),
        });
        let crypto = Arc::new(FakeCrypto);
        let config = Arc::new(AsyncMutex::new(cfg));
        let write_back = Arc::new(Mutex::new(None));
        let handle = CapturePipeline::start(
            port.clone(),
            store.clone(),
            bus.clone(),
            crypto,
            config,
            write_back,
            paused,
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
            log: Arc::new(Mutex::new(Vec::new())),
            self_write: Arc::new(AtomicBool::new(false)),
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
            Arc::new(AtomicBool::new(false)),
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

    // ---- §8-④ 暂停捕获 ----

    fn row_count(store: &Arc<ClipStore>) -> usize {
        store
            .search(&crate::types::SearchQuery::default())
            .unwrap()
            .items
            .len()
    }

    #[test]
    #[allow(non_snake_case)]
    fn capturePause_skipsIngestAndCountsSkipped() {
        let paused = Arc::new(AtomicBool::new(true));
        let (handle, store, port, _bus) = start_with_fake_paused("pause_skip", paused);
        for i in 0..3 {
            fire(&port, &format!("paused-copy-{i}"));
        }
        std::thread::sleep(DB_BATCH_WINDOW + WORKER_TICK * 3);
        assert_eq!(row_count(&store), 0, "暂停期间到达的内容不得入库");
        assert_eq!(
            handle.pipeline().skipped_while_paused(),
            3,
            "跳过次数须与到达条数一致（UI 横幅如实显示）"
        );
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }

    #[test]
    #[allow(non_snake_case)]
    fn capturePause_resumeDeliversAgain() {
        // 正对照：同一管线实例恢复后照常入库，且跳过计数不清零（历史事实持久）
        let paused = Arc::new(AtomicBool::new(true));
        let (handle, store, port, _bus) = start_with_fake_paused("pause_resume", paused.clone());
        fire(&port, "skipped-while-paused");
        std::thread::sleep(DB_BATCH_WINDOW + WORKER_TICK * 3);
        assert_eq!(row_count(&store), 0);
        assert_eq!(handle.pipeline().skipped_while_paused(), 1);

        paused.store(false, Ordering::Relaxed);
        fire(&port, "after-resume-lands");
        assert!(
            wait_until(|| row_count(&store) == 1, Duration::from_secs(2)),
            "恢复后捕获必须重新入库（否则本批两枚断言同为空洞）"
        );
        assert_eq!(
            first_row(&store).unwrap().preview,
            "after-resume-lands",
            "暂停期间跳过的内容不得补录"
        );
        assert_eq!(handle.pipeline().skipped_while_paused(), 1);
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
            log: Arc::new(Mutex::new(Vec::new())),
            self_write: Arc::new(AtomicBool::new(false)),
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
            Arc::new(AtomicBool::new(false)),
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

    /// 01§5-1「建议制非自动改」红线：auto_group=false 时分类结果只进 suggested_*，
    /// group_name 恒 NULL——用户看不到"未经同意的自动分组"，但采纳流仍可复算。
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-4）字面测试名优先于 rustc 命名惯例
    fn classifierSuggestion_notAutoAppliedWhenAutoGroupOff() {
        let payload = r#"{"a": 1, "b": [2, 3]}"#;
        let cfg = ClipboardConfig {
            auto_group: false,
            ..Default::default()
        };
        let (handle, store, port, _bus) =
            start_with_fake_cfg("sugg_off", Arc::new(AtomicBool::new(false)), cfg);
        fire(&port, payload);
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
            "auto_group 关闭不得阻断捕获入库"
        );
        let sugg = store.suggestions(50).unwrap();
        assert_eq!(sugg.len(), 1, "建议列仍须落库，否则采纳流无物可列");
        assert_eq!(sugg[0].suggested_group, "json");
        assert!(
            sugg[0].confidence >= 0.85,
            "置信度须原样入库（阈值内才进队列）"
        );
        let row = first_row(&store).expect("条目可读");
        assert!(
            row.group.is_none(),
            "auto_group=false 时 group_name 必须为 NULL，实际 {:?}",
            row.group
        );
        handle.shutdown();

        // 正对照：默认开时同一载荷直接落组（防"永远不分组"的假安全断言）
        let (on, store_on, port_on, _bus) = start_with_fake("sugg_on");
        fire(&port_on, payload);
        assert!(
            wait_until(
                || store_on
                    .search(&crate::types::SearchQuery::default())
                    .unwrap()
                    .items
                    .first()
                    .and_then(|e| e.group.as_deref())
                    == Some("json"),
                Duration::from_secs(2)
            ),
            "auto_group=true 时分类结果应写入 group_name"
        );
        assert_eq!(
            store_on.suggestions(50).unwrap().len(),
            0,
            "已自动落组的条目不重复出现在建议队列（谓词含 group_name IS NULL）"
        );
        on.shutdown();
    }

    // ---- T-B3-7 内容屏蔽 + 自写来源标记（01§7.2-① / §8-1） ----

    fn fire_content(port: &FakeClipboard, content: ClipContent) {
        let cb = port.cb.lock();
        let Some(f) = cb.as_ref() else { return };
        f(content, Some("tester".into()));
    }

    fn drain_events(rx: &mut tokio::sync::broadcast::Receiver<Event>, dur: Duration) -> usize {
        let deadline = Instant::now() + dur;
        let mut n = 0;
        while Instant::now() < deadline {
            while let Ok(_ev) = rx.try_recv() {
                n += 1;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        n
    }

    fn search_hits(store: &Arc<ClipStore>, text: &str) -> usize {
        store
            .search(&crate::types::SearchQuery {
                text: Some(text.into()),
                ..Default::default()
            })
            .unwrap()
            .items
            .len()
    }

    fn block_cfg(patterns: &[&str]) -> ClipboardConfig {
        ClipboardConfig {
            block_patterns: patterns.iter().map(|p| (*p).to_string()).collect(),
            ..Default::default()
        }
    }

    /// 红线主件：命中屏蔽规则的六位验证码既不入库、搜不到，也不进事件通道。
    /// 正对照（不匹配规则的五位数）在同一张库上入库且可搜——否则本测试的三条
    /// 零断言全都可以由"管线压根没跑"这个假象满足。
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-7）字面测试名优先于 rustc 命名惯例
    fn blockPattern_otpLikeText_neverStoredOrSearchable() {
        let (handle, store, port, bus) = start_with_fake_cfg(
            "blk_otp",
            Arc::new(AtomicBool::new(false)),
            block_cfg(&[r"\d{6}$"]),
        );
        let mut rx = bus.subscribe("clipboard.captured").unwrap();

        fire(&port, "123456");
        std::thread::sleep(DB_BATCH_WINDOW + WORKER_TICK * 3);
        assert_eq!(row_count(&store), 0, "命中屏蔽规则的验证码不得入库");
        assert_eq!(
            handle.pipeline().blocked_count(),
            1,
            "屏蔽计数须与丢弃条数一致（与暂停跳过数分账）"
        );
        assert_eq!(
            drain_events(&mut rx, Duration::from_millis(700)),
            0,
            "被屏蔽内容不得发布 clipboard.captured（合并窗口 300ms，700ms 足以暴露漏发）"
        );

        fire(&port, "12345");
        assert!(
            wait_until(|| row_count(&store) == 1, Duration::from_secs(2)),
            "不匹配规则的内容应照常入库（正对照）"
        );
        assert_eq!(
            drain_events(&mut rx, Duration::from_millis(700)),
            1,
            "正对照须真的发出事件（否则上面的零事件断言是空洞）"
        );
        assert_eq!(
            search_hits(&store, "12345"),
            1,
            "正对照须可搜（FTS 面活着）"
        );
        assert_eq!(
            search_hits(&store, "123456"),
            0,
            "被屏蔽的验证码不得出现在搜索结果里"
        );
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-7）字面测试名优先于 rustc 命名惯例
    fn blockPattern_badRegex_warnsAndDoesNotBlockOthers() {
        // 一条坏正则（未闭合分组）只作废自己：同表的好规则继续拦截，正常内容继续入库。
        assert_eq!(
            block_reason(&block_cfg(&["("]), "任何内容"),
            None,
            "坏规则单独成表时不得屏蔽任何内容"
        );
        let (handle, store, port, _bus) = start_with_fake_cfg(
            "blk_bad",
            Arc::new(AtomicBool::new(false)),
            block_cfg(&["(", r"\d{6}$"]),
        );
        fire(&port, "带 ( 号与 ) 号的正常内容");
        assert!(
            wait_until(|| row_count(&store) == 1, Duration::from_secs(2)),
            "坏正则不得吞掉正常捕获"
        );
        assert_eq!(handle.pipeline().blocked_count(), 0);
        fire(&port, "999999");
        assert!(
            wait_until(
                || handle.pipeline().blocked_count() == 1,
                Duration::from_secs(2)
            ),
            "同一张表里的好规则须仍然生效"
        );
        assert_eq!(row_count(&store), 1, "被拦的是命中好规则的那条，不是全部");
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }

    /// 诚实边界：v1 屏蔽只作用于文本面。`.*` 能命中任何文本，却管不到图片/文件条目。
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-7）字面测试名优先于 rustc 命名惯例
    fn blockPattern_appliesToTextOnly_filesAndImagePassThrough() {
        let (handle, store, port, _bus) = start_with_fake_cfg(
            "blk_scope",
            Arc::new(AtomicBool::new(false)),
            block_cfg(&[".*"]),
        );
        fire_content(
            &port,
            ClipContent::Files {
                paths: vec![std::path::PathBuf::from("d:/secret.txt")],
            },
        );
        fire_content(
            &port,
            ClipContent::Image {
                format: "dib".into(),
                width: 2,
                height: 2,
                bytes: Arc::from(vec![7u8; 48]),
            },
        );
        assert!(
            wait_until(|| row_count(&store) == 2, Duration::from_secs(2)),
            "文件与图片条目不得被文本屏蔽规则拦下"
        );
        assert_eq!(handle.pipeline().blocked_count(), 0);
        fire(&port, "任意一行文本都会被 .* 命中");
        assert!(
            wait_until(
                || handle.pipeline().blocked_count() == 1,
                Duration::from_secs(2)
            ),
            "同一条规则对文本面须真的生效（正对照）"
        );
        assert_eq!(row_count(&store), 2, "文本被拦，非文本两条原样保留");
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-7）字面测试名优先于 rustc 命名惯例
    fn captureSkip_precedenceMatrix_allFourCombinations() {
        assert!(should_skip_capture(true, true));
        assert!(
            should_skip_capture(true, false),
            "标记成立即弃用，不必等时间窗"
        );
        assert!(
            should_skip_capture(false, true),
            "时间窗仍是第二道（标记可能被外部写入提前抹掉）"
        );
        assert!(
            !should_skip_capture(false, false),
            "两道都不成立时不得误弃正常捕获（负格；前三格的断言力全来自这一格）"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-7）字面测试名优先于 rustc 命名惯例
    fn capturePause_precedesBlockRules_orderingPinned() {
        let paused = Arc::new(AtomicBool::new(true));
        let (handle, store, port, _bus) =
            start_with_fake_cfg("blk_order", paused.clone(), block_cfg(&[r"\d{6}$"]));
        fire(&port, "123456");
        std::thread::sleep(DB_BATCH_WINDOW + WORKER_TICK * 3);
        assert_eq!(
            handle.pipeline().blocked_count(),
            0,
            "暂停须排在屏蔽之前：暂停期间到达的内容计为跳过，不得计为屏蔽"
        );
        assert_eq!(handle.pipeline().skipped_while_paused(), 1);
        assert_eq!(row_count(&store), 0);

        // 正对照：解除暂停后同一条规则才走屏蔽臂（否则上面两格可能同为空洞）
        paused.store(false, Ordering::Relaxed);
        fire(&port, "123456");
        assert!(
            wait_until(
                || handle.pipeline().blocked_count() == 1,
                Duration::from_secs(2)
            ),
            "解除暂停后同一规则应命中屏蔽臂"
        );
        assert_eq!(
            handle.pipeline().skipped_while_paused(),
            1,
            "暂停计数不重复累加"
        );
        assert_eq!(row_count(&store), 0);
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }

    /// 标记位的接线证明：真值表只证明纯函数对，这一枚证明回调**确实**去问了端口。
    #[test]
    #[allow(non_snake_case)] // 命名随相邻任务书测试风格
    fn selfWriteMarker_portReportsTrue_dropsCaptureBeforeEnqueue() {
        let (handle, store, port, _bus) = start_with_fake("marker_wire");
        port.self_write.store(true, Ordering::Relaxed);
        fire(&port, "外部程序不该看见的自写正文");
        std::thread::sleep(DB_BATCH_WINDOW + WORKER_TICK * 3);
        assert_eq!(
            row_count(&store),
            0,
            "自写标记为真时不得入库（时间窗此刻是关的）"
        );
        // 正对照：清掉标记后同一管线的正常捕获照旧入库
        port.self_write.store(false, Ordering::Relaxed);
        fire(&port, "标记清除后的正常捕获");
        assert!(
            wait_until(|| row_count(&store) == 1, Duration::from_secs(2)),
            "标记为假时不得有任何丢弃"
        );
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }

    /// T-B3-8：一次富文本复制 = 正文 + HTML 两份同源入库。列表行只带 `has_html` 布尔，
    /// 正文须经 `get_html` 显式口取（分页不拖 512KB 源文）。
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-8）字面测试名优先于 rustc 命名惯例
    fn clipHtml_cfHtmlFormat_storedAlongsideText() {
        let (handle, store, port, _bus) = start_with_fake("html_along");
        fire_content(
            &port,
            ClipContent::Text {
                text: "富文本一行".into(),
                html: Some("<p><b>富文本一行</b></p>".into()),
            },
        );
        assert!(
            wait_until(|| row_count(&store) == 1, Duration::from_secs(2)),
            "带 HTML 的文本捕获须照常入库"
        );
        let page = store.search(&crate::types::SearchQuery::default()).unwrap();
        let item = &page.items[0];
        assert!(item.has_html, "有 HTML 的行须在列表上如实标注");
        assert_eq!(
            store.get_html(&item.id).unwrap().as_deref(),
            Some("<p><b>富文本一行</b></p>"),
            "HTML 原样存，不剥标签也不转义"
        );
        assert!(item.preview.contains("富文本一行"), "预览仍取自纯文本那份");
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }

    /// 负例（"无该格式不假有"）：纯文本复制的 html 恒 NULL、has_html 恒 false。
    /// 同库第二条带 HTML 作正对照——否则 false 可能只是"列没读对/守卫没补列"。
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-8）字面测试名优先于 rustc 命名惯例
    fn clipHtml_plainTextOnly_staysNull() {
        let (handle, store, port, _bus) = start_with_fake("html_null");
        fire_content(
            &port,
            ClipContent::Text {
                text: "记事本里的一行".into(),
                html: None,
            },
        );
        assert!(wait_until(
            || row_count(&store) == 1,
            Duration::from_secs(2)
        ));
        let plain = store
            .search(&crate::types::SearchQuery::default())
            .unwrap()
            .items[0]
            .clone();
        assert!(!plain.has_html);
        assert_eq!(store.get_html(&plain.id).unwrap(), None);

        fire_content(
            &port,
            ClipContent::Text {
                text: "浏览器里的一行".into(),
                html: Some("<i>浏览器里的一行</i>".into()),
            },
        );
        assert!(
            wait_until(|| row_count(&store) == 2, Duration::from_secs(2)),
            "正对照：同一条管线要能真把 HTML 写进去"
        );
        let items = store
            .search(&crate::types::SearchQuery::default())
            .unwrap()
            .items;
        let rich = items
            .iter()
            .find(|e| e.has_html)
            .expect("正对照行须带 has_html");
        assert_eq!(
            store.get_html(&rich.id).unwrap().as_deref(),
            Some("<i>浏览器里的一行</i>")
        );
        assert_eq!(
            store.get_html(&plain.id).unwrap(),
            None,
            "补写的 HTML 不得串到先前那条纯文本行上"
        );
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }

    /// 红线：敏感行永不带 HTML 入库 —— 那是同一份明文的第二副本，
    /// 存了它，信封加密就只剩存储成本（写侧收口在 insert_encrypted 根本没有 html 参）。
    #[test]
    #[allow(non_snake_case)] // 命名随相邻任务书测试风格
    fn clipHtml_secretRow_neverStoresHtml() {
        let cfg = ClipboardConfig {
            sensitive_filter: true,
            ..Default::default()
        };
        let (handle, store, port, _bus) =
            start_with_fake_cfg("html_secret", Arc::new(AtomicBool::new(false)), cfg);
        fire_content(
            &port,
            ClipContent::Text {
                text: "sk-abcdefghijklmnopqrstuvwx".into(),
                html: Some("<code>sk-abcdefghijklmnopqrstuvwx</code>".into()),
            },
        );
        assert!(wait_until(
            || row_count(&store) == 1,
            Duration::from_secs(2)
        ));
        let items = store
            .search(&crate::types::SearchQuery::default())
            .unwrap()
            .items;
        let row = &items[0];
        assert!(row.secret, "该行须被判为敏感");
        assert!(!row.has_html, "敏感行不得标注带 HTML");
        assert_eq!(store.get_html(&row.id).unwrap(), None);
        handle.shutdown();
        assert!(handle.wait_idle(Duration::from_secs(3)));
    }
}
