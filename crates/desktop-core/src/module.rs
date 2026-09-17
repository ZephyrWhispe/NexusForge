//! DesktopModule 模块壳（docs/impl/05 D）：Module + HotkeyProvider。
//!
//! - init：打开 NoteStore + 构建启动器索引（同步扫描，bootstrap 已在后台线程调用）+ 注册内置动作
//! - start：启动提醒轮询线程（30s take_due → desktop.remind_due 事件）
//! - 快捷键：Alt+Q 呼出启动器 / Ctrl+Alt+N 呼出速记条（事件 → 前端建窗，同 quickpanel 模式）

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use host_core::capability::{HotkeyAction, HotkeyBinding, HotkeyProvider};
use host_core::error::ModuleError;
use host_core::events::{Event, EventBus};
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};
use host_core::ports::ShellPort;

use crate::index::{LauncherIndex, ItemKind};
use crate::note::NoteStore;

/// 提醒轮询间隔
const REMIND_POLL_MS: u64 = 30_000;

pub struct DesktopModule {
    state: AtomicU8,
    bus: RwLock<Option<Arc<EventBus>>>,
    notes: RwLock<Option<Arc<NoteStore>>>,
    shell: RwLock<Option<Arc<dyn ShellPort>>>,
    index: Arc<LauncherIndex>,
    desktop_dir: PathBuf,
    /// app data 目录（manifest/db 等派生路径的根）
    app_data_dir: PathBuf,
    /// 提醒线程取消标志（true = 停止）
    remind_cancel: Arc<std::sync::atomic::AtomicBool>,
    remind_thread: RwLock<Option<std::thread::JoinHandle<()>>>,
}

impl DesktopModule {
    pub fn new(app_data_dir: &std::path::Path) -> Self {
        let index = Arc::new(LauncherIndex::new(
            app_data_dir.join("desktop").join("usage.json"),
        ));
        Self {
            state: AtomicU8::new(0),
            bus: RwLock::new(None),
            notes: RwLock::new(None),
            shell: RwLock::new(None),
            index,
            desktop_dir: desktop_dir_path(),
            app_data_dir: app_data_dir.to_path_buf(),
            remind_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            remind_thread: RwLock::new(None),
        }
    }

    /// IPC 层入口
    pub fn index(&self) -> &Arc<LauncherIndex> {
        &self.index
    }

    pub fn note_store(&self) -> Option<Arc<NoteStore>> {
        self.notes.read().ok().and_then(|g| g.clone())
    }

    pub fn desktop_dir(&self) -> &std::path::Path {
        &self.desktop_dir
    }

    pub fn tidy_planner(&self) -> crate::tidy::TidyPlanner {
        crate::tidy::TidyPlanner::new(
            self.app_data_dir.join("desktop").join("tidy_manifest.json"),
        )
    }

    /// launch：App → ShellExecuteW；Action → 发事件。均记频次。
    pub fn launch(&self, id: &str) -> Result<(), ModuleError> {
        let item = self.index.get(id).map_err(|e| ModuleError::Init(e.to_string()))?;
        self.index.record_launch(id);
        let bus = self.bus.read().ok().and_then(|g| g.clone());
        match item.kind {
            ItemKind::App => {
                let shell = self
                    .shell
                    .read()
                    .ok()
                    .and_then(|g| g.clone())
                    .ok_or_else(|| ModuleError::Init("ShellPort 未注册".into()))?;
                shell
                    .shell_execute(&item.path)
                    .map_err(|e| ModuleError::Init(e.to_string()))?;
            }
            ItemKind::Action => {
                // 动作 = 发对应模块主题事件（截图/OCR/剪贴板面板已在该模块内实现响应）
                if let (Some(bus), Some(topic)) = (bus, item.topic) {
                    bus.publish(Event::new(topic, "desktop", item.payload.unwrap_or_default())).ok();
                }
            }
        }
        Ok(())
    }

    /// 提醒轮询线程（start 在 spawn_blocking 内被调用，无 tokio 上下文 → std::thread）
    fn start_remind_loop(&self) {
        let already = self.remind_cancel.swap(false, Ordering::SeqCst);
        if already && self.remind_thread.read().map(|g| g.is_some()).unwrap_or(false) {
            // 线程仍在跑（cancel 复位即可），不重复拉起
            return;
        }
        let cancel = self.remind_cancel.clone();
        let notes = self.note_store();
        let bus = self.bus.read().ok().and_then(|g| g.clone());
        let (Some(notes), Some(bus)) = (notes, bus) else {
            return;
        };
        let handle = std::thread::spawn(move || {
            loop {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(REMIND_POLL_MS));
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                match notes.take_due(now) {
                    Ok(due) => {
                        for n in due {
                            bus.publish(Event::new(
                                "desktop.remind_due",
                                "desktop",
                                serde_json::json!({
                                    "id": n.id,
                                    "content": n.content,
                                    "remind_at": n.remind_at,
                                    "tags": n.tags,
                                }),
                            ))
                            .ok();
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "提醒轮询失败"),
                }
            }
        });
        *self.remind_thread.write().expect("提醒线程句柄写锁") = Some(handle);
    }
}

