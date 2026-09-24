//! EditorModule 模块壳（docs/impl/06 E）：Module trait 实现。
//!
//! 无 Windows 端口依赖（文本/PDF 全为纯文件操作）；会话保存在内存，
//! 自动保存草稿由 IPC 层防抖触发（E2 前端 3s 防抖）。

use std::path::PathBuf;

use std::sync::Arc;

use host_core::error::ModuleError;
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};

use crate::session::EditorSessions;

pub struct EditorModule {
    state: ModuleStateCell,
    sessions: Arc<EditorSessions>,
    /// 自动保存草稿根（{appData}/editor/autosave 日志目录，预留）
    work_dir: PathBuf,
}

impl EditorModule {
    pub fn new(app_data_dir: &std::path::Path) -> Self {
        let work_dir = app_data_dir.join("editor");
        Self {
            state: ModuleStateCell::new(),
            // T-B7-20：清单目录=work_dir（session_list.json 与 autosave 日志同根）
            sessions: Arc::new(EditorSessions::with_store(work_dir.clone())),
            work_dir,
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
            priority: priority_of("editor"),
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        std::fs::create_dir_all(&self.work_dir).map_err(|e| ModuleError::Storage(e.to_string()))?;
        let _ = ctx; // 无端口依赖
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        // T-B7-20：恢复标签行（只 stat 不读内容，懒经 content 首拉）；
        // 清单=可观测数据谱，坏档弃行均不阻塞启动（warn 在 load_manifest 内）
        let report = self.sessions.load_manifest();
        tracing::info!(
            restored = report.restored,
            dropped = report.dropped.len(),
            corrupt = report.corrupt,
            "编辑器会话清单恢复"
        );
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
        self.state.get()
    }

    fn set_status(&self, state: ModuleState) {
        self.state.set(state);
    }
}
