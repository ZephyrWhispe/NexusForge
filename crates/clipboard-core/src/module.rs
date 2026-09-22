//! ClipboardModule：Module trait 实现 + start 时挂接捕获管线（docs/impl/02 C3/C7）

use parking_lot::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use host_core::capability::{
    HotkeyAction, HotkeyBinding, HotkeyProvider, TrayAction, TrayMenuItem, TrayProvider,
};
use host_core::config::ConfigStore;
use host_core::error::{AppError, ModuleError};
use host_core::events::{Event, EventBus};
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};
use host_core::ports::{ClipContent, ClipboardPort, CryptoPort, InputInjectPort, RawInput};
use tokio::sync::watch;
use tokio::sync::Mutex as AsyncMutex;

use crate::pipeline::{CapturePipeline, PipelineHandle};
use crate::store::ClipStore;
use crate::types::{ClipboardConfig, SearchQuery};

/// 队首投递结果（命令层据此映射 wire DTO，core 不持有序列化形状）
pub struct StackDelivery {
    pub id: String,
    pub delivered: bool,
    pub error: Option<String>,
}

/// 全部粘贴汇总
pub struct StackPasteReport {
    pub delivered: u32,
    pub failed: u32,
    pub remaining: u32,
}

/// 格式粘贴解析结果（core 侧形状；命令层映射成 wire 的 `PasteResultDto`）
pub struct PastePayload {
    pub content: ClipContent,
    /// 实际投出去的格式："plain" | "html"
    pub format_used: String,
    /// 请求了 html 而只投成 plain —— 降级须让用户看见
    pub degraded: bool,
}

pub struct ClipboardModule {
    db_dir: RwLock<Option<std::path::PathBuf>>,
    store: RwLock<Option<Arc<ClipStore>>>,
    /// 回写标志：clipboard_paste 先置位，管线回调据此丢弃自回写事件（防循环）
    write_back: Arc<parking_lot::Mutex<Option<std::time::Instant>>>,
    port: RwLock<Option<Arc<dyn ClipboardPort>>>,
    /// 堆栈投递的第二段：写完后注入 Ctrl+V（可选端口，缺失只在投递时点名，不阻断 init）
    injector: RwLock<Option<Arc<dyn InputInjectPort>>>,
    crypto: RwLock<Option<Arc<dyn CryptoPort>>>,
    bus: RwLock<Option<Arc<EventBus>>>,
    config: Arc<AsyncMutex<ClipboardConfig>>,
    /// 配置写侧真源（缺陷① 纪律：内存态是唯一运行期真源，盘上是它的持久化投影）
    config_store: Arc<ConfigStore>,
    /// 暂停捕获运行态：与 CapturePipeline 共享同一原子位，`apply_config` 是唯一写点
    paused: Arc<AtomicBool>,
    /// C9 清理线程取消标志
    cleanup_cancel: RwLock<Option<Arc<AtomicU8>>>,
    /// S3：捕获管线运行句柄（start 建立、stop 拆除；None 表示未运行）
    pipeline: RwLock<Option<PipelineHandle>>,
    /// D-25：kvm.clip_received 消费协程停机信道（S4 watch，同 ocr-core 模式）
    remote_shutdown: RwLock<Option<watch::Sender<bool>>>,
    state: ModuleStateCell,
}

impl ClipboardModule {
    pub fn new_with_config(config_store: Arc<ConfigStore>) -> Self {
        Self {
            db_dir: RwLock::new(None),
            store: RwLock::new(None),
            write_back: Arc::new(parking_lot::Mutex::new(None)),
            port: RwLock::new(None),
            injector: RwLock::new(None),
            crypto: RwLock::new(None),
            bus: RwLock::new(None),
            config: Arc::new(AsyncMutex::new(ClipboardConfig::default())),
            config_store,
            paused: Arc::new(AtomicBool::new(false)),
            cleanup_cancel: RwLock::new(None),
            pipeline: RwLock::new(None),
            remote_shutdown: RwLock::new(None),
            state: ModuleStateCell::new(),
        }
    }

    /// 当前是否暂停捕获（读运行期原子位，非读盘）
    pub fn capture_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// 暂停期间已跳过的捕获次数；管线未运行时为 0
    pub fn capture_skipped(&self) -> u32 {
        self.pipeline
            .read()
            .as_ref()
            .map(|h| h.pipeline().skipped_while_paused())
            .unwrap_or(0)
    }

