//! V7 模块壳（docs/impl/05 V7）：Module trait 实现。
//!
//! - init：打开 [`VaultService`]（meta/db 在 appData 下；库未创建 = Uninitialized）
//! - stop：立即锁定（DEK wipe），安全语义优先
//! - V4 Windows Hello / V5 自动锁后续轮次接入（start 时挂计时器）

use parking_lot::RwLock;

use std::sync::Arc;

use host_core::error::ModuleError;
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};

use crate::vault::VaultService;

pub struct VaultModule {
    service: RwLock<Option<Arc<VaultService>>>,
    state: ModuleStateCell,
}

impl VaultModule {
    pub fn new() -> Self {
        Self {
            service: RwLock::new(None),
            state: ModuleStateCell::new(),
        }
    }

    /// IPC 层入口（全部命令经此取服务；未 init 返回 None）
    pub fn service(&self) -> Option<Arc<VaultService>> {
        self.service.read().clone()
    }
}

impl Default for VaultModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for VaultModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "vault",
            name: "安全与凭据",
            version: "0.1.0",
            icon: Some("vault"),
            priority: priority_of("vault"),
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let svc = VaultService::open(&ctx.app_data_dir)
            .map_err(|e| ModuleError::Storage(e.to_string()))?;
        *self.service.write() = Some(Arc::new(svc));
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        // V5 自动锁定计时器在后续轮次接入；当前仅标记运行态
        self.state.set(ModuleState::Running);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        // 停用即锁定：内存 DEK 立即 wipe（SecretKey Drop 兜底）
        if let Some(svc) = self.service() {
            let _ = svc.lock();
        }
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "clear_clipboard_secs": {
                    "type": "integer", "title": "复制密码后自动清除剪贴板",
                    "description": "0 = 不清除（docs/impl/05 V 风险项：默认 90s）",
                    "minimum": 0, "maximum": 600, "default": 90
                },
                "auto_lock_idle_mins": {
                    "type": "integer", "title": "空闲自动锁定（分钟）",
                    "description": "0 = 禁用；V5 交付后生效", "minimum": 0, "maximum": 120, "default": 15
                }
            }
        })
    }

    fn status(&self) -> ModuleState {
        self.state.get()
    }

    fn set_status(&self, state: ModuleState) {
        self.state.set(state);
    }
}
