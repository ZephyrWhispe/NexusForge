//! ClipboardModule：Module trait 实现 + start 时挂接捕获管线（docs/impl/02 C3/C7）

use parking_lot::RwLock;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use host_core::capability::{
    HotkeyAction, HotkeyBinding, HotkeyProvider, TrayAction, TrayMenuItem, TrayProvider,
};
use host_core::error::{AppError, ModuleError};
use host_core::events::{Event, EventBus};
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};
use host_core::ports::{ClipContent, ClipboardPort, CryptoPort};
use tokio::sync::watch;
use tokio::sync::Mutex as AsyncMutex;

use crate::pipeline::{CapturePipeline, PipelineHandle};
use crate::store::ClipStore;
use crate::types::{ClipboardConfig, SearchQuery};

pub struct ClipboardModule {
    db_dir: RwLock<Option<std::path::PathBuf>>,
    store: RwLock<Option<Arc<ClipStore>>>,
    /// 回写标志：clipboard_paste 先置位，管线回调据此丢弃自回写事件（防循环）
    write_back: Arc<parking_lot::Mutex<Option<std::time::Instant>>>,
    port: RwLock<Option<Arc<dyn ClipboardPort>>>,
    crypto: RwLock<Option<Arc<dyn CryptoPort>>>,
    bus: RwLock<Option<Arc<EventBus>>>,
    config: Arc<AsyncMutex<ClipboardConfig>>,
    /// C9 清理线程取消标志
    cleanup_cancel: RwLock<Option<Arc<AtomicU8>>>,
    /// S3：捕获管线运行句柄（start 建立、stop 拆除；None 表示未运行）
    pipeline: RwLock<Option<PipelineHandle>>,
    /// D-25：kvm.clip_received 消费协程停机信道（S4 watch，同 ocr-core 模式）
    remote_shutdown: RwLock<Option<watch::Sender<bool>>>,
    state: ModuleStateCell,
}

impl ClipboardModule {
    pub fn new() -> Self {
        Self {
            db_dir: RwLock::new(None),
            store: RwLock::new(None),
            write_back: Arc::new(parking_lot::Mutex::new(None)),
            port: RwLock::new(None),
            crypto: RwLock::new(None),
            bus: RwLock::new(None),
            config: Arc::new(AsyncMutex::new(ClipboardConfig::default())),
            cleanup_cancel: RwLock::new(None),
            pipeline: RwLock::new(None),
            remote_shutdown: RwLock::new(None),
            state: ModuleStateCell::new(),
        }
    }

    pub fn store(&self) -> Option<Arc<ClipStore>> {
        self.store.read().clone()
    }

    pub fn port(&self) -> Option<Arc<dyn ClipboardPort>> {
        self.port.read().clone()
    }

    pub fn config(&self) -> Arc<AsyncMutex<ClipboardConfig>> {
        self.config.clone()
    }

    /// clipboard_paste：先置回写标志（防循环），再写剪贴板
    pub fn write_back(&self, content: &host_core::ports::ClipContent) -> Result<(), AppError> {
        {
            let mut g = self.write_back.lock();
            *g = Some(std::time::Instant::now());
        }
        let port = self.port.read().clone().ok_or(AppError::module(
            "CLIPBOARD_PASTE_001",
            "模块未就绪",
            None,
        ))?;
        port.write(content)
    }
}

impl Default for ClipboardModule {
    fn default() -> Self {
        Self::new()
    }
}

fn err(code: &str, m: impl Into<String>) -> ModuleError {
    ModuleError::Init(format!("[{code}] {}", m.into()))
}

/// 解析 kvm.clip_received 载荷（纯函数，回归入口）：
/// {device_id, content} → (device_id, ClipContent)；任何缺字段/反序列化失败 → None
pub(crate) fn parse_clip_event(ev: &Event) -> Option<(String, ClipContent)> {
    let device_id = ev.payload.get("device_id")?.as_str()?.to_string();
    let content = serde_json::from_value(ev.payload.get("content")?.clone()).ok()?;
    Some((device_id, content))
}