    /// 暂停开关的写入口：先落运行态原子位，再读-改-写持久值（缺文件按 `{}` 起）。
    /// 唯一实现点 [`write_capture_paused`] 与托盘动作共用，防两入口漂移。
    pub fn set_capture_paused(&self, paused: bool) -> Result<(), AppError> {
        let bus = self
            .bus
            .read()
            .clone()
            .ok_or_else(|| AppError::module("CLIPBOARD_INIT_001", "模块未就绪", None))?;
        write_capture_paused(&self.paused, &self.config_store, &bus, paused)
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

fn err(code: &str, m: impl Into<String>) -> ModuleError {
    ModuleError::Init(format!("[{code}] {}", m.into()))
}

/// 暂停捕获的单一实现点：刷运行态原子位 → 读-改-写 ConfigStore → 广播运行态。
/// IPC 命令与托盘动作共用此函数，两条入口不得各自演化。
fn write_capture_paused(
    flag: &AtomicBool,
    store: &ConfigStore,
    bus: &EventBus,
    paused: bool,
) -> Result<(), AppError> {
    flag.store(paused, Ordering::Relaxed);
    let mut values = store
        .get_module("clipboard")
        .unwrap_or_else(|_| serde_json::json!({}));
    let obj = values.as_object_mut().ok_or_else(|| {
        AppError::module("CLIPBOARD_CONFIG_001", "clipboard 配置不是 JSON 对象", None)
    })?;
    obj.insert("capture_paused".into(), serde_json::json!(paused));
    // set_module 发 host.config_changed → 订阅臂再 apply 一次（幂等，同一原子位）
    store.set_module("clipboard", values)?;
    bus.publish(Event::new(
        "clipboard.capture_state",
        "clipboard",
        serde_json::json!({ "paused": paused }),
    ))
    .ok();
    Ok(())
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
        *self.injector.write() = ctx.ports.get::<dyn InputInjectPort>();
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
                self.paused.clone(),
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
                },
                "block_patterns": {
                    "type": "array", "title": "内容屏蔽正则",
                    "description": "命中即整条不入库（例：验证码 \\d{6}$、卡号）；仅作用文本捕获",
                    "items": { "type": "string" }, "default": []
                },
                "capture_paused": {
                    "type": "boolean", "title": "暂停捕获",
                    "description": "开启后新复制内容不入库（已有历史与设置不受影响）",
                    "default": false, "readOnly": true
                }
            }
        })
    }

    fn apply_config(&self, values: serde_json::Value) -> Result<(), ModuleError> {
        let cfg: ClipboardConfig =
            serde_json::from_value(values).map_err(|e| ModuleError::Config(e.to_string()))?;
        // 运行期唯一写点：暂停原子位与配置内存态同批落，二者不得分叉
        self.paused.store(cfg.capture_paused, Ordering::Relaxed);
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

    /// 分组数据面门面（T-B3-4）：以下六项都是 store 的直通，未就绪时统一
    /// CLIPBOARD_QUERY_001，错误码语义由 store 侧给出。
    pub fn set_entry_group(&self, id: &str, group: Option<&str>) -> Result<(), AppError> {
        self.require_store()?.set_entry_group(id, group)
    }

    pub fn rename_group(&self, from: &str, to: &str) -> Result<u32, AppError> {
        self.require_store()?.rename_group(from, to)
    }

    pub fn delete_group(&self, name: &str) -> Result<u32, AppError> {
        self.require_store()?.delete_group(name)
    }

    pub fn suggestions(&self, limit: u32) -> Result<Vec<crate::types::SuggestionDto>, AppError> {
        self.require_store()?.suggestions(limit)
    }

    pub fn apply_suggestion(&self, ids: &[String], accept: bool) -> Result<u32, AppError> {
        self.require_store()?.apply_suggestion(ids, accept)
    }

    pub fn stats(&self) -> Result<crate::types::StatsDto, AppError> {
        self.require_store()?.stats()
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

    /// 入栈（返栈深；同 id 重复入栈幂等）
    pub fn stack_push(&self, id: &str) -> Result<u32, AppError> {
        self.require_store()?.stack_push(id)
    }

    pub fn stack_list(&self) -> Result<Vec<String>, AppError> {
        self.require_store()?.stack_list()
    }

    /// 队列视图（按入栈序回填条目；已删条目静默少一行，DTO 侧如实呈现）
    pub fn stack_entries(&self) -> Result<Vec<crate::types::ClipEntry>, AppError> {
        let store = self.require_store()?;
        let ids = store.stack_list()?;
        store.entries_in_order(&ids)
    }

    pub fn stack_move(&self, id: &str, to: usize) -> Result<(), AppError> {
        self.require_store()?.stack_move(id, to)
    }

    pub fn stack_remove(&self, id: &str) -> Result<bool, AppError> {
        self.require_store()?.stack_remove(id)
    }

    pub fn stack_clear(&self) -> Result<u32, AppError> {
        self.require_store()?.stack_clear()
    }

    /// 队首（不弹出）：投递成功后才 stack_take_next 真出栈，失败项不得凭空消失
    pub fn stack_peek(&self) -> Result<Option<String>, AppError> {
        Ok(self.stack_list()?.into_iter().next())
    }

    pub fn stack_take_next(&self) -> Result<Option<String>, AppError> {
        self.require_store()?.stack_take_next()
    }

    /// 投递第二段：向焦点应用注入组合键序列。
    /// 端口缺失/注入失败都是真失败，调用方须把该项记为未投递。
    pub fn inject(&self, seq: &[RawInput]) -> Result<(), AppError> {
        let port = self.injector.read().clone().ok_or_else(|| {
            AppError::module(
                "CLIPBOARD_STACK_002",
                "InputInjectPort 未注册，无法注入粘贴按键",
                Some("内容已写入剪贴板，请手动按 Ctrl+V"),
            )
        })?;
        port.inject(seq)
    }

    /// 队首投递一次（两段式：先写系统剪贴板，再注入组合键）。`None` = 空栈。
    /// 红线：敏感条目既不写也不出栈；写失败同样不出栈（内容根本没进剪贴板）；
    /// 注入失败则已出栈——明文确实进了剪贴板，项不能凭空留栈。
    pub fn stack_deliver_head(&self, seq: &[RawInput]) -> Result<Option<StackDelivery>, AppError> {
        let Some(id) = self.stack_peek()? else {
            return Ok(None);
        };
        // 载荷解析与 clipboard_paste 同一决策点：敏感行在那里被拒，就在这里被拒，
        // 两处规则各写一份迟早分叉。
        let content = match self.paste_content(&id) {
            Ok(content) => content,
            Err(ref e) if e.code() == "CLIPBOARD_PASTE_004" => {
                return Ok(Some(StackDelivery {
                    id,
                    delivered: false,
                    error: Some(e.to_string()),
                }))
            }
            Err(e) => return Err(e),
        };
        if let Err(e) = self.write_back(&content) {
            return Ok(Some(StackDelivery {
                id,
                delivered: false,
                error: Some(e.to_string()),
            }));
        }
        let injected = self.inject(seq);
        self.stack_take_next()?;
        Ok(Some(StackDelivery {
            id,
            delivered: injected.is_ok(),
            error: injected.err().map(|e| e.to_string()),
        }))
    }

    /// 全部粘贴：逐条投递，两条之间间隔 `gap`；首个失败即停，剩余如实留栈。
    pub fn stack_paste_all(
        &self,
        gap: std::time::Duration,
        seq: &[RawInput],
    ) -> Result<StackPasteReport, AppError> {
        let (mut delivered, mut failed) = (0u32, 0u32);
        while let Some(head) = self.stack_deliver_head(seq)? {
            if !head.delivered {
                failed += 1;
                break;
            }
            delivered += 1;
            if self.stack_peek()?.is_some() {
                std::thread::sleep(gap);
            }
        }
        Ok(StackPasteReport {
            delivered,
            failed,
            remaining: self.stack_list()?.len() as u32,
        })
    }

    fn require_store(&self) -> Result<Arc<ClipStore>, AppError> {
        self.store()
            .ok_or(AppError::module("CLIPBOARD_QUERY_001", "模块未就绪", None))
    }

    /// 原始载荷（**不经**信封解密）：堆栈投递须区分敏感行并当场拒投，
    /// 而 get_payload 的自动解密臂会把敏感行还原成明文——两个语义必须分开。
    pub fn raw_payload(&self, id: &str) -> Result<Option<crate::store::Payload>, AppError> {
        self.require_store()?.get_payload(id)
    }

    /// 解密读取（clipboard_get；信封解密经 CryptoPort）
    ///
    /// 红线（09 §8.1-⑤）：敏感行**不随通用读口**返回明文。写侧把明文封进信封，
    /// 读侧若一口全放则加密只剩存储成本，故明文只有一个出口：`reveal_secret`
    /// （带前端二次确认 + 宿主审计）。
    pub fn get_content(&self, id: &str) -> Result<Option<String>, AppError> {
        if let Some(crate::store::Payload::SecretB64(_)) = self.raw_payload(id)? {
            return Err(AppError::module(
                "CLIPBOARD_GET_001",
                "敏感条目不随通用读口返回明文",
                Some("请调用 clipboard_secret_reveal；揭示会写入宿主审计日志"),
            ));
        }
        let store = self.require_store()?;
        let crypto = self.crypto.read().clone().ok_or(AppError::module(
            "CLIPBOARD_QUERY_001",
            "CryptoPort 未就绪",
            None,
        ))?;
        store.get_content(id, move |c| crypto.unprotect(c))
    }

    /// 按需揭示（唯一明文出口，T-B3-5）：成功即落一条 audit 目标日志。
    pub fn reveal_secret(&self, id: &str) -> Result<String, AppError> {
        use crate::store::Payload;
        use base64::Engine;
        let b64 = match self.raw_payload(id)? {
            None => {
                return Err(AppError::module(
                    "CLIPBOARD_REVEAL_002",
                    "敏感条目内容缺失",
                    Some("密文列为空或 blob 文件已清理，无法还原明文"),
                ))
            }
            Some(Payload::SecretB64(b64)) => b64,
            Some(_) => {
                return Err(AppError::module(
                    "CLIPBOARD_REVEAL_001",
                    "该条目不是敏感条目",
                    Some("普通条目用 clipboard_get"),
                ))
            }
        };
        let crypto = self.crypto.read().clone().ok_or(AppError::module(
            "CLIPBOARD_QUERY_001",
            "CryptoPort 未就绪",
            None,
        ))?;
        let cipher = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .map_err(|e| reveal_failed(e.to_string()))?;
        let plain = crypto
            .unprotect(&cipher)
            .map_err(|e| reveal_failed(e.to_string()))?;
        let text = String::from_utf8_lossy(&plain).to_string();
        tracing::warn!(
            target: "audit",
            kind = crate::types::SECRET_CATEGORY_LABEL,
            "clipboard secret revealed id={id}"
        );
        Ok(text)
    }

    /// clipboard_paste 的载荷解析：敏感行当场拒投。
    /// 必须走 `raw_payload`——`get_payload` 的自动解密臂会把密文行还原成明文，
    /// 那样 `CLIPBOARD_PASTE_004` 成一具永远到不了的空壳，粘贴即外泄明文。
    pub fn paste_content(&self, id: &str) -> Result<ClipContent, AppError> {
        use crate::store::Payload;
        let payload = self
            .raw_payload(id)?
            .ok_or_else(|| AppError::module("CLIPBOARD_PASTE_002", "条目不存在", None))?;
        Ok(match payload {
            Payload::Text(text) => ClipContent::Text { text, html: None },
            Payload::Files(paths) => ClipContent::Files { paths },
            Payload::Image { format, bytes } => ClipContent::Image {
                format,
                width: 0,
                height: 0,
                bytes: Arc::from(bytes.into_boxed_slice()),
            },
            Payload::SecretB64(_) => {
                return Err(AppError::module(
                    "CLIPBOARD_PASTE_004",
                    "敏感条目需先揭示后粘贴",
                    Some("请在敏感库点「揭示」确认后再复制，或改用堆栈投递明文条目"),
                ))
            }
        })
    }

    /// HTML 正文读取（clipboard_html_get）：详情框的 HTML 源视图是唯一取正文的口，
    /// 列表与堆栈查询只带 `has_html` 布尔（最多 512KB 的源文不随分页拖出）。
    ///
    /// 敏感行一律拒（与 `get_content` 的 CLIPBOARD_GET_001 同律）：管线从不为敏感行
    /// 存 HTML，但手改库/备份还原能把那份明文塞回来——读口比写口宽就是自我绕过。
    pub fn get_html(&self, id: &str) -> Result<Option<String>, AppError> {
        if let Some(crate::store::Payload::SecretB64(_)) = self.raw_payload(id)? {
            return Err(AppError::module(
                "CLIPBOARD_HTML_002",
                "敏感条目不随 HTML 读口返回正文",
                Some("请到「敏感库」视图逐行揭示；HTML 源是同一份明文的第二副本"),
            ));
        }
        self.require_store()?.get_html(id)
    }

    /// 宿主 app_data 根目录（`start` 时记下）。导出目录是它的 `export/` 子目录——
    /// 写盘白名单只有这一个，导入侧的路径 Input 只出现在**读**侧。
    pub fn app_data_dir(&self) -> Option<std::path::PathBuf> {
        self.db_dir.read().clone()
    }

    /// 加密导出（T-B3-9）。红线：**出文件的零明文**——敏感行在此解一次明文，
    /// 那份明文只活在内存里的 `BackupRow.secret_b64`，随即被口令派生密钥的
    /// AES-256-GCM 信封包住；`include_secrets` 只是范围开关，口令门在其为真时立起
    /// （拒得干脆，而不是静默丢几行让用户以为备份是全的）。
    pub fn export_backup(
        &self,
        passphrase: &str,
        include_secrets: bool,
    ) -> Result<(std::path::PathBuf, crate::backup::ExportMeta), AppError> {
        crate::backup::check_export_passphrase(passphrase, include_secrets)?;
        let store = self.require_store()?;
        let crypto = self.crypto.read().clone().ok_or(AppError::module(
            "CLIPBOARD_QUERY_001",
            "CryptoPort 未就绪",
            None,
        ))?;
        let dir = self.app_data_dir().ok_or_else(|| {
            AppError::module(
                "CLIPBOARD_QUERY_001",
                "模块未就绪（无 app_data 目录）",
                None,
            )
        })?;

        let mut rows = Vec::new();
        let mut meta = crate::backup::ExportMeta::default();
        for src in store.all_for_export()? {
            if src.blob_path.is_some() || src.content_type == "image" {
                meta.images_skipped += 1;
                continue;
            }
            let (text, secret_b64) = if !src.secret {
                (src.content.clone(), None)
            } else if !include_secrets {
                // 用户明确不要敏感行：整行不出门（与"静默丢"的区别是这是所选范围）
                continue;
            } else {
                let cipher = host_core::util::b64_decode(&src.content).ok_or_else(|| {
                    AppError::module("CLIPBOARD_EXPORT_002", "敏感行密文列不是合法 base64", None)
                })?;
                let plain = crypto.unprotect(&cipher).map_err(|e| {
                    AppError::module(
                        "CLIPBOARD_EXPORT_002",
                        format!("敏感行解密失败，导出中止而非跳过该行: {e}"),
                        None,
                    )
                })?;
                (String::new(), Some(host_core::util::b64_encode(&plain)))
            };
            if src.secret {
                meta.secrets += 1;
            }
            rows.push(crate::backup::BackupRow {
                content_type: src.content_type,
                text,
                html: src.html,
                group_name: src.group_name,
                secret: src.secret,
                secret_b64,
                source_app: src.source_app,
                created_at: src.created_at,
                pinned: src.pinned,
            });
        }
        meta.entries = rows.len() as u32;
        let now = crate::types::now_ms();
        meta.exported_at_ms = now.max(0) as u64;

        let json = serde_json::to_vec(&rows).map_err(|e| {
            AppError::module("CLIPBOARD_EXPORT_003", format!("备份序列化失败: {e}"), None)
        })?;
        let mut env = crate::backup::encrypt_backup(&json, passphrase, meta.exported_at_ms)?;
        env.meta = meta.clone();

        let dir = dir.join("export");
        std::fs::create_dir_all(&dir).map_err(|e| {
            AppError::module("CLIPBOARD_EXPORT_004", format!("建导出目录失败: {e}"), None)
        })?;
        let path = dir.join(format!("clipboard-{now}.nfclip.json"));
        // 先 .tmp 后 rename：中途失败留下的是 .tmp，不会有一份"看起来完整"的半个备份
        let tmp = path.with_extension("nfclip.json.tmp");
        let envelope = serde_json::to_vec_pretty(&env).map_err(|e| {
            AppError::module("CLIPBOARD_EXPORT_003", format!("备份封装失败: {e}"), None)
        })?;
        std::fs::write(&tmp, envelope).map_err(|e| {
            AppError::module("CLIPBOARD_EXPORT_004", format!("写备份失败: {e}"), None)
        })?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            AppError::module(
                "CLIPBOARD_EXPORT_004",
                format!("备份改名落位失败: {e}"),
                None,
            )
        })?;
        tracing::info!(
            entries = meta.entries,
            secrets = meta.secrets,
            "剪贴板备份已导出"
        );
        Ok((path, meta))
    }

    /// 加密导入（T-B3-9）。失败顺序即红线顺序：文件读不到 / 非本格式 / 口令错，
    /// 三者的任何一路都在落库之前，故"导入失败"不会留下半套数据。
    pub fn import_backup(
        &self,
        path: &std::path::Path,
        passphrase: &str,
    ) -> Result<crate::store::ImportReport, AppError> {
        let raw = std::fs::read(path).map_err(|e| {
            AppError::module(
                "CLIPBOARD_IMPORT_002",
                format!("读不到备份文件「{}」: {e}", path.display()),
                None,
            )
        })?;
        let env = crate::backup::decode_envelope(&raw)?;
        let json = crate::backup::decrypt_backup(&env, passphrase)?;
        let rows = crate::backup::parse_backup_rows(&json)?;
        if env.meta.entries as usize != rows.len() {
            return Err(AppError::module(
                "CLIPBOARD_IMPORT_003",
                format!(
                    "备份清单声明 {} 行，实际 {} 行（文件被改过或写入未完成）",
                    env.meta.entries,
                    rows.len()
                ),
                None,
            ));
        }
        let store = self.require_store()?;
        let crypto = self.crypto.read().clone().ok_or(AppError::module(
            "CLIPBOARD_QUERY_001",
            "CryptoPort 未就绪",
            None,
        ))?;
        let report = store.import_rows(&rows, crypto.as_ref())?;
        tracing::info!(
            imported = report.imported,
            duplicates = report.duplicates,
            "剪贴板备份已导入"
        );
        Ok(report)
    }

    /// clipboard_paste(id, format) 的载荷解析（T-B3-8）。
    /// 请求 html 而行无 html → 回落纯文本并 `degraded = true`：投出去的是纯文本，
    /// 就不谎称带了格式（前端据此出「已降级」提示，而不是静默成功）。
    /// 图片/文件行本就没有"带格式"一说：want_html 时同样 plain + degraded，
    /// 该组合 UI 不产生（无 html 的行不渲染带格式钮），留在数据面如实成一格。
    pub fn paste_content_format(
        &self,
        id: &str,
        want_html: bool,
    ) -> Result<PastePayload, AppError> {
        let mut content = self.paste_content(id)?;
        if want_html {
            if let ClipContent::Text { text: _, html } = &mut content {
                if let Some(body) = self.get_html(id)? {
                    *html = Some(body);
                    return Ok(PastePayload {
                        content,
                        format_used: "html".to_string(),
                        degraded: false,
                    });
                }
            }
            return Ok(PastePayload {
                content,
                format_used: "plain".to_string(),
                degraded: true,
            });
        }
        Ok(PastePayload {
            content,
            format_used: "plain".to_string(),
            degraded: false,
        })
    }

    /// 载荷读取（clipboard_get_image 用；secret 自动解密成明文，
    /// 因此**粘贴/揭示都不走此口**——见 `paste_content` 与 `reveal_secret`）
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

