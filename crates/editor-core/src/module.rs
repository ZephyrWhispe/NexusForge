//! EditorModule 模块壳（docs/impl/06 E）：Module trait 实现。
//!
//! 无 Windows 端口依赖（文本/PDF 全为纯文件操作）；会话保存在内存，
//! 自动保存草稿由 IPC 层防抖触发（E2 前端 3s 防抖）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use host_core::error::ModuleError;
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};

use crate::session::EditorSessions;

pub struct EditorModule {
    state: AtomicU8,
    sessions: Arc<EditorSessions>,
    /// 自动保存草稿根（{appData}/editor/autosave 日志目录，预留）
    work_dir: PathBuf,
}

impl EditorModule {
    pub fn new(app_data_dir: &std::path::Path) -> Self {
        Self {
            state: AtomicU8::new(0),
            sessions: Arc::new(EditorSessions::new()),
            work_dir: app_data_dir.join("editor"),
        }
    }

    /// IPC 层入口
    pub fn sessions(&self) -> &Arc<EditorSessions> {
        &self.sessions
    }

    pub fn work_dir(&self) -> &std::path::Path {
        &self.work_dir
    }
}

impl Module for EditorModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "editor",
            name: "文本与 PDF",
            version: "0.1.0",
            icon: Some("editor"),
            priority: 20,
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        std::fs::create_dir_all(&self.work_dir).map_err(|e| ModuleError::Storage(e.to_string()))?;
        let _ = ctx; // 无端口依赖
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
                "big_file_highlight_mb": {
                    "type": "integer", "title": "大文件阈值（MB，关闭语法高亮）",
                    "description": "超过该大小关闭语法高亮（E2）", "minimum": 1, "maximum": 100,
                    "default": 5
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