/// D-25：kvm.clip_received 消费协程（S4 watch 协作停机，同 ocr-core 模式）。
/// 逐事件 spawn_blocking 走 CapturePipeline::ingest_remote（回写窗口+写端口+入库）；
/// 非法载荷逐条丢弃协程存活，Lagged 不假设重放。
fn spawn_kvm_clip_consumer(
    bus: Arc<EventBus>,
    pipeline: Arc<CapturePipeline>,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let mut rx = match bus.subscribe("kvm.clip_received") {
        Ok(rx) => rx,
        Err(e) => {
            tracing::warn!(error = %e, "kvm.clip_received 订阅失败，KVM 剪贴板回写不可用");
            return tokio::spawn(async {});
        }
    };
    tokio::spawn(async move {
        loop {
            let ev = tokio::select! {
                biased;
                ch = shutdown.changed() => {
                    if ch.is_err() || *shutdown.borrow_and_update() { break; }
                    continue;
                }
                received = rx.recv() => match received {
                    Ok(ev) => ev,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                },
            };
            let Some((device_id, content)) = parse_clip_event(&ev) else {
                tracing::warn!("kvm.clip_received 载荷非法，丢弃该事件");
                continue;
            };
            let pipe = pipeline.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || {
                pipe.ingest_remote(content, &device_id);
            })
            .await
            {
                tracing::warn!(error = %e, "KVM 剪贴板回写任务 join 失败");
            }
        }
    })
}

