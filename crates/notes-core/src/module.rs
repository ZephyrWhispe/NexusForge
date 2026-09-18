//! NotesModule 模块壳（docs/impl/06 N）：Module trait 实现。
//!
//! - init：装载 StoragePort（D-02：宿主注册，模块间零横向依赖）+ 打开 NoteLibrary
//!   （库根 {appData}/notes，docs/impl/06 N1）+ 首次全量索引
//! - 文件 CRUD 全走 host-core::storage::StorageDriver（N5 多存储后端抽象）
//! - 变更事件 notes.changed 由 IPC 层发布（含 path/action），UI 事件驱动刷新

use std::path::PathBuf;

use std::sync::Arc;

use host_core::error::ModuleError;
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};

use crate::library::NoteLibrary;

pub struct NotesModule {
    state: ModuleStateCell,
    library: RwLockOption,
    /// 库根 {appData}/notes
    root: PathBuf,
    /// 索引库 {appData}/db/notes.db
    db_path: PathBuf,
}

// RwLock<Option<Arc<NoteLibrary>>> 的轻量别名（避免逐处写泛型）
type RwLockOption = parking_lot::RwLock<Option<Arc<NoteLibrary>>>;

impl NotesModule {
    pub fn new(app_data_dir: &std::path::Path) -> Self {
        Self {
            state: ModuleStateCell::new(),
            library: parking_lot::RwLock::new(None),
            root: app_data_dir.join("notes"),
            db_path: app_data_dir.join("db").join("notes.db"),
        }
    }

    /// IPC 层入口（init 后可用）
    pub fn library(&self) -> Option<Arc<NoteLibrary>> {
        self.library.read().clone()
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }
}

impl Module for NotesModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "notes",
            name: "笔记与知识",
            version: "0.1.0",
            icon: Some("notes"),
            priority: priority_of("notes"),
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        // D-02：存储驱动经 StoragePort 注入（file-core 注册实现），不直依 file-core
        let storage = ctx
            .ports
            .get::<dyn host_core::storage::StoragePort>()
            .ok_or_else(|| {
                ModuleError::Init("StoragePort 未注册（宿主需在模块 init 前登记）".into())
            })?;
        let driver = storage
            .driver("local")
            .ok_or_else(|| ModuleError::Init("本地存储驱动缺失".into()))?;
        let lib = NoteLibrary::open(self.root.clone(), &self.db_path, driver)
            .map_err(|e| ModuleError::Storage(e.to_string()))?;
        // 首次增量索引（外部编辑器改动在此收敛；失败不阻塞模块启动）
        match lib.sync() {
            Ok(r) => tracing::info!(
                added = r.added,
                updated = r.updated,
                removed = r.removed,
                total = r.total,
                "笔记索引同步完成"
            ),
            Err(e) => tracing::warn!(error = %e, "笔记索引同步失败（UI 可手动 reindex）"),
        }
        *self.library.write() = Some(Arc::new(lib));
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        self.state.set(ModuleState::Running);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "vault_root_note": {
                    "type": "string", "title": "库根说明",
                    "description": "v1 库根固定为 {appData}/notes；用户自定义目录在后续里程碑开放",
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