/// 当前用户桌面目录（无环境变量时回退主目录）
fn desktop_dir_path() -> PathBuf {
    if let Ok(up) = std::env::var("USERPROFILE") {
        let d = PathBuf::from(&up).join("Desktop");
        if d.is_dir() {
            // 中文系统实际桌面可能被重定向到 OneDrive，检测注册表成本高；
            // Windows 真实桌面以 SHGetKnownFolderPath 为准（win-integration 后续补 Port），
            // v1 优先用 USERPROFILE\Desktop，若不存在回退 OneDrive\Desktop
            return d;
        }
        let od = PathBuf::from(&up).join("OneDrive").join("Desktop");
        if od.is_dir() {
            return od;
        }
    }
    PathBuf::from(".")
}

impl Module for DesktopModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "desktop",
            name: "桌面效率",
            version: "0.1.0",
            icon: Some("desktop"),
            priority: 16,
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let notes = NoteStore::open(&ctx.app_data_dir.join("db").join("desktop.db"))
            .map_err(|e| ModuleError::Storage(e.to_string()))?;
        *self.notes.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(Arc::new(notes));
        *self.bus.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(ctx.event_bus.clone());
        *self.shell.write().map_err(|_| ModuleError::Init("锁污染".into()))? = ctx.ports.get::<dyn ShellPort>();

        // D1：同步构建索引（bootstrap_modules 在后台任务里调用 init，不阻塞 UI）
        let total = self
            .index
            .build(&crate::index::start_menu_dirs(), &crate::index::path_dirs());
        tracing::info!(total, "启动器索引构建完成");

        // 内置动作（docs/impl/05 D1：剪贴板/截图/OCR 快捷入口）
        self.index.register_action(
            "clipboard_panel",
            "剪切板面板",
            "clipboard.quick_panel_toggled",
            serde_json::json!({}),
        );
        self.index.register_action(
            "screenshot",
            "截图",
            "screenshot.overlay_requested",
            serde_json::json!({ "mode": "shot" }),
        );
        self.index.register_action(
            "ocr",
            "文字识别 (OCR)",
            "screenshot.overlay_requested",
            serde_json::json!({ "mode": "ocr" }),
        );

        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        self.start_remind_loop();
        self.state.store(2, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        self.remind_cancel.store(true, Ordering::SeqCst);
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "launcher_hotkey_note": {
                    "type": "string", "title": "快捷键说明",
                    "description": "启动器 Alt+Q；速记条 Ctrl+Alt+N（v1 固定，后续可配置）",
                    "default": ""
                }
            }
        })
    }

    fn apply_config(&self, _values: serde_json::Value) -> Result<(), ModuleError> {
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

impl HotkeyProvider for DesktopModule {
    fn global_hotkeys(&self) -> Vec<HotkeyBinding> {
        vec![
            HotkeyBinding {
                id: "desktop.launcher_toggle".into(),
                label: "快速启动器".into(),
                // MOD_ALT(0x01) | MOD_NOREPEAT(0x4000)
                modifiers: 0x01 | 0x4000,
                vk: 0x51, // 'Q'
            },
            HotkeyBinding {
                id: "desktop.note_quick".into(),
                label: "快速速记".into(),
                // MOD_CONTROL(0x02) | MOD_ALT(0x01) | MOD_NOREPEAT(0x4000)
                modifiers: 0x02 | 0x01 | 0x4000,
                vk: 0x4E, // 'N'
            },
        ]
    }

    fn hotkey_actions(&self) -> Vec<HotkeyAction> {
        let Some(bus) = self.bus.read().ok().and_then(|g| g.clone()) else {
            return vec![];
        };
        let bus1 = bus.clone();
        let bus2 = bus;
        vec![
            HotkeyAction {
                binding_id: "desktop.launcher_toggle".into(),
                action: Arc::new(move || {
                    bus1.publish(Event::new("desktop.launcher_toggled", "desktop", serde_json::json!({}))).ok();
                }),
            },
            HotkeyAction {
                binding_id: "desktop.note_quick".into(),
                action: Arc::new(move || {
                    bus2.publish(Event::new("desktop.note_quick", "desktop", serde_json::json!({}))).ok();
                }),
            },
        ]
    }
}