impl Module for ClipboardModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "clipboard",
            name: "剪切板中枢",
            version: "0.1.0",
            icon: Some("clipboard"),
            priority: priority_of("clipboard"),
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let store = Arc::new(
            ClipStore::open(
                &ctx.app_data_dir.join("db").join("clipboard.db"),
                ctx.app_data_dir.join("blobs").join("clipboard"),
            )
            .map_err(|e| ModuleError::Storage(e.to_string()))?,
        );
        // D-05：启动期孤儿 blob GC（删除失败的补偿路径也在此收敛）
        match store.gc_orphan_blobs() {
            Ok(n) if n > 0 => tracing::info!(n, "启动清理：已覆写删除孤儿 blob"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "孤儿 blob GC 失败（不影响启动）"),
        }
        let port = ctx.ports.get::<dyn ClipboardPort>().ok_or_else(|| {
            err(
                "CLIPBOARD_INIT_001",
                "ClipboardPort 未注册（win-integration 缺失）",
            )
        })?;
        let crypto = ctx
            .ports
            .get::<dyn CryptoPort>()
            .ok_or_else(|| err("CLIPBOARD_INIT_002", "CryptoPort 未注册（信封加密缺失）"))?;

        *self.db_dir.write() = Some(ctx.app_data_dir.clone());
        *self.store.write() = Some(store);
        *self.port.write() = Some(port);
        *self.crypto.write() = Some(crypto);
        *self.bus.write() = Some(ctx.event_bus.clone());
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        // S3：捕获管线在 start 挂接（与 stop 对称），而非 init——否则 restart（stop→init→start）
        // 每轮都新增一个不退出的 worker 线程 + 一份 DB 连接
        let already = self.pipeline.read().is_some();
        if !already {
            let port = self
                .port()
                .ok_or_else(|| ModuleError::Start("ClipboardPort 未就绪".into()))?;
            let store = self
                .store()
                .ok_or_else(|| ModuleError::Start("store 未就绪".into()))?;
            let crypto = self
                .crypto
                .read()
                .clone()
                .ok_or_else(|| ModuleError::Start("CryptoPort 未就绪".into()))?;
            let bus = self
                .bus
                .read()
                .clone()
                .ok_or_else(|| ModuleError::Start("event bus 未就绪".into()))?;
            let handle = CapturePipeline::start(
                port,
                store,
                bus,
                crypto,
                self.config.clone(),
                self.write_back.clone(),
            )
            .map_err(|e| ModuleError::Start(e.to_string()))?;
            *self.pipeline.write() = Some(handle);
        }
        // D-25：kvm.clip_received 消费协程（事件唯一通道，O1；每次 start 重建停机信道，S4）
        {
            let (tx, rx) = watch::channel(false);
            *self.remote_shutdown.write() = Some(tx);
            let bus = self.bus.read().clone();
            let pipeline = self.pipeline.read().as_ref().map(|h| h.pipeline());
            if let (Some(bus), Some(pipeline)) = (bus, pipeline) {
                if tokio::runtime::Handle::try_current().is_ok() {
                    drop(spawn_kvm_clip_consumer(bus, pipeline, rx));
                } else {
                    *self.remote_shutdown.write() = None;
                    tracing::warn!("无 tokio 运行时，KVM 剪贴板回写协程未启动（本次 start 跳过）");
                }
            }
        }
        // 启动 C9 清理线程
        self.start_cleanup();
        self.state.set(ModuleState::Running);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        // S3：真正停机——先摘系统监听（结束消息循环线程并释放回调 Sender），
        // 再 shutdown 管线 worker（退出线程并释放 Arc<ClipStore>）
        if let Some(port) = self.port() {
            let _ = port.stop_listener();
        }
        {
            let mut g = self.pipeline.write();
            if let Some(handle) = g.take() {
                handle.shutdown();
            }
        }
        // D-25：先停消费协程（watch 协作退出，S4 对称停机）
        if let Some(tx) = self.remote_shutdown.write().take() {
            tx.send(true).ok();
        }
        // 停止清理线程（置取消标志；线程 sleep 期间会滞后响应，可接受）
        {
            let cancel = self.cleanup_cancel.read();
            if let Some(flag) = cancel.as_ref() {
                flag.store(3, Ordering::SeqCst);
            }
        }
        *self.cleanup_cancel.write() = None;
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "max_entries": {
                    "type": "integer", "title": "历史保留条数",
                    "description": "超过后按最旧删除（置顶除外）",
                    "minimum": 100, "maximum": 100000, "default": 5000
                },
                "retention_days": {
                    "type": "integer", "title": "保留天数",
                    "description": "到期自动清理", "minimum": 1, "maximum": 365, "default": 30
                },
                "capture_images": {
                    "type": "boolean", "title": "捕获图片内容",
                    "description": ">64KB 自动转 blob 存储", "default": true
                },
                "capture_files": {
                    "type": "boolean", "title": "捕获文件列表",
                    "description": "复制的文件路径入历史", "default": true
                },
                "sensitive_filter": {
                    "type": "boolean", "title": "敏感数据保护",
                    "description": "密钥/银行卡/JWT 识别并加密存储", "default": true
                },
                "auto_group": {
                    "type": "boolean", "title": "智能分组",
                    "description": "URL/代码/JSON 自动分类", "default": true
                },
                "excluded_apps": {
                    "type": "array", "title": "永不记录黑名单",
                    "description": "进程名，逗号分隔（如 1password,keepass）",
                    "items": { "type": "string" }
                }
            }
        })
    }

    fn apply_config(&self, values: serde_json::Value) -> Result<(), ModuleError> {
        let cfg: ClipboardConfig =
            serde_json::from_value(values).map_err(|e| ModuleError::Config(e.to_string()))?;
        if let Ok(mut g) = self.config.try_lock() {
            *g = cfg;
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

impl HotkeyProvider for ClipboardModule {
    fn global_hotkeys(&self) -> Vec<HotkeyBinding> {
        vec![HotkeyBinding {
            id: "clipboard.quick_panel".into(),
            label: "剪切板快速面板".into(),
            // MOD_CONTROL(0x02) | MOD_SHIFT(0x04) | MOD_NOREPEAT(0x4000)
            modifiers: 0x02 | 0x04 | 0x4000,
            vk: 0x56, // 'V'
        }]
    }

    fn hotkey_actions(&self) -> Vec<HotkeyAction> {
        let bus = self.bus.read().clone();
        let Some(bus) = bus else {
            return vec![]; // init 前不提供动作
        };
        vec![HotkeyAction {
            binding_id: "clipboard.quick_panel".into(),
            action: Arc::new(move || {
                bus.publish(Event::new(
                    "clipboard.quick_panel_toggled",
                    "clipboard",
                    serde_json::json!({}),
                ))
                .ok();
            }),
        }]
    }
}

impl ClipboardModule {
    /// C9 定时清理线程（docs/impl/02 C9）：start 时启动，每 6h 执行一次 purge；
    /// 用 std::thread + 取消标志（start 在 spawn_blocking 内被调用，无 tokio 上下文）
    fn start_cleanup(&self) {
        let already = self.cleanup_cancel.read().clone();
        if already.is_some() {
            return; // 幂等：restart 时 stop 已清空，此分支防御
        }
        let Some(store) = self.store() else { return };
        let cancel = Arc::new(AtomicU8::new(0));
        {
            let mut g = self.cleanup_cancel.write();
            *g = Some(cancel.clone());
        }
        let config = self.config.clone();
        std::thread::Builder::new()
            .name("clipboard-cleanup".into())
            .spawn(move || loop {
                // 分片睡眠：每 10min 检查一次取消标志，最长 6h
                let mut waited = std::time::Duration::ZERO;
                let interval = std::time::Duration::from_secs(6 * 3600);
                while waited < interval {
                    if cancel.load(Ordering::SeqCst) == 3 {
                        return;
                    }
                    let step = std::time::Duration::from_secs(600).min(interval - waited);
                    std::thread::sleep(step);
                    waited += step;
                }
                let cfg = config.try_lock().map(|g| g.clone()).unwrap_or_default();
                let removed = store
                    .purge(cfg.retention_days, cfg.max_entries)
                    .unwrap_or(0);
                if removed > 0 {
                    tracing::info!(removed, "剪切板历史定期清理完成");
                }
            })
            .ok();
    }

    /// 分组计数（SubNav 角标）
    pub fn group_counts(&self) -> Result<serde_json::Value, AppError> {
        self.store()
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?
            .group_counts()
    }

    /// 查询入口（C7 IPC 调用）
    pub fn search(
        &self,
        q: &SearchQuery,
    ) -> Result<crate::types::Page<crate::types::ClipEntry>, AppError> {
        self.store()
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?
            .search(q)
    }

    pub fn pin(&self, id: &str, pinned: bool) -> Result<(), AppError> {
        self.store()
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?
            .pin(id, pinned)
    }

    pub fn delete(&self, id: &str) -> Result<(), AppError> {
        self.store()
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?
            .delete(id)
    }

    pub fn clear(&self, keep_pinned: bool) -> Result<u32, AppError> {
        self.store()
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?
            .clear(keep_pinned)
    }

    pub fn push_stack(&self, id: &str) -> Result<(), AppError> {
        self.store()
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?
            .push_stack(id)
    }

    pub fn pop_stack(&self) -> Result<Option<String>, AppError> {
        self.store()
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?
            .pop_stack()
    }

    /// 解密读取（clipboard_get；信封解密经 CryptoPort）
    pub fn get_content(&self, id: &str) -> Result<Option<String>, AppError> {
        let store =
            self.store()
                .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?;
        let crypto = self.crypto.read().clone().ok_or(AppError::module(
            "CLIPBOARD_QUERY_001",
            "CryptoPort 未就绪",
            None,
        ))?;
        store.get_content(id, move |c| crypto.unprotect(c))
    }

    /// 载荷读取（clipboard_paste / clipboard_get_image 用；secret 自动解密）
    pub fn get_payload(&self, id: &str) -> Result<Option<crate::store::Payload>, AppError> {
        use crate::store::Payload;
        let store =
            self.store()
                .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?;
        let crypto = self.crypto.read().clone().ok_or(AppError::module(
            "CLIPBOARD_QUERY_001",
            "CryptoPort 未就绪",
            None,
        ))?;
        match store.get_payload(id)? {
            Some(Payload::SecretB64(b64)) => {
                use base64::Engine;
                let cipher = base64::engine::general_purpose::STANDARD
                    .decode(&b64)
                    .map_err(|e| AppError::module("CLIPBOARD_QUERY_003", e.to_string(), None))?;
                let plain = crypto.unprotect(&cipher)?;
                Ok(Some(Payload::Text(
                    String::from_utf8_lossy(&plain).to_string(),
                )))
            }
            other => Ok(other),
        }
    }
}

impl TrayProvider for ClipboardModule {
    /// D-26：托盘段「剪切板中枢」——与全局热键同一事件通路（quick_panel_toggled）
    fn tray_menu_items(&self) -> Vec<TrayMenuItem> {
        vec![TrayMenuItem {
            id: "quick_panel".into(),
            label: "打开剪切板面板".into(),
            enabled: true,
        }]
    }

    fn tray_actions(&self) -> Vec<TrayAction> {
        let bus = self.bus.read().clone();
        let Some(bus) = bus else {
            return vec![]; // init 前不提供动作
        };
        vec![TrayAction {
            item_id: "quick_panel".into(),
            action: Arc::new(move || {
                bus.publish(Event::new(
                    "clipboard.quick_panel_toggled",
                    "clipboard",
                    serde_json::json!({}),
                ))
                .ok();
            }),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{FakeClipboard, FakeCrypto};
    use host_core::ports::Ports;
    use parking_lot::Mutex;
    use std::time::Duration;

    fn clip_event(device_id: &str, text: &str) -> Event {
        let content = ClipContent::Text {
            text: text.into(),
            html: None,
        };
        Event::new(
            "kvm.clip_received",
            "kvm",
            serde_json::json!({ "device_id": device_id, "content": content }),
        )
    }

    fn find_row(store: &ClipStore, preview: &str) -> Option<crate::types::ClipEntry> {
        store
            .search(&crate::types::SearchQuery::default())
            .unwrap()
            .items
            .into_iter()
            .find(|e| e.preview == preview)
    }

    #[test]
    fn parse_clip_event_accepts_valid_and_drops_malformed() {
        // 验收②纯函数半边：缺字段 / 类型错 / content 非对象 → None，不 panic
        let good = clip_event("dev-a", "payload-text");
        let (id, content) = parse_clip_event(&good).unwrap();
        assert_eq!(id, "dev-a");
        assert!(matches!(content, ClipContent::Text { ref text, .. } if text == "payload-text"));
        for bad in [
            serde_json::json!({ "content": { "Text": { "text": "x" } } }),
            serde_json::json!({ "device_id": "d" }),
            serde_json::json!({ "device_id": "d", "content": "not-an-object" }),
            serde_json::json!({ "device_id": 7, "content": {} }),
            serde_json::json!(null),
        ] {
            let ev = Event::new("kvm.clip_received", "kvm", bad);
            assert!(parse_clip_event(&ev).is_none(), "非法载荷必须解析为 None");
        }
    }

    #[tokio::test]
    async fn kvm_clip_consumer_end_to_end_with_symmetric_shutdown() {
        // 验收①②⑥：start 挂协程 → 非法事件丢弃且协程存活 → 合法事件写系统剪贴板
        // 并入库 origin=remote → stop 后信道关闭，再发事件不产生任何写入/入库
        let dir = std::env::temp_dir().join(format!("nf_clip_mod_kvm_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ports = Arc::new(Ports::new());
        let fake = Arc::new(FakeClipboard {
            cb: Arc::new(Mutex::new(None)),
            writes: Arc::new(Mutex::new(Vec::new())),
            fail_write: false,
        });
        ports.register::<dyn ClipboardPort>(fake.clone());
        ports.register::<dyn CryptoPort>(Arc::new(FakeCrypto));
        let bus = Arc::new(EventBus::new());
        let ctx = Arc::new(ModuleContext {
            app_data_dir: dir.clone(),
            ports,
            event_bus: bus.clone(),
        });
        let module = ClipboardModule::new();
        module.init(ctx).unwrap();
        module.start().unwrap();

        bus.publish(Event::new(
            "kvm.clip_received",
            "kvm",
            serde_json::json!({ "device_id": "ghost" }),
        ))
        .unwrap();
        bus.publish(clip_event("dev-m", "kvm-mod-event")).unwrap();

        let store = module.store().unwrap();
        let row = {
            let mut found = None;
            for _ in 0..100 {
                if let Some(r) = find_row(&store, "kvm-mod-event") {
                    found = Some(r);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            found.expect("协程应消费事件并入库")
        };
        assert_eq!(row.origin, "remote");
        assert_eq!(row.source_app.as_deref(), Some("kvm:dev-m"));
        assert_eq!(*fake.writes.lock(), vec!["kvm-mod-event".to_string()]);

        module.stop().unwrap();
        bus.publish(clip_event("dev-m", "after-stop")).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            find_row(&store, "after-stop").is_none(),
            "stop 后协程必须已退出（S4 对称停机）"
        );
        assert_eq!(fake.writes.lock().len(), 1, "stop 后不得再写系统剪贴板");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D-26 验收⑤：托盘 quick_panel 动作经 bus 发布与热键同通路事件；init 前动作表为空
    #[tokio::test]
    async fn tray_action_publishes_quick_panel_toggle_observable_on_bus() {
        let m = ClipboardModule::new();
        assert!(m.tray_actions().is_empty(), "init 前不得提供动作");

        let bus = Arc::new(EventBus::new());
        let mut rx = bus
            .subscribe("clipboard.quick_panel_toggled")
            .expect("订阅 tray 事件");
        *m.bus.write() = Some(bus);
        let actions = m.tray_actions();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].item_id, "quick_panel");
        (actions[0].action)();
        let ev = rx.recv().await.expect("动作闭包应发布事件");
        assert_eq!(ev.source, "clipboard");
        assert_eq!(m.tray_menu_items()[0].label, "打开剪切板面板");
    }
}
