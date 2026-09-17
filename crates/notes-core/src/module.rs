//! NotesModule 模块壳（docs/impl/06 N）：Module trait 实现。
//!
//! - init：打开 NoteLibrary（库根 {appData}/notes，docs/impl/06 N1）+ 首次全量索引
//! - 无 Windows 端口依赖；文件 CRUD 全走 file-core StorageDriver（N5 多存储后端抽象）
//! - 变更事件 notes.changed 由 IPC 层发布（含 path/action），UI 事件驱动刷新

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use host_core::error::ModuleError;
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};

use crate::library::NoteLibrary;

pub struct NotesModule {
    state: AtomicU8,
    library: RwLockOption,
    /// 库根 {appData}/notes
    root: PathBuf,
    /// 索引库 {appData}/db/notes.db
    db_path: PathBuf,
}

// RwLock<Option<Arc<NoteLibrary>>> 的轻量别名（避免逐处写泛型）
type RwLockOption = std::sync::RwLock<Option<Arc<NoteLibrary>>>;

impl NotesModule {
    pub fn new(app_data_dir: &std::path::Path) -> Self {
        Self {
            state: AtomicU8::new(0),
            library: std::sync::RwLock::new(None),
            root: app_data_dir.join("notes"),
            db_path: app_data_dir.join("db").join("notes.db"),
        }
    }

    /// IPC 层入口（init 后可用）
    pub fn library(&self) -> Option<Arc<NoteLibrary>> {
        self.library.read().ok().and_then(|g| g.clone())
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
            priority: 15,
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let _ = ctx; // 无端口依赖
        let reg = file_core::driver::DriverRegistry::new();
        let lib = NoteLibrary::open(
            self.root.clone(),
            &self.db_path,
            reg.get("local").ok_or_else(|| ModuleError::Init("本地驱动缺失".into()))?,
        )
        .map_err(|e| ModuleError::Storage(e.to_string()))?;
        // 首次增量索引（外部编辑器改动在此收敛；失败不阻塞模块启动）
        match lib.sync() {
            Ok(r) => tracing::info!(added = r.added, updated = r.updated, removed = r.removed, total = r.total, "笔记索引同步完成"),
            Err(e) => tracing::warn!(error = %e, "笔记索引同步失败（UI 可手动 reindex）"),
        }
        *self.library.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(Arc::new(lib));
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        self.state.store(2, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        self.state.store(1, Ordering::SeqCst);
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
        match self.state.load(Ordering::SeqCst) {
            0 => ModuleState::Uninitialized,
            1 => ModuleState::Stopped,
            _ => ModuleState::Running,
        }
    }
}
