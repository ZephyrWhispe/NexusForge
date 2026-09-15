//! ClipboardModule：Module trait 实现 + start 时挂接捕获管线（docs/impl/02 C3/C7）

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use host_core::capability::{HotkeyAction, HotkeyBinding, HotkeyProvider};
use host_core::error::{AppError, ModuleError};
use host_core::events::{Event, EventBus};
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};
use host_core::ports::{ClipboardPort, CryptoPort};
use tokio::sync::Mutex as AsyncMutex;

use crate::pipeline::CapturePipeline;
use crate::store::ClipStore;
use crate::types::{ClipboardConfig, SearchQuery};

pub struct ClipboardModule {
    db_dir: RwLock<Option<std::path::PathBuf>>,
    store: RwLock<Option<Arc<ClipStore>>>,
    /// 回写标志：clipboard_paste 先置位，管线回调据此丢弃自回写事件（防循环）
    write_back: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    port: RwLock<Option<Arc<dyn ClipboardPort>>>,
    crypto: RwLock<Option<Arc<dyn CryptoPort>>>,
    bus: RwLock<Option<Arc<EventBus>>>,
    config: Arc<AsyncMutex<ClipboardConfig>>,
    /// C9 清理线程取消标志
    cleanup_cancel: RwLock<Option<Arc<AtomicU8>>>,
    state: AtomicU8,
}

impl ClipboardModule {
    pub fn new() -> Self {
        Self {
            db_dir: RwLock::new(None),
            store: RwLock::new(None),
            write_back: Arc::new(std::sync::Mutex::new(None)),
            port: RwLock::new(None),
            crypto: RwLock::new(None),
            bus: RwLock::new(None),
            config: Arc::new(AsyncMutex::new(ClipboardConfig::default())),
            cleanup_cancel: RwLock::new(None),
            state: AtomicU8::new(0),
        }
    }

    pub fn store(&self) -> Option<Arc<ClipStore>> {
        self.store.read().ok().and_then(|g| g.clone())
    }

    pub fn port(&self) -> Option<Arc<dyn ClipboardPort>> {
        self.port.read().ok().and_then(|g| g.clone())
    }

    pub fn config(&self) -> Arc<AsyncMutex<ClipboardConfig>> {
        self.config.clone()
    }

    /// clipboard_paste：先置回写标志（防循环），再写剪贴板
    pub fn write_back(&self, content: &host_core::ports::ClipContent) -> Result<(), AppError> {
        if let Ok(mut g) = self.write_back.lock() {
            *g = Some(std::time::Instant::now());
        }
        let port = self
            .port
            .read()
            .ok()
            .and_then(|g| g.clone())
            .ok_or(AppError::module("CLIPBOARD_PASTE_001", "模块未就绪", None))?;
        port.write(content)
    }
}

impl Default for ClipboardModule {
    fn default() -> Self {
        Self::new()
    }
}

fn err(code: &str, m: impl Into<String>) -> ModuleError {
    ModuleError::Init(m.into())
}

