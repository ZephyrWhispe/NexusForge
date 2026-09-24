//! TermModule 模块壳（docs/impl/06 T）：Module trait 实现。
//!
//! - init：注入 ConptyPort/DockerPipePort + EventBus；SSH known_hosts 打开
//! - stop：强制回收全部会话（docs/impl/06 风险标注：ConPTY 句柄泄漏最常见缺陷）
//! - 无全局快捷键 ability

use parking_lot::RwLock;
use std::path::PathBuf;

use std::sync::Arc;

use host_core::error::ModuleError;
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};
use host_core::ports::ConptyPort;

use crate::session::TermSessions;
use crate::ssh::SshService;

pub struct TermModule {
    state: ModuleStateCell,
    sessions: Arc<TermSessions>,
    ssh: RwLock<Option<Arc<SshService>>>,
    /// appData 根（known_hosts 路径）
    app_data_dir: PathBuf,
}

impl TermModule {
    pub fn new(app_data_dir: &std::path::Path) -> Self {
        Self {
            state: ModuleStateCell::new(),
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
        self.ssh.read().clone()
    }
}

impl Module for TermModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "term",
            name: "终端与运维",
            version: "0.1.0",
            icon: Some("term"),
            priority: priority_of("term"),
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let conpty = ctx
            .ports
            .get::<dyn ConptyPort>()
            .ok_or_else(|| ModuleError::Init("ConptyPort 未注册".into()))?;
        self.sessions.attach(conpty, ctx.event_bus.clone());

        // T-B7-1：信任面走两域共享单一事实源（appData 根下的 ssh/ 文件；
        // 首载触发两旧表合并）。坏文件 fail-closed——init 即 Err 点名路径，
        // 带病启动等于信任任意主机。
        let ssh =
            SshService::new(&self.app_data_dir).map_err(|e| ModuleError::Init(e.to_string()))?;
        *self.ssh.write() = Some(Arc::new(ssh));

        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        self.state.set(ModuleState::Running);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        // 强制回收全部会话（ConPTY 句柄防泄漏）
        self.sessions.kill_all();
        self.state.set(ModuleState::Stopped);
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
        self.state.get()
    }

    fn set_status(&self, state: ModuleState) {
        self.state.set(state);
    }
}
