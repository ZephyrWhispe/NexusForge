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

use crate::config::DesktopConfig;
use crate::index::{ItemKind, LauncherIndex};
use crate::note::NoteStore;
use crate::tidy::TidyMapping;

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
    /// T-B7-16 整理映射运行态真源（Arc 下发 TidyPlanner；apply_config 覆写=不重启）
    tidy_map: Arc<RwLock<Option<TidyMapping>>>,
    /// 最近一次生效的 desktop 段配置（merged 派发基底，盘上镜像；映射真源在 tidy_map）
    cfg: RwLock<DesktopConfig>,
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
            tidy_map: Arc::new(RwLock::new(None)),
            cfg: RwLock::new(DesktopConfig::default()),
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
        crate::tidy::TidyPlanner::new(
            self.app_data_dir.join("desktop").join("tidy_manifest.json"),
            self.tidy_map.clone(),
        )
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
        // 键集 ≡ DesktopConfig 字段集（死键守卫测钉死）。T-B7-16：
        // launcher_hotkey_note 死键除名（纯说明文案无读者，描述已并入本 title/description）；
        // tidy_map readOnly=SchemaForm 不渲染，写口唯一在 DesktopPanel 映射编辑表
        // （Rust 侧 schema + validate 双层拒存点名）。
        serde_json::json!({
            "type": "object",
            "properties": {
                "tidy_map": {
                    "type": ["object", "null"],
                    "title": "桌面整理自定义分类映射",
                    "description": "null=内置六类；每行 [类名, 目标夹(带盘符绝对路径，拒相对/UNC/引号), 扩展名数组]，声明序即优先序（编辑口在桌面效率面板整理区）",
                    "default": null,
                    "readOnly": true,
                    "required": ["categories"],
                    "additionalProperties": false,
                    "properties": {
                        "categories": {
                            "type": "array",
                            "items": {
                                "type": "array",
                                "minItems": 3,
                                "maxItems": 3,
                                "prefixItems": [
                                    { "type": "string", "minLength": 1 },
                                    { "type": "string", "pattern": "^[A-Za-z]:[\\\\/][^\"'<>|?]*$" },
                                    { "type": "array", "minItems": 1, "items": { "type": "string", "pattern": "^[A-Za-z0-9_-]+$" } }
                                ]
                            }
                        }
                    }
                }
            }
        })
    }

    fn apply_config(&self, values: serde_json::Value) -> Result<(), ModuleError> {
        // merged 三件套派发（同 kvm T-B7-8 形制）：缺键不动；坏值整批点名拒收；
        // 映射变更经共享 Arc 直达 TidyPlanner——不重启生效
        let base = self.cfg.read().clone();
        let next = base.merged(&values).map_err(ModuleError::Config)?;
        if next.tidy_map != base.tidy_map {
            *self.tidy_map.write() = next.tidy_map.clone();
            tracing::info!("桌面整理分类映射已热更新（不重启生效）");
        }
        *self.cfg.write() = next;
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
    use host_core::config::ConfigStore;

    /// 自闭合夹具目录（pid 会被复用，补纳秒盐并先清场）
    fn tmp_dir(tag: &str) -> PathBuf {
        let salt = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let dir =
            std::env::temp_dir().join(format!("nf-desktop-{tag}-{}-{salt}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-16）字面测试名优先于 rustc 命名惯例
    fn tidyMap_runtimeApply_noRestart() {
        let dir = tmp_dir("tidyapply");
        let bus = Arc::new(EventBus::new());
        let m = DesktopModule::new(&dir);
        let store = Arc::new(ConfigStore::new(dir.join("config"), bus.clone()));
        store.register_schema("desktop", m.config_schema());
        // planner 在配置生效之前创建：热更新必须走同一 Arc，而非换 planner（重建=重启语义）
        let p0 = m.tidy_planner();

        let mapping = serde_json::json!({
            "categories": [["设计稿", "D:\\Design", ["psd", "sketch"]]]
        });
        // ① 真写侧：store.set_module schema 门放行 → 盘上 → apply_config 派发
        store
            .set_module(
                "desktop",
                serde_json::json!({ "tidy_map": mapping.clone() }),
            )
            .unwrap();
        let disk = store.get_module("desktop").unwrap();
        m.apply_config(disk).unwrap();
        let want = TidyMapping {
            categories: vec![(
                "设计稿".into(),
                "D:\\Design".into(),
                vec!["psd".into(), "sketch".into()],
            )],
        };
        assert_eq!(*m.tidy_map.read(), Some(want.clone()));
        // ② 派发证明：旧 planner 持同一 Arc 且立即可见（不重启生效）
        assert!(
            Arc::ptr_eq(&m.tidy_map, p0.map_ref()),
            "配置生效不得以换代 TidyPlanner 为代价——B6 T-B6-6 Arc::ptr_eq 判据形制"
        );
        assert_eq!(*p0.map_ref().read(), Some(want.clone()));

        // ③ 运行态坏值整批弹回点名（绕过 store 直灌派发口的防御臂）
        let e = m
            .apply_config(
                serde_json::json!({ "tidy_map": { "categories": [["x", "Docs", ["pdf"]]] } }),
            )
            .unwrap_err();
        assert!(e.to_string().contains("绝对路径"), "拒因点名: {e}");
        assert_eq!(*m.tidy_map.read(), Some(want), "弹回批不得连坐运行态");

        // ④ schema 门独立臂：相对路径与 UNC 都进不了盘（store 层拒存，不必到模块）
        for bad in ["Docs", "\\\\nas\\share"] {
            let e = store
                .set_module(
                    "desktop",
                    serde_json::json!({ "tidy_map": { "categories": [["x", bad, ["pdf"]]] } }),
                )
                .unwrap_err();
            assert!(
                e.to_string().contains("配置校验失败"),
                "schema 门必须拦下非法目标夹: {e}"
            );
        }
        assert_eq!(
            store.get_module("desktop").unwrap()["tidy_map"],
            mapping,
            "拒存批不得污染盘上旧值"
        );

        // ⑤ 显式 null 回内置（经真写侧全链路）
        store
            .set_module("desktop", serde_json::json!({ "tidy_map": null }))
            .unwrap();
        m.apply_config(store.get_module("desktop").unwrap())
            .unwrap();
        assert_eq!(*m.tidy_map.read(), None, "null=收回内置六类");
        assert_eq!(*p0.map_ref().read(), None, "同一 Arc：planner 同步看见");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-16）随批：死键守卫 desktop 段
    fn dead_config_keys_are_revived_or_removed() {
        // kvm/file-core 同形制：module.rs 是声明现场，读者必须在别处（注释行不算）
        let src_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut reader_lines: Vec<Vec<String>> = Vec::new();
        for entry in walkdir::WalkDir::new(&src_root).into_iter() {
            let entry = entry.expect("walkdir 不应失败");
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            if path.file_name() == Some(std::ffi::OsStr::new("module.rs")) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            reader_lines.push(
                text.lines()
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .map(|l| l.to_owned())
                    .collect(),
            );
        }
        let key_has_reader = |key: &str| {
            reader_lines
                .iter()
                .any(|lines| lines.iter().any(|l| l.contains(key)))
        };

        let schema = DesktopModule::new(&PathBuf::from(".")).config_schema();
        let props = schema["properties"].as_object().expect("schema 应为对象");
        let default_ser = serde_json::to_value(DesktopConfig::default()).unwrap();
        let default_obj = default_ser.as_object().expect("DesktopConfig 可序列化");

        let mut keys: Vec<&String> = props.keys().collect();
        keys.sort();
        let mut fields: Vec<&String> = default_obj.keys().collect();
        fields.sort();
        assert_eq!(keys, fields, "schema 键集必须恰等 DesktopConfig 字段集");
        assert_eq!(keys, vec!["tidy_map"], "launcher_hotkey_note 死键已除名");
        for key in &keys {
            assert!(
                key_has_reader(key),
                "死键：{key} 在 config_schema 声明却在 module.rs 之外零读者"
            );
            assert_eq!(
                props[*key].get("default").cloned(),
                default_obj.get(*key).cloned(),
                "schema default 与 Default 必须逐值同源: {key}"
            );
        }
    }

    /// 红线回归（09 §4.2 T-B1-6）：build() 整体替换条目，重放是内置动作的唯一保留路径
    #[test]
    fn launcher_reindex_preserves_registered_actions() {
        let dir = tmp_dir("reindex");
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