impl Module for ClipboardModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "clipboard",
            name: "剪切板中枢",
            version: "0.1.0",
            icon: Some("clipboard"),
            priority: 10,
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let store = Arc::new(ClipStore::open(
            &ctx.app_data_dir.join("db").join("clipboard.db"),
            ctx.app_data_dir.join("blobs").join("clipboard"),
        ).map_err(|e| ModuleError::Storage(e.to_string()))?);
        let port = ctx
            .ports
            .get::<dyn ClipboardPort>()
            .ok_or_else(|| err("CLIPBOARD_INIT_001", "ClipboardPort 未注册（win-integration 缺失）"))?;
        let crypto = ctx
            .ports
            .get::<dyn CryptoPort>()
            .ok_or_else(|| err("CLIPBOARD_INIT_002", "CryptoPort 未注册（DPAPI 缺失）"))?;

        // 捕获管线启动（worker 常驻；panic 由注册表隔离层捕获）
        CapturePipeline::start(
            port.clone(),
            store.clone(),
            ctx.event_bus.clone(),
            crypto.clone(),
            self.config.clone(),
            self.write_back.clone(),
        )
        .map_err(|e| ModuleError::Start(e.to_string()))?;

        *self.db_dir.write().map_err(|_| ModuleError::Init("锁污染".into()))? =
            Some(ctx.app_data_dir.clone());
        *self.store.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(store);
        *self.port.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(port);
        *self.crypto.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(crypto);
        *self.bus.write().map_err(|_| ModuleError::Init("锁污染".into()))? =
            Some(ctx.event_bus.clone());
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        // 管线已在 init 挂接；start 标记运行态 + 启动 C9 清理线程
        self.start_cleanup();
        self.state.store(2, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        // 停止清理线程（置取消标志；线程 sleep 期间会滞后响应，可接受）
        if let Ok(cancel) = self.cleanup_cancel.read() {
            if let Some(flag) = cancel.as_ref() {
                flag.store(3, Ordering::SeqCst);
            }
        }
        *self.cleanup_cancel.write().map_err(|_| ModuleError::Stop("锁污染".into()))? = None;
        self.state.store(1, Ordering::SeqCst);
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
        let cfg: ClipboardConfig = serde_json::from_value(values)
            .map_err(|e| ModuleError::Config(e.to_string()))?;
        if let Ok(mut g) = self.config.try_lock() {
            *g = cfg;
        }
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
        let bus = self.bus.read().ok().and_then(|g| g.clone());
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
        let already = self
            .cleanup_cancel
            .read()
            .ok()
            .and_then(|g| g.clone());
        if already.is_some() {
            return; // 幂等：restart 时 stop 已清空，此分支防御
        }
        let Some(store) = self.store() else { return };
        let cancel = Arc::new(AtomicU8::new(0));
        if let Ok(mut g) = self.cleanup_cancel.write() {
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
                let removed = store.purge(cfg.retention_days, cfg.max_entries).unwrap_or(0);
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
    pub fn search(&self, q: &SearchQuery) -> Result<crate::types::Page<crate::types::ClipEntry>, AppError> {
        self.store()
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?
            .search(q)
    }

    pub fn pin(&self, id: &str, pinned: bool) -> Result<(), AppError> {
        self.store()
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?
            .pin(id, pinned)
    }

    pub fn delete(&self, id: &str) -> Result<Option<String>, AppError> {
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

    /// 解密读取（clipboard_get；DPAPI 经 CryptoPort）
    pub fn get_content(&self, id: &str) -> Result<Option<String>, AppError> {
        let store = self
            .store()
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?;
        let crypto = self
            .crypto
            .read()
            .ok()
            .and_then(|g| g.clone())
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "CryptoPort 未就绪", None))?;
        store.get_content(id, move |c| crypto.unprotect(c))
    }

    /// 载荷读取（clipboard_paste / clipboard_get_image 用；secret 自动解密）
    pub fn get_payload(
        &self,
        id: &str,
    ) -> Result<Option<crate::store::Payload>, AppError> {
        use crate::store::Payload;
        let store = self
            .store()
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))?;
        let crypto = self
            .crypto
            .read()
            .ok()
            .and_then(|g| g.clone())
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "CryptoPort 未就绪", None))?;
        match store.get_payload(id)? {
            Some(Payload::SecretB64(b64)) => {
                use base64::Engine;
                let cipher = base64::engine::general_purpose::STANDARD
                    .decode(&b64)
                    .map_err(|e| AppError::module("CLIPBOARD_QUERY_003", e.to_string(), None))?;
                let plain = crypto.unprotect(&cipher)?;
                Ok(Some(Payload::Text(String::from_utf8_lossy(&plain).to_string())))
            }
            other => Ok(other),
        }
    }
}