/// 揭示失败（密文解不开）：与"非敏感"分开编码，hint 指向 KEK 这一真因，
/// 否则用户只会看到一个没有下文的"解密失败"。
fn reveal_failed(reason: String) -> AppError {
    AppError::module(
        "CLIPBOARD_REVEAL_002",
        format!("敏感条目解密失败：{reason}"),
        Some("信封 KEK 可能已变更（重装/密钥重置），旧密文不可恢复"),
    )
}

impl TrayProvider for ClipboardModule {
    /// D-26：托盘段「剪切板中枢」——与全局热键同一事件通路（quick_panel_toggled）。
    /// §8-④：capture 项标签读运行态暂停原子位；tray.rs 订阅 clipboard.capture_state
    /// 重建菜单，故切换后托盘文案真的翻转（不是常驻静默项）。
    fn tray_menu_items(&self) -> Vec<TrayMenuItem> {
        let paused = self.capture_paused();
        vec![
            TrayMenuItem {
                id: "quick_panel".into(),
                label: "打开剪切板面板".into(),
                enabled: true,
            },
            TrayMenuItem {
                id: "capture".into(),
                label: if paused {
                    "恢复捕获"
                } else {
                    "暂停捕获"
                }
                .into(),
                enabled: true,
            },
        ]
    }

