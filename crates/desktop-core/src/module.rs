//! DesktopModule 模块壳（docs/impl/05 D）：Module + HotkeyProvider。
//!
//! - init：打开 NoteStore + 构建启动器索引（同步扫描，bootstrap 已在后台线程调用）+ 注册内置动作
//! - start：启动提醒轮询线程（30s take_due → desktop.remind_due 事件）
//! - 快捷键：Alt+Q 呼出启动器 / Ctrl+Alt+N 呼出速记条（事件 → 前端建窗，同 quickpanel 模式）

use parking_lot::RwLock;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use host_core::capability::{HotkeyAction, HotkeyBinding, HotkeyProvider};
use host_core::error::ModuleError;
use host_core::events::{Event, EventBus};
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};
use host_core::ports::ShellPort;

use crate::index::{ItemKind, LauncherIndex};
use crate::note::NoteStore;

/// 提醒轮询间隔
const REMIND_POLL_MS: u64 = 30_000;

pub struct DesktopModule {
    state: ModuleStateCell,
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
            state: ModuleStateCell::new(),
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
        self.notes.read().clone()
    }

    pub fn desktop_dir(&self) -> &std::path::Path {
        &self.desktop_dir
    }

    pub fn tidy_planner(&self) -> crate::tidy::TidyPlanner {
        crate::tidy::TidyPlanner::new(self.app_data_dir.join("desktop").join("tidy_manifest.json"))
    }

    /// launch：App → ShellExecuteW；Action → 发事件。均记频次。
    pub fn launch(&self, id: &str) -> Result<(), ModuleError> {
        let item = self
            .index
            .get(id)
            .map_err(|e| ModuleError::Init(e.to_string()))?;
        self.index.record_launch(id);
        let bus = self.bus.read().clone();
        match item.kind {
            ItemKind::App => {
                let shell = self
                    .shell
                    .read()
                    .clone()
                    .ok_or_else(|| ModuleError::Init("ShellPort 未注册".into()))?;
                shell
                    .shell_execute(&item.path)
                    .map_err(|e| ModuleError::Init(e.to_string()))?;
            }
            ItemKind::Action => {
                // 动作 = 发对应模块主题事件（截图/OCR/剪贴板面板已在该模块内实现响应）
                if let (Some(bus), Some(topic)) = (bus, item.topic) {
                    bus.publish(Event::new(
                        topic,
                        "desktop",
                        item.payload.unwrap_or_default(),
                    ))
                    .ok();
                }
            }
        }
        Ok(())
    }

    /// 注册内置动作（init 与 reindex 共用；build() 整体替换条目，重放是唯一保留路径）
    fn register_builtin_actions(&self) {
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
    }

    /// D1：重建启动器索引（UI「重建索引」入口）。返回 App 条目数（不含内置动作）。
    pub fn reindex(&self) -> usize {
        self.reindex_with(&crate::index::start_menu_dirs(), &crate::index::path_dirs())
    }

    /// 目录注入版（测试确定性）：build 会整体替换条目，故随后必须重放内置动作
    pub fn reindex_with(&self, start_menu: &[PathBuf], path_dirs: &[PathBuf]) -> usize {
        let total = self.index.build(start_menu, path_dirs);
        self.register_builtin_actions();
        tracing::info!(total, "启动器索引重建完成");
        total
    }

    /// 提醒轮询线程（start 在 spawn_blocking 内被调用，无 tokio 上下文 → std::thread）
    fn start_remind_loop(&self) {
        let already = self.remind_cancel.swap(false, Ordering::SeqCst);
        if already && self.remind_thread.read().is_some() {
            // 线程仍在跑（cancel 复位即可），不重复拉起
            return;
        }
        let cancel = self.remind_cancel.clone();
        let notes = self.note_store();
        let bus = self.bus.read().clone();
        let (Some(notes), Some(bus)) = (notes, bus) else {
            return;
        };
        let handle = std::thread::spawn(move || loop {
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
        });
        *self.remind_thread.write() = Some(handle);
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
            priority: priority_of("desktop"),
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let notes = NoteStore::open(&ctx.app_data_dir.join("db").join("desktop.db"))
            .map_err(|e| ModuleError::Storage(e.to_string()))?;
        *self.notes.write() = Some(Arc::new(notes));
        *self.bus.write() = Some(ctx.event_bus.clone());
        *self.shell.write() = ctx.ports.get::<dyn ShellPort>();

        // D1：同步构建索引（bootstrap_modules 在后台任务里调用 init，不阻塞 UI）
        let total = self
            .index
            .build(&crate::index::start_menu_dirs(), &crate::index::path_dirs());
        tracing::info!(total, "启动器索引构建完成");

        // 内置动作（docs/impl/05 D1：剪贴板/截图/OCR 快捷入口）
        self.register_builtin_actions();

        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        self.start_remind_loop();
        self.state.set(ModuleState::Running);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        self.remind_cancel.store(true, Ordering::SeqCst);
        self.state.set(ModuleState::Stopped);
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
        self.state.get()
    }

    fn set_status(&self, state: ModuleState) {
        self.state.set(state);
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
        let Some(bus) = self.bus.read().clone() else {
            return vec![];
        };
        let bus1 = bus.clone();
        let bus2 = bus;
        vec![
            HotkeyAction {
                binding_id: "desktop.launcher_toggle".into(),
                action: Arc::new(move || {
                    bus1.publish(Event::new(
                        "desktop.launcher_toggled",
                        "desktop",
                        serde_json::json!({}),
                    ))
                    .ok();
                }),
            },
            HotkeyAction {
                binding_id: "desktop.note_quick".into(),
                action: Arc::new(move || {
                    bus2.publish(Event::new(
                        "desktop.note_quick",
                        "desktop",
                        serde_json::json!({}),
                    ))
                    .ok();
                }),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 红线回归（09 §4.2 T-B1-6）：build() 整体替换条目，重放是内置动作的唯一保留路径
    #[test]
    fn launcher_reindex_preserves_registered_actions() {
        let dir = std::env::temp_dir().join(format!("nf_desktop_reindex_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sm = dir.join("sm");
        std::fs::create_dir_all(&sm).unwrap();
        std::fs::write(sm.join("记事本.lnk"), b"fake-lnk").unwrap();

        let m = DesktopModule::new(&dir);
        let total = m.reindex_with(std::slice::from_ref(&sm), &[]);
        assert_eq!(total, 1, "返回 App 条目数（不含内置动作）");

        for id in ["action:clipboard_panel", "action:screenshot", "action:ocr"] {
            assert!(m.index().get(id).is_ok(), "reindex 后内置动作丢失: {id}");
        }
        let hits = m.index().search("截图", 10).unwrap();
        let hit = hits
            .iter()
            .find(|h| h.item.id == "action:screenshot")
            .expect("search(截图) 未命中内置截图动作");
        assert_eq!(hit.item.kind, ItemKind::Action);
        assert_eq!(hit.item.topic, Some("screenshot.overlay_requested"));
        assert_eq!(hit.item.payload.as_ref().unwrap()["mode"], "shot");

        // 对照臂：裸 build() 不重放确实会抹掉动作——证明上述判据由重放兑现而非巧合
        m.index().build(std::slice::from_ref(&sm), &[]);
        assert!(
            m.index().get("action:ocr").is_err(),
            "裸 build 后动作仍在说明判据是摆设"
        );

        // 再次 reindex：动作恢复且不重复累积（register_action 覆盖语义）
        assert_eq!(m.reindex_with(&[sm], &[]), 1);
        let (ready, n) = m.index().status();
        assert!(ready);
        assert_eq!(n, 4, "1 App + 3 Action，重放不重复");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
