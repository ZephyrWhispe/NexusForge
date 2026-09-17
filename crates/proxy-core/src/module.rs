//! ProxyModule 模块壳（docs/impl/05 PR）：Module trait 实现。
//!
//! - init：打开 [`ProxyService`]（内含启动扫描：kill -9 残留系统代理自动还原）
//! - stop：**停内核 + 还原系统代理**（安全语义优先，进程退出的兜底还原点之一）
//! - panic hook 还原由 src-tauri state.rs 经 host-core crash::add_recovery_hook 注册

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use host_core::error::ModuleError;
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};
use host_core::ports::SysProxyPort;

use crate::service::ProxyService;

pub struct ProxyModule {
    service: RwLock<Option<Arc<ProxyService>>>,
    state: AtomicU8,
}

impl ProxyModule {
    pub fn new() -> Self {
        Self { service: RwLock::new(None), state: AtomicU8::new(0) }
    }

    /// IPC 层入口（全部命令经此取服务；未 init 返回 None）
    pub fn service(&self) -> Option<Arc<ProxyService>> {
        self.service.read().ok().and_then(|g| g.clone())
    }
}

impl Default for ProxyModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ProxyModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "proxy",
            name: "网络代理",
            version: "0.1.0",
            icon: Some("proxy"),
            priority: 15,
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let sp = ctx
            .ports
            .get::<dyn SysProxyPort>()
            .ok_or_else(|| ModuleError::Init("SysProxyPort 未注册".into()))?;
        let svc = ProxyService::open(&ctx.app_data_dir, ctx.event_bus.clone(), sp)
            .map_err(|e| ModuleError::Storage(e.to_string()))?;
        *self.service.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(svc);
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        self.state.store(2, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        // 停止即还原：停内核 + 恢复用户原系统代理（高危安全语义，docs/impl/05 PR 风险标注）
        if let Some(svc) = self.service() {
            svc.shutdown();
        }
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "mixed_port": {
                    "type": "integer", "title": "本地混合入站端口",
                    "description": "系统代理指向 127.0.0.1:此端口；修改后需重新切换模式生效",
                    "minimum": 1024, "maximum": 65535, "default": 7890
                }
            }
        })
    }

    fn apply_config(&self, values: serde_json::Value) -> Result<(), ModuleError> {
        if let Some(port) = values.get("mixed_port").and_then(|v| v.as_u64()) {
            if let Some(svc) = self.service() {
                svc.set_mixed_port(port as u16)
                    .map_err(|e| ModuleError::Init(e.to_string()))?;
            }
        }
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
