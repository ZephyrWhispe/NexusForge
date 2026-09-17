//! TermModule 模块壳（docs/impl/06 T）：Module trait 实现。
//!
//! - init：注入 ConptyPort/DockerPipePort + EventBus；SSH known_hosts 打开
//! - stop：强制回收全部会话（docs/impl/06 风险标注：ConPTY 句柄泄漏最常见缺陷）
//! - 无全局快捷键 ability

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use host_core::error::ModuleError;
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};
use host_core::ports::{ConptyPort, DockerPipePort};

use crate::session::TermSessions;
use crate::ssh::SshService;

pub struct TermModule {
    state: AtomicU8,
    sessions: Arc<TermSessions>,
    ssh: RwLock<Option<Arc<SshService>>>,
    /// appData 根（known_hosts 路径）
    app_data_dir: PathBuf,
}

impl TermModule {
    pub fn new(app_data_dir: &std::path::Path) -> Self {
        Self {
            state: AtomicU8::new(0),
            sessions: Arc::new(TermSessions::new()),
            ssh: RwLock::new(None),
            app_data_dir: app_data_dir.to_path_buf(),
        }
    }

    /// IPC 层入口
    pub fn sessions(&self) -> &Arc<TermSessions> {
        &self.sessions
    }

    pub fn ssh(&self) -> Option<Arc<SshService>> {
        self.ssh.read().ok().and_then(|g| g.clone())
    }
}

impl Module for TermModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "term",
            name: "终端与运维",
            version: "0.1.0",
            icon: Some("term"),
            priority: 14,
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let conpty = ctx
            .ports
            .get::<dyn ConptyPort>()
            .ok_or_else(|| ModuleError::Init("ConptyPort 未注册".into()))?;
        self.sessions.attach(conpty, ctx.event_bus.clone());

        let ssh = SshService::new(self.app_data_dir.join("term").join("known_hosts.json"))
            .map_err(|e| ModuleError::Init(e.to_string()))?;
        *self.ssh.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(Arc::new(ssh));

        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        self.state.store(2, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        // 强制回收全部会话（ConPTY 句柄防泄漏）
        self.sessions.kill_all();
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "default_shell_note": {
                    "type": "string", "title": "默认 shell",
                    "description": "本地会话默认 PowerShell（pwsh 7 优先）；可按会话指定完整命令行",
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