    fn tray_actions(&self) -> Vec<TrayAction> {
        let bus = self.bus.read().clone();
        let Some(bus) = bus else {
            return vec![]; // init 前不提供动作
        };
        let panel_bus = bus.clone();
        let cap_bus = bus.clone();
        let cap_flag = self.paused.clone();
        let cap_store = self.config_store.clone();
        vec![
            TrayAction {
                item_id: "quick_panel".into(),
                action: Arc::new(move || {
                    panel_bus
                        .publish(Event::new(
                            "clipboard.quick_panel_toggled",
                            "clipboard",
                            serde_json::json!({}),
                        ))
                        .ok();
                }),
            },
            TrayAction {
                item_id: "capture".into(),
                action: Arc::new(move || {
                    let next = !cap_flag.load(Ordering::Relaxed);
                    if let Err(e) = write_capture_paused(&cap_flag, &cap_store, &cap_bus, next) {
                        tracing::warn!(error = %e, "托盘切换捕获开关失败");
                    }
                }),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{CallLog, FakeClipboard, FakeCrypto, FakeInjector};
    use crate::store::NewClip;
    use host_core::ports::Ports;
    use host_core::registry::ModuleRegistry;
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
            log: Arc::new(Mutex::new(Vec::new())),
            self_write: Default::default(),
        });
        ports.register::<dyn ClipboardPort>(fake.clone());
        ports.register::<dyn CryptoPort>(Arc::new(FakeCrypto));
        let bus = Arc::new(EventBus::new());
        let ctx = Arc::new(ModuleContext {
            app_data_dir: dir.clone(),
            ports,
            event_bus: bus.clone(),
        });
        let module = ClipboardModule::new_with_config(test_config_store(&dir, &bus));
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

    /// 测试共用 ConfigStore（与模块同一 bus，host.config_changed 才可观测）
    fn test_config_store(dir: &std::path::Path, bus: &Arc<EventBus>) -> Arc<ConfigStore> {
        Arc::new(ConfigStore::new(dir.join("config"), bus.clone()))
    }

    fn fire_text(fake: &FakeClipboard, text: &str) {
        let cb = fake.cb.lock();
        let Some(f) = cb.as_ref() else { return };
        f(
            ClipContent::Text {
                text: text.into(),
                html: None,
            },
            Some("tester".into()),
        );
    }

    /// 起一套「真 ConfigStore + 真模块 + 假端口」的运行期装配（缺陷① 回归共用体）
    struct Harness {
        dir: std::path::PathBuf,
        bus: Arc<EventBus>,
        store_cfg: Arc<ConfigStore>,
        registry: Arc<ModuleRegistry>,
        module: Arc<ClipboardModule>,
        port: Arc<FakeClipboard>,
        /// 两个假端口共写的有序轨迹（写 vs 注入的先后即由它判定）
        log: CallLog,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn harness(tag: &str) -> Harness {
        harness_injecting(tag, None).await
    }

    /// `fail_at = Some(n)`：第 n 次起的注入返回 Err（paste_all 首个失败即停负例）
    async fn harness_injecting(tag: &str, fail_at: Option<usize>) -> Harness {
        let dir = std::env::temp_dir().join(format!("nf_clip_mod_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bus = Arc::new(EventBus::new());
        let store_cfg = test_config_store(&dir, &bus);
        let ports = Arc::new(Ports::new());
        let log: CallLog = Arc::default();
        let port = Arc::new(FakeClipboard {
            log: log.clone(),
            ..Default::default()
        });
        let injector = Arc::new(FakeInjector {
            log: log.clone(),
            fail_at,
            ..Default::default()
        });
        ports.register::<dyn ClipboardPort>(port.clone());
        ports.register::<dyn CryptoPort>(Arc::new(FakeCrypto));
        ports.register::<dyn InputInjectPort>(injector.clone());
        let module = Arc::new(ClipboardModule::new_with_config(store_cfg.clone()));
        store_cfg.register_schema("clipboard", module.config_schema());
        let registry = Arc::new(ModuleRegistry::new(bus.clone()));
        registry.register(module.clone()).unwrap();
        let ctx = Arc::new(ModuleContext {
            app_data_dir: dir.clone(),
            ports,
            event_bus: bus.clone(),
        });
        for (_, r) in registry.init_all(ctx).await {
            r.expect("init 应成功");
        }
        for (_, r) in registry.start_all().await {
            r.expect("start 应成功");
        }
        Harness {
            dir,
            bus,
            store_cfg,
            registry,
            module,
            port,
            log,
        }
    }

    async fn wait_for_row(store: &ClipStore, preview: &str) -> bool {
        for _ in 0..100 {
            if find_row(store, preview).is_some() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    /// 缺陷① 核心红线：运行期改配置经订阅派发真正抵达运行中模块。
    /// 对照臂（绕过 set_module 直接改盘文件必须**不**生效）证明兑现的是
    /// host.config_changed 订阅通路，而不是轮询或巧合。
    #[tokio::test]
    #[allow(non_snake_case)]
    async fn hostConfig_changeAtRuntime_reachesRunningModule() {
        let h = harness("cfg_runtime").await;
        let store = h.module.store().unwrap();

        // 对照臂：手改盘文件（无事件）→ 运行态与捕获行为均不变
        std::fs::create_dir_all(h.dir.join("config")).unwrap();
        std::fs::write(
            h.dir.join("config").join("clipboard.json"),
            r#"{"capture_paused": true}"#,
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !h.module.capture_paused(),
            "直接改盘文件不得影响运行态：真源接线只认 set_module 事件"
        );
        fire_text(&h.port, "control-arm-lands");
        assert!(
            wait_for_row(&store, "control-arm-lands").await,
            "对照臂期间捕获须照常入库"
        );

        // 正臂：set_module → host.config_changed → run_config_feed → apply_one
        let rx = h.bus.subscribe("host.config_changed").unwrap();
        tokio::spawn(host_core::registry::run_config_feed(
            rx,
            h.registry.clone(),
            h.store_cfg.clone(),
        ));
        h.store_cfg
            .set_module("clipboard", serde_json::json!({ "capture_paused": true }))
            .unwrap();
        for _ in 0..100 {
            if h.module.capture_paused() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(h.module.capture_paused(), "配置变更须在运行期派发到位");

        fire_text(&h.port, "must-not-land-while-paused");
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            find_row(&store, "must-not-land-while-paused").is_none(),
            "派发到位后新捕获须被丢弃"
        );
        assert!(h.module.capture_skipped() >= 1);
    }

    /// 缺陷① 启动半边：bootstrap 的 apply_configs 派发持久值 → 首条复制即不入库
    #[tokio::test]
    #[allow(non_snake_case)]
    async fn hostConfig_bootstrap_appliesPersistedValues() {
        let h = harness("cfg_bootstrap").await;
        h.store_cfg
            .set_module("clipboard", serde_json::json!({ "capture_paused": true }))
            .unwrap();
        // 未派发前仍是运行默认（暂停值不会自己长出来）
        assert!(!h.module.capture_paused());
        let results = h.registry.apply_configs(&h.store_cfg).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_ok(), "派发须成功： {:?}", results[0].1);
        assert!(h.module.capture_paused());

        let store = h.module.store().unwrap();
        fire_text(&h.port, "first-copy-after-bootstrap");
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            find_row(&store, "first-copy-after-bootstrap").is_none(),
            "重启后盘上 paused=true 须让首条复制即不入库"
        );
    }

    /// 负例：盘上缺文件 / 空对象不得把运行态打回模块默认值
    #[tokio::test]
    #[allow(non_snake_case)]
    async fn applyConfigs_emptyStoredFile_doesNotOverwriteRuntime() {
        let h = harness("cfg_empty").await;
        h.module
            .apply_config(serde_json::json!({ "max_entries": 1234, "capture_paused": true }))
            .unwrap();

        // 缺文件
        assert!(
            h.registry.apply_configs(&h.store_cfg).await.is_empty(),
            "缺文件不派发"
        );
        assert!(h.module.capture_paused(), "缺文件不得覆写运行态");
        assert_eq!(h.module.config().try_lock().unwrap().max_entries, 1234);

        // 空对象文件
        std::fs::create_dir_all(h.dir.join("config")).unwrap();
        std::fs::write(h.dir.join("config").join("clipboard.json"), "{}").unwrap();
        assert!(
            h.registry.apply_configs(&h.store_cfg).await.is_empty(),
            "空对象不派发"
        );
        assert!(
            h.module.capture_paused(),
            "空对象不得把 paused 打回默认 false"
        );

        // 正对照：真值文件必须派发（否则上面两臂是空洞）
        h.store_cfg
            .set_module("clipboard", serde_json::json!({ "max_entries": 777 }))
            .unwrap();
        let results = h.registry.apply_configs(&h.store_cfg).await;
        assert_eq!(results.len(), 1, "有值才进派发结果");
        assert!(results[0].1.is_ok());
        assert_eq!(h.module.config().try_lock().unwrap().max_entries, 777);
        assert!(
            !h.module.config().try_lock().unwrap().capture_paused,
            "缺省键经 serde(default) 回落默认值"
        );
    }

    /// §8-④ 托盘项：capture 项存在、可用，且标签随运行态真实翻转
    #[tokio::test]
    #[allow(non_snake_case)]
    async fn trayMenu_hasCaptureItem_withDynamicLabel() {
        let h = harness("tray_capture").await;
        // 单一写口红线：该键在 schema 中标 readOnly，通用设置表单据此跳过它（SchemaForm 消费此注解）
        assert_eq!(
            h.module.config_schema()["properties"]["capture_paused"]["readOnly"],
            serde_json::json!(true)
        );
        let cap_label = |m: &ClipboardModule| {
            m.tray_menu_items()
                .into_iter()
                .find(|i| i.id == "capture")
                .map(|i| (i.label, i.enabled))
        };
        let (label, enabled) = cap_label(&h.module).expect("托盘须有 capture 项");
        assert_eq!(label, "暂停捕获");
        assert!(enabled, "暂停项须可点，不是常驻灰静默项");

        let mut rx = h.bus.subscribe("clipboard.capture_state").unwrap();
        let action = h
            .module
            .tray_actions()
            .into_iter()
            .find(|a| a.item_id == "capture")
            .expect("capture 动作须注册");
        (action.action)();
        let ev = rx.recv().await.expect("动作须广播运行态");
        assert_eq!(ev.payload["paused"], serde_json::json!(true));
        assert!(h.module.capture_paused(), "托盘动作须真的翻转运行态");
        let (label, _) = cap_label(&h.module).unwrap();
        assert_eq!(
            label, "恢复捕获",
            "标签随暂停态翻转（tray.rs 重建菜单消费此值）"
        );
        assert_eq!(
            h.store_cfg.get_module("clipboard").unwrap()["capture_paused"],
            serde_json::json!(true),
            "托盘切换同样落持久值（重启后仍是暂停态）"
        );

        // 再点一次回到恢复态
        let again = h
            .module
            .tray_actions()
            .into_iter()
            .find(|a| a.item_id == "capture")
            .expect("暂停态下托盘仍须给出 capture 动作项");
        (again.action)();
        assert!(!h.module.capture_paused());
        assert_eq!(cap_label(&h.module).unwrap().0, "暂停捕获");
    }

    /// D-26 验收⑤：托盘 quick_panel 动作经 bus 发布与热键同通路事件；init 前动作表为空
    #[tokio::test]
    async fn tray_action_publishes_quick_panel_toggle_observable_on_bus() {
        let bus = Arc::new(EventBus::new());
        let dir = std::env::temp_dir().join(format!("nf_clip_mod_tray_{}", std::process::id()));
        let m = ClipboardModule::new_with_config(test_config_store(&dir, &bus));
        assert!(m.tray_actions().is_empty(), "init 前不得提供动作");

        let mut rx = bus
            .subscribe("clipboard.quick_panel_toggled")
            .expect("订阅 tray 事件");
        *m.bus.write() = Some(bus);
        let actions = m.tray_actions();
        assert_eq!(actions.len(), 2, "quick_panel + capture 两项");
        let panel = actions
            .iter()
            .find(|a| a.item_id == "quick_panel")
            .expect("quick_panel 动作须注册");
        (panel.action)();
        let ev = rx.recv().await.expect("动作闭包应发布事件");
        assert_eq!(ev.source, "clipboard");
        assert_eq!(m.tray_menu_items()[0].label, "打开剪切板面板");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 与 src-tauri `PASTE_KEY_SEQ` 同形的注入序列（常量形状由命令层单测钉，
    /// 此处只需同形序列以便日志字面断言）
    const PASTE_SEQ: &[RawInput] = &[
        RawInput::KeyDown {
            vk: 0x11,
            scan: 0x1D,
        },
        RawInput::KeyDown {
            vk: 0x56,
            scan: 0x2F,
        },
        RawInput::KeyUp {
            vk: 0x56,
            scan: 0x2F,
        },
        RawInput::KeyUp {
            vk: 0x11,
            scan: 0x1D,
        },
    ];

    fn stacked_ids(h: &Harness) -> Vec<String> {
        h.module.stack_list().unwrap()
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-3）字面测试名优先于 rustc 命名惯例
    async fn stack_pasteNext_writesThenInjectsCtrlV() {
        let h = harness("stk_deliver").await;
        let store = h.module.store().unwrap();
        let id = store.insert_row(&NewClip::new("stack-first")).unwrap();
        assert_eq!(h.module.stack_push(&id).unwrap(), 1);

        let dto = h
            .module
            .stack_deliver_head(PASTE_SEQ)
            .unwrap()
            .expect("非空栈须有投递结果");
        assert_eq!(dto.id, id);
        assert!(dto.delivered, "注入成功即投递：{:?}", dto.error);
        assert_eq!(
            std::mem::take(&mut *h.log.lock()),
            vec![
                "write".to_string(),
                "inject:17,29;86,47;86,47,up;17,29,up".to_string()
            ],
            "写剪贴板必须先于按键注入，且四元素组合键完整"
        );
        assert_eq!(*h.port.writes.lock(), vec!["stack-first".to_string()]);
        assert!(stacked_ids(&h).is_empty(), "投递成功后队首出栈");
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    async fn stack_pasteNext_secretEntry_refusedAndStaysOnStack() {
        let h = harness("stk_secret").await;
        let store = h.module.store().unwrap();
        let id = store
            .insert_encrypted("aGVsbG8tY2lwaGVy", None, None, None, "local")
            .unwrap();
        h.module.stack_push(&id).unwrap();

        let dto = h
            .module
            .stack_deliver_head(PASTE_SEQ)
            .unwrap()
            .expect("敏感项须有如实回执，不是静默丢弃");
        assert!(!dto.delivered);
        assert_eq!(dto.error.as_deref(), Some("敏感条目需先揭示后粘贴"));
        assert!(
            h.log.lock().is_empty(),
            "敏感条目不得写剪贴板也不得注入：{:?}",
            h.log.lock()
        );
        assert_eq!(stacked_ids(&h), vec![id.clone()], "未投递的项必须仍在栈上");

        // 对照臂：明文项走同一路径正常投递，证明上一条拒的是内容语义而非端口没通
        let plain = store.insert_row(&NewClip::new("stack-not-secret")).unwrap();
        h.module.stack_push(&plain).unwrap();
        assert!(
            !h.module
                .stack_deliver_head(PASTE_SEQ)
                .unwrap()
                .expect("队首仍是敏感项")
                .delivered,
            "敏感项在队首即挡住队列——它不出栈，直到用户揭示或移出（T-B3-5 揭示门）"
        );
        assert!(h.module.stack_remove(&id).unwrap(), "移出敏感项须报成功");
        let ok = h
            .module
            .stack_deliver_head(PASTE_SEQ)
            .unwrap()
            .expect("明文项须投递");
        assert!(ok.delivered, "明文项投递失败：{:?}", ok.error);
        assert_eq!(ok.id, plain);
        assert!(stacked_ids(&h).is_empty());
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    async fn stack_pasteAll_stopsAtFirstInjectFailure() {
        let h = harness_injecting("stk_all", Some(2)).await;
        let store = h.module.store().unwrap();
        let ids: Vec<String> = ["stk-1", "stk-2", "stk-3"]
            .iter()
            .map(|t| store.insert_row(&NewClip::new(t)).unwrap())
            .collect();
        for id in &ids {
            h.module.stack_push(id).unwrap();
        }

        let report = h.module.stack_paste_all(Duration::ZERO, PASTE_SEQ).unwrap();
        assert_eq!(report.delivered, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.remaining, 1);
        assert_eq!(
            stacked_ids(&h),
            vec![ids[2].clone()],
            "首个失败之后的条目留在栈上，顺序不变"
        );
        assert_eq!(h.log.lock().len(), 4, "两条尝试各 write+inject");
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    async fn stack_pasteEmpty_returnsNone() {
        let h = harness("stk_empty").await;
        assert!(
            h.module.stack_deliver_head(PASTE_SEQ).unwrap().is_none(),
            "空栈不得假成功"
        );
        assert!(
            h.module
                .stack_paste_all(Duration::ZERO, PASTE_SEQ)
                .unwrap()
                .delivered
                == 0,
            "空栈全部粘贴返回 0"
        );
        assert!(h.log.lock().is_empty(), "空栈不得触碰任何端口");
    }

    // ---------------- T-B3-5 敏感库视图 + 按需揭示门 ----------------

    /// 假 CryptoPort 是恒等变换，故"密文列"= 明文的 base64；揭示即还原。
    fn secret_row(h: &Harness, plain: &str) -> String {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(plain);
        h.module
            .store()
            .unwrap()
            .insert_encrypted(&b64, None, None, None, "local")
            .unwrap()
    }

    fn plain_row(h: &Harness, text: &str) -> String {
        h.module
            .store()
            .unwrap()
            .insert_row(&NewClip::new(text))
            .unwrap()
    }

    /// 错误三要素拆开断言：码给程序、message 给用户、hint 给指路，任一错位都是契约破裂
    fn err_parts(e: AppError) -> (String, String, Option<String>) {
        match e {
            AppError::Module {
                code,
                message,
                hint,
            } => (code, message, hint),
            other => panic!("期望 Module 变体，实际: {other:?}"),
        }
    }

    /// 红线主件：通用读口对敏感行必须拒，且指路点名揭示口、错误里零明文。
    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-5）字面测试名优先于 rustc 命名惯例
    async fn get_secretRow_refusedWithRevealHint_notPlaintext() {
        let h = harness("reveal_gate").await;
        let plain = "sk-gate-8f2c19e0a1b2c3d4";
        let id = secret_row(&h, plain);

        let err = h.module.get_content(&id).unwrap_err();
        let (code, message, hint) = err_parts(err);
        assert_eq!(code, "CLIPBOARD_GET_001");
        assert!(
            !message.contains(plain),
            "错误消息本身不得夹带明文：{message}"
        );
        assert_eq!(
            hint.as_deref(),
            Some("请调用 clipboard_secret_reveal；揭示会写入宿主审计日志")
        );

        // 正对照：普通条目同口照常返回——否则上面那臂只是"整个读口坏了"
        let open = plain_row(&h, "plain-row-visible");
        assert_eq!(
            h.module.get_content(&open).unwrap().as_deref(),
            Some("plain-row-visible")
        );
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    async fn secretReveal_returnsPlaintextOnce() {
        let h = harness("reveal_roundtrip").await;
        let plain = "ghp_AAAABBBBccccDDDD1234567890abcdefghij";
        let id = secret_row(&h, plain);
        assert_eq!(
            h.module.reveal_secret(&id).unwrap(),
            plain,
            "揭示须还原原文"
        );
        assert!(
            h.module.get_content(&id).is_err(),
            "揭示一次不等于该条目从此随通用读口放行"
        );
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    async fn secretReveal_nonSecret_rejected001() {
        let h = harness("reveal_nonsecret").await;
        let open = plain_row(&h, "ordinary-text");
        let err = h.module.reveal_secret(&open).unwrap_err();
        let (code, _, hint) = err_parts(err);
        assert_eq!(code, "CLIPBOARD_REVEAL_001");
        assert_eq!(hint.as_deref(), Some("普通条目用 clipboard_get"));

        // 正对照：敏感行走同一函数成功，证明拒的是"非敏感"而非函数本身不通
        let secret = secret_row(&h, "AKIA1234567890ABCDEF12");
        assert!(h.module.reveal_secret(&secret).is_ok());

        // 不存在的条目 → 002（内容缺失）而非 001（非敏感）：两种失败必须分得开
        let missing = h.module.reveal_secret("no-such-entry").unwrap_err();
        assert_eq!(missing.code(), "CLIPBOARD_REVEAL_002");
    }

    /// 红线：搜索列表整棵 JSON 里既无明文也无其 base64 变体（掩码在 DTO 层，不靠前端不显示）
    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    async fn search_previewForSecret_neverContainsPlaintext() {
        use base64::Engine;
        let h = harness("reveal_search").await;
        let plain = "sk-search-must-not-leak-0042";
        let b64 = base64::engine::general_purpose::STANDARD.encode(plain);
        secret_row(&h, plain);

        let page = h
            .module
            .search(&crate::types::SearchQuery {
                group: Some("secret".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.items.len(), 1);
        let dump = serde_json::to_string(&page.items).unwrap();
        assert!(!dump.contains(plain), "列表泄漏明文：{dump}");
        assert!(!dump.contains(&b64), "列表泄漏密文原串：{dump}");
        assert!(page.items[0].secret);
        assert_eq!(
            page.items[0].preview,
            format!("[{}] 已加密存储", crate::types::SECRET_CATEGORY_LABEL)
        );
    }

    /// 既有拒语义回归不破，并顺手钉住「此口此前是死码」的修复：
    /// clipboard_paste 必须走 raw 载荷，否则 get_payload 的自动解密臂会让
    /// CLIPBOARD_PASTE_004 永远到不了，粘贴敏感行等于外泄明文。
    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    async fn paste_secretEntry_stillRefused004() {
        let h = harness("reveal_paste").await;
        let id = secret_row(&h, "sk-paste-stay-encrypted-778899");
        let err = h.module.paste_content(&id).unwrap_err();
        let (code, message, _) = err_parts(err);
        assert_eq!(code, "CLIPBOARD_PASTE_004");
        assert_eq!(message, "敏感条目需先揭示后粘贴");
        assert!(
            h.log.lock().is_empty() && h.port.writes.lock().is_empty(),
            "拒粘贴不得写系统剪贴板：{:?}",
            h.port.writes.lock()
        );

        // 正对照：普通条目同口解析成功（否则断言的是"谁都拒"）
        let open = plain_row(&h, "paste-visible");
        assert!(matches!(
            h.module.paste_content(&open).unwrap(),
            ClipContent::Text { ref text, .. } if text == "paste-visible"
        ));
    }

    /// 审计面存在性静态钉：日志捕获 crate 不在依赖面，无法断言"日志真被写出"，
    /// 于是钉更弱但可核的一件事——揭示成功路径里确实带着 `target: "audit"` 的 warn
    /// 调用（把它删掉或改了 target，本条即判红）。
    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    fn revealSecret_auditLogTargetPresent() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/module.rs"))
            .expect("读自身源码");
        let at = src
            .find("pub fn reveal_secret")
            .expect("揭示口须在册（明文唯一出口）");
        let after = &src[at..];
        let end = ["\n    pub fn ", "\n    fn ", "\n}"]
            .iter()
            .filter_map(|p| after.find(p))
            .min()
            .unwrap_or(after.len());
        assert!(
            after[..end].contains(r#"target: "audit""#),
            "明文出口必须落宿主审计日志"
        );
    }

    // ---------------- T-B3-8 HTML 捕获与格式粘贴 ----------------

    /// 写侧载荷字面：`paste_content_format` 返回的 `content` 就是命令层交给
    /// `write_back` 的那个值（假端口刻意不扩成"记录载荷"，断言打在唯一的真源上）。
    fn html_row(h: &Harness, text: &str, html: &str) -> String {
        h.module
            .store()
            .unwrap()
            .insert_row(&NewClip::new(text).html(html))
            .unwrap()
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-8）字面测试名优先于 rustc 命名惯例
    async fn pasteFormat_plain_writesTextWithoutHtml() {
        let h = harness("fmt_plain").await;
        let id = html_row(&h, "富文本纯粘贴", "<p><b>富文本纯粘贴</b></p>");

        // 前置：这一行确实带 HTML，否则"不带格式"只是什么都没存
        assert!(
            h.module.get_html(&id).unwrap().is_some(),
            "夹具须真的有 HTML 正文"
        );

        let out = h.module.paste_content_format(&id, false).unwrap();
        assert_eq!(out.format_used, "plain");
        assert!(!out.degraded, "没要格式就谈不上降级");
        assert!(
            matches!(&out.content, ClipContent::Text { text, html } if text == "富文本纯粘贴" && html.is_none()),
            "plain 分支投出的 Text 必须 html: None，实际: {:?}",
            out.content
        );

        // 正对照：同一行改口要格式即带出 HTML——证明上一臂拒的是格式参数而非这一行
        let rich = h.module.paste_content_format(&id, true).unwrap();
        assert_eq!(rich.format_used, "html");
        assert!(matches!(
            &rich.content,
            ClipContent::Text { html: Some(_), .. }
        ));
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-8）字面测试名优先于 rustc 命名惯例
    async fn pasteFormat_html_degradesToPlainWhenAbsent_andReports() {
        let h = harness("fmt_degrade").await;
        let id = plain_row(&h, "只有纯文本的一行");

        let out = h.module.paste_content_format(&id, true).unwrap();
        assert_eq!(
            out.format_used, "plain",
            "投出去的是纯文本就不许自称 html（写进系统剪贴板的即断言的那个值）"
        );
        assert!(out.degraded, "缺格式而回落必须如实上报，不得静默成功");
        assert!(matches!(
            &out.content,
            ClipContent::Text { text, html } if text == "只有纯文本的一行" && html.is_none()
        ));

        // 正对照：同一函数在有 HTML 的行上 degraded=false（见 pasteFormat_html_carriesHtmlWhenPresent），
        // 这里另钉一条边界——敏感行不适用"降级"：它的纯文本本身就是明文，
        // 静默回落成 plain 等于给带格式粘贴开第二明文出口，故须整口拒。
        let secret = secret_row(&h, "sk-degrade-must-not-00112233445566");
        let err = match h.module.paste_content_format(&secret, true) {
            Ok(_) => panic!("敏感行不得因「带格式」参数而放行"),
            Err(e) => e,
        };
        assert_eq!(err_parts(err).0, "CLIPBOARD_PASTE_004");
        assert!(h.port.writes.lock().is_empty(), "拒粘贴不得写系统剪贴板");
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-8）字面测试名优先于 rustc 命名惯例
    async fn pasteFormat_html_carriesHtmlWhenPresent() {
        let h = harness("fmt_html").await;
        let id = html_row(
            &h,
            "带格式的正文",
            "<h1>带格式的正文</h1><ul><li>一</li></ul>",
        );

        let out = h.module.paste_content_format(&id, true).unwrap();
        assert_eq!(out.format_used, "html");
        assert!(!out.degraded);
        assert!(
            matches!(
                &out.content,
                ClipContent::Text { text, html: Some(src) }
                    if text == "带格式的正文" && src == "<h1>带格式的正文</h1><ul><li>一</li></ul>"
            ),
            "HTML 须逐字节带出，实际: {:?}",
            out.content
        );

        // 对立面：同一条目走 plain 不带 HTML（两分支同源不同投，互不污染）
        assert!(matches!(
            h.module.paste_content_format(&id, false).unwrap().content,
            ClipContent::Text { html: None, .. }
        ));
    }

    // ---------------- T-B3-9 加密导出/导入 ----------------

    fn all_rows(h: &Harness) -> Vec<crate::types::ClipEntry> {
        h.module
            .store()
            .unwrap()
            .search(&crate::types::SearchQuery::default())
            .unwrap()
            .items
    }

    fn err_code(e: &AppError) -> String {
        match e {
            AppError::Module { code, .. } | AppError::Storage { code, .. } => code.clone(),
            other => format!("其他: {other}"),
        }
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-9）字面测试名优先于 rustc 命名惯例
    async fn exportImport_roundtrip_underPassphrase_reencryptsSecrets() {
        let h = harness("bk_roundtrip").await;
        let plain = html_row(&h, "备份往返正文", "<p><b>备份往返正文</b></p>");
        secret_row(&h, "sk-令牌-12345");
        h.module.store().unwrap().pin(&plain, true).unwrap();

        let (path, meta) = h
            .module
            .export_backup("十六字符以上的备份口令", true)
            .unwrap();
        assert_eq!(meta.entries, 2, "两行都在库内自持正文里，都该进备份");
        assert_eq!(meta.secrets, 1);
        assert_eq!(meta.images_skipped, 0);
        assert!(path.exists(), "导出须真落盘");

        h.module.store().unwrap().clear(false).unwrap();
        assert_eq!(all_rows(&h).len(), 0, "清库后须真空，否则下面的断言会空洞");

        let report = h
            .module
            .import_backup(&path, "十六字符以上的备份口令")
            .unwrap();
        assert_eq!(report.imported, 2);
        assert_eq!(report.duplicates, 0);
        assert_eq!(report.secrets, 1);

        let rows = all_rows(&h);
        let restored = rows
            .iter()
            .find(|e| e.preview == "备份往返正文")
            .expect("普通行须按原文回来");
        assert!(restored.pinned, "置顶是用户的显式意图，随备份回来");
        assert!(
            restored.has_html,
            "T-B3-8 的 HTML 正文同在备份里，须一并回来"
        );
        let secret = rows.iter().find(|e| e.secret).expect("敏感行须回来");
        assert_eq!(
            secret.preview,
            format!("[{}] 已加密存储", crate::types::SECRET_CATEGORY_LABEL),
            "回来的是重新封信封的敏感行，不是一行普通明文（预览仍掩码）"
        );
        assert_eq!(
            h.module.reveal_secret(&secret.id).unwrap(),
            "sk-令牌-12345",
            "重加密后揭示必须得回原明文"
        );
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn export_noPlaintextSecretInFileBytes() {
        let h = harness("bk_no_plain").await;
        secret_row(&h, "sk-超级机密-abcdef");
        html_row(&h, "同批导出的普通正文", "<i>普通</i>");

        let (path, _) = h.module.export_backup("口令十六字符以上OK", true).unwrap();
        let raw = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&raw).to_lowercase();
        // 明文与其 base64 变体都不许出现（后者正是"只换个编码当加密"的那种假安全）
        for needle in [
            "sk-超级机密-abcdef",
            &host_core::util::b64_encode("sk-超级机密-abcdef".as_bytes()),
            "同批导出的普通正文",
        ] {
            assert!(
                !text.contains(&needle.to_lowercase()),
                "导出文件字节里不得出现「{needle}」"
            );
        }
        // 反空洞：同一份文件用正确口令能解出上面这些明文——grep 判绿是因为加密生效，
        // 不是因为里面本来什么都没有。
        let env = crate::backup::decode_envelope(&raw).unwrap();
        assert_eq!(env.meta.secrets, 1, "敏感行确实被收进了这份备份");
        let json = crate::backup::decrypt_backup(&env, "口令十六字符以上OK").unwrap();
        let inner = String::from_utf8(json).unwrap();
        // 内层明文里敏感行是 base64 形态（BackupRow::secret_b64），两种形态都要在：
        // grep 判绿因此只可能是加密的功劳，而不是"什么都没导出"。
        assert!(
            inner.contains(&host_core::util::b64_encode(
                "sk-超级机密-abcdef".as_bytes()
            )),
            "内层明文须含敏感原文的 base64 形态"
        );
        assert!(inner.contains("同批导出的普通正文"));
        assert!(
            crate::backup::decrypt_backup(&env, "错一个字符也不行").is_err(),
            "口令错即解不开（正对照：上一条同一份文件用对口令解得开）"
        );
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn export_secretsWithoutPassphrase_rejected001() {
        let h = harness("bk_gate").await;
        secret_row(&h, "sk-需要口令才走");
        let e = h
            .module
            .export_backup("", true)
            .expect_err("含敏感行而口令为空须拒");
        assert_eq!(err_code(&e), "CLIPBOARD_EXPORT_001");
        assert!(
            !h.dir.join("export").exists()
                || std::fs::read_dir(h.dir.join("export"))
                    .unwrap()
                    .filter_map(|p| p.ok())
                    .filter(|p| p.file_name().to_string_lossy().ends_with(".nfclip.json"))
                    .count()
                    == 0,
            "拒就要拒在写盘之前，不许留下一份残缺备份"
        );
        // 正对照：同一模块不要敏感行时门不立（口径是"含敏感才要口令"，不是万能口令门）
        let (path, meta) = h.module.export_backup("", false).unwrap();
        assert_eq!(meta.entries, 0, "无敏感行请求时敏感行整行不出门");
        assert_eq!(meta.secrets, 0);
        assert!(path.exists());
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn import_wrongPassphrase_rejectedAndStoreUntouched() {
        let h = harness("bk_wrong").await;
        html_row(&h, "口令错一行都不落", "<b>x</b>");
        let (path, _) = h
            .module
            .export_backup("正确口令十六字符以上", false)
            .unwrap();
        h.module.store().unwrap().clear(false).unwrap();

        let e = h
            .module
            .import_backup(&path, "错误口令十六字符以上")
            .expect_err("AEAD 失败须报错");
        assert_eq!(err_code(&e), "CLIPBOARD_IMPORT_001");
        assert_eq!(
            all_rows(&h).len(),
            0,
            "认证失败前不得落任何一行（半套导入比不导入更坏）"
        );
        // 正对照：同一文件同一库，对口令能落
        assert_eq!(
            h.module
                .import_backup(&path, "正确口令十六字符以上")
                .unwrap()
                .imported,
            1
        );
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn import_twice_isIdempotentByHash() {
        let h = harness("bk_twice").await;
        html_row(&h, "重导不产生第二份", "<i>a</i>");
        html_row(&h, "另一行普通正文", "<i>b</i>");
        secret_row(&h, "sk-重导会多出来");
        let (path, _) = h.module.export_backup("口令十六字符以上!!", true).unwrap();
        h.module.store().unwrap().clear(false).unwrap();

        let first = h.module.import_backup(&path, "口令十六字符以上!!").unwrap();
        assert_eq!(first.imported, 3);
        assert_eq!(first.duplicates, 0);
        let after_first = all_rows(&h).len();
        assert_eq!(after_first, 3);

        let second = h.module.import_backup(&path, "口令十六字符以上!!").unwrap();
        assert_eq!(second.imported, 1, "只有敏感行会新增（见下）");
        assert_eq!(second.duplicates, 2, "两行普通正文按 content_hash 命中");
        assert_eq!(all_rows(&h).len(), 4);
        // 如实记录一条语义而非掩盖它：库内敏感行的哈希是**密文**的哈希，
        // 每次 protect 产出新密文，故 content_hash 去重在敏感行上结构性失效。
        assert_eq!(
            all_rows(&h).iter().filter(|e| e.secret).count(),
            2,
            "敏感行重导会各留一份——不是幂等漏洞，是密文哈希去重的必然"
        );
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn backup_truncatedOrForeignJson_rejected003() {
        let h = harness("bk_format").await;
        html_row(&h, "格式负例的正对照正文", "<p>x</p>");
        let (good, _) = h.module.export_backup("口令十六字符以上!!", false).unwrap();
        let good_bytes = std::fs::read(&good).unwrap();

        let foreign = h.dir.join("foreign.json");
        std::fs::write(&foreign, r#"{"proxies":["不是备份"]}"#).unwrap();
        let e = h.module.import_backup(&foreign, "任意").unwrap_err();
        assert_eq!(err_code(&e), "CLIPBOARD_IMPORT_003");

        let truncated = h.dir.join("truncated.json");
        std::fs::write(&truncated, &good_bytes[..good_bytes.len() / 3]).unwrap();
        assert_eq!(
            err_code(
                &h.module
                    .import_backup(&truncated, "口令十六字符以上!!")
                    .expect_err("截断的信封须在解析外壳时就拒")
            ),
            "CLIPBOARD_IMPORT_003"
        );

        let newer: serde_json::Value = {
            let mut v: serde_json::Value = serde_json::from_slice(&good_bytes).unwrap();
            v["schema"] = serde_json::json!(99);
            v
        };
        let future = h.dir.join("future.json");
        std::fs::write(&future, serde_json::to_vec(&newer).unwrap()).unwrap();
        let e = h
            .module
            .import_backup(&future, "口令十六字符以上!!")
            .unwrap_err();
        assert_eq!(err_code(&e), "CLIPBOARD_IMPORT_003");
        match e {
            AppError::Module { message, .. } => assert!(
                message.contains("99"),
                "版本不符须点名收到的版本号：{message}"
            ),
            other => panic!("预期 Module 型错误，得到 {other:?}"),
        }

        // 第四形：密文一格未动，只把密文之外的明文计数改掉 —— 口令仍然解得开，
        // 故须由 entries 与行数的互校兜住（否则"少了几行"的备份会被当作导入成功）。
        let miscounted: serde_json::Value = {
            let mut v: serde_json::Value = serde_json::from_slice(&good_bytes).unwrap();
            v["meta"]["entries"] = serde_json::json!(7);
            v
        };
        let padded = h.dir.join("miscounted.json");
        std::fs::write(&padded, serde_json::to_vec(&miscounted).unwrap()).unwrap();
        let e = h
            .module
            .import_backup(&padded, "口令十六字符以上!!")
            .expect_err("声明行数与实际不符须拒");
        assert_eq!(err_code(&e), "CLIPBOARD_IMPORT_003");
        match e {
            AppError::Module { message, .. } => assert!(
                message.contains("7") && message.contains("1"),
                "须点名声明数与实际数两侧：{message}"
            ),
            other => panic!("预期 Module 型错误，得到 {other:?}"),
        }

        // 正对照：同一份未改动的文件走同一入口即成功（四枚拒的是格式，不是导入通路本身）
        h.module.store().unwrap().clear(false).unwrap();
        assert_eq!(
            h.module
                .import_backup(&good, "口令十六字符以上!!")
                .unwrap()
                .imported,
            1
        );
    }
}
