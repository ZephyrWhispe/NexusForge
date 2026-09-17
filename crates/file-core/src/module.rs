//! FileModule 模块壳（docs/impl/05 F1–F7）：Module trait 实现。
//!
//! - init：打开 [`FileService`]（pending_ops 在 appData 下；2 worker）
//! - start：崩溃恢复扫描 pending_ops，自动断点续传（docs/impl/01 S6.5）
//! - stop：不再接收新任务；存量任务由 worker 完成或进程退出自然终止

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use host_core::error::ModuleError;
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};

use crate::service::FileService;

pub struct FileModule {
    service: RwLock<Option<Arc<FileService>>>,
    state: AtomicU8,
}

impl FileModule {
    pub fn new() -> Self {
        Self { service: RwLock::new(None), state: AtomicU8::new(0) }
    }

    /// IPC 层入口（全部命令经此取服务；未 init 返回 None）
    pub fn service(&self) -> Option<Arc<FileService>> {
        self.service.read().ok().and_then(|g| g.clone())
    }
}

impl Default for FileModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for FileModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "file",
            name: "文件与存储",
            version: "0.1.0",
            icon: Some("file"),
            priority: 25,
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let svc = FileService::open(&ctx.app_data_dir, ctx.event_bus.clone(), ctx.ports.clone())
            .map_err(|e| ModuleError::Storage(e.to_string()))?;
        *self.service.write().map_err(|_| ModuleError::Init("锁污染".into()))? =
            Some(Arc::new(svc));
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        // 崩溃恢复：pending_ops 断点续传（失败仅告警，不阻断启动）
        if let Some(svc) = self.service() {
            let resumed = svc.resume_pending();
            if resumed > 0 {
                tracing::info!(count = resumed, "文件操作崩溃恢复重入队");
            }
        }
        self.state.store(2, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        // worker 随服务句柄存活到进程退出；暂停全部活跃操作保证断点落盘
        if let Some(svc) = self.service() {
            for p in svc.ops_active() {
                if p.state == crate::ops::OpState::Running || p.state == crate::ops::OpState::Queued {
                    let _ = svc.op_pause(&p.op_id);
                }
            }
        }
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "default_conflict_policy": {
                    "type": "string", "title": "默认同名冲突策略",
                    "enum": ["ask", "skip", "overwrite", "rename"],
                    "default": "ask"
                },
                "delete_to_recycle": {
                    "type": "boolean", "title": "删除默认进回收站",
                    "description": "关闭为永久删除（docs/impl/05 F 风险项）",
                    "default": true
                }
            }
        })
    }

    fn status(&self) -> ModuleState {
        match self.state.load(Ordering::SeqCst) {
            0 => ModuleState::Uninitialized,
            1 => ModuleState::Stopped,
            _ => ModuleState::Running,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::events::EventBus;
    use host_core::ports::Ports;

    #[test]
    fn module_lifecycle_with_tmp_appdata() {
        let dir = std::env::temp_dir().join(format!("nf_file_mod_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let m = FileModule::new();
        assert_eq!(m.status(), ModuleState::Uninitialized);
        let ctx = Arc::new(ModuleContext {
            app_data_dir: dir.clone(),
            ports: Arc::new(Ports::new()),
            event_bus: Arc::new(EventBus::new()),
        });
        m.init(ctx).unwrap();
        assert_eq!(m.status(), ModuleState::Stopped);
        assert!(m.service().is_some());
        m.start().unwrap();
        assert_eq!(m.status(), ModuleState::Running);
        m.stop().unwrap();
        assert_eq!(m.status(), ModuleState::Stopped);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
