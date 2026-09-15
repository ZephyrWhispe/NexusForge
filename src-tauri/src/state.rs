//! Tauri 集成层（docs/impl/01 S7）：宿主状态组装、事件转发、演示模块
//!
//! DemoModule 说明：M1 演示用最小模块，用于端到端验证
//! 注册 → init/start → 托盘聚合 → 状态查询 → panic 隔离 重启链路。
//! clipboard-core 注册后将移除。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use host_core::capability::{HotkeyProvider, TrayMenuItem, TrayProvider};
use host_core::config::ConfigStore;
use host_core::crash;
use host_core::error::ModuleError;
use host_core::events::EventBus;
use host_core::hotkey::HotkeyManager;
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};
use host_core::ports::Ports;
use host_core::registry::ModuleRegistry;
use serde::Serialize;

/// 命令行启动选项（docs/impl/01 S6.5）
pub struct StartupOptions {
    /// `--safe-mode`：只启动宿主，不 init/start 任何模块
    pub safe_mode: bool,
    /// `--restore-proxy`：紧急还原系统代理后退出（真实还原在阶段二 PR4 接入）
    pub restore_proxy: bool,
}

impl StartupOptions {
    pub fn from_env() -> Self {
        let args: Vec<String> = std::env::args().collect();
        Self {
            safe_mode: args.iter().any(|a| a == "--safe-mode"),
            restore_proxy: args.iter().any(|a| a == "--restore-proxy"),
        }
    }
}

/// 宿主状态（全 Arc 字段，Clone 为浅拷贝）
#[derive(Clone)]
pub struct HostState {
    pub bus: Arc<EventBus>,
    pub ports: Arc<Ports>,
    pub config: Arc<ConfigStore>,
    pub registry: Arc<ModuleRegistry>,
    pub hotkeys: Arc<HotkeyManager>,
    pub app_data_dir: PathBuf,
    pub safe_mode: bool,
}

impl HostState {
    /// 组装宿主核心（docs/impl/01 S1–S6 全部组件接线）。
    /// 顺序约束：崩溃钩子必须最先安装。
    pub fn init(
        app_data_dir: PathBuf,
        opts: &StartupOptions,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        crash::install_panic_hook(app_data_dir.clone());

        let bus = Arc::new(EventBus::new());
        let ports = Arc::new(Ports::new());
        let config = Arc::new(ConfigStore::new(app_data_dir.join("config"), bus.clone()));
        let _global = config.load()?;
        let registry = Arc::new(ModuleRegistry::new(bus.clone()));
        let hotkeys = Arc::new(HotkeyManager::new(ports.clone()));

        // ---- M1 演示模块（临时，C1 移除）----
        // 保留具体类型 Arc<DemoModule>，按需向上转型为不同 trait 对象注册
        let demo: Arc<DemoModule> = Arc::new(DemoModule::default());
        config.register_schema("demo", demo.config_schema());
        registry.register(demo.clone())?;
        registry.register_ability::<dyn TrayProvider>(demo.clone());
        registry.register_ability::<dyn HotkeyProvider>(demo.clone());

        Ok(Self {
            bus,
            ports,
            config,
            registry,
            hotkeys,
            app_data_dir,
            safe_mode: opts.safe_mode,
        })
    }

    /// 模块 init/start + 快捷键批量注册 + 托盘聚合（后台任务调用）
    pub async fn bootstrap_modules(&self) {
        if self.safe_mode {
            tracing::info!("safe-mode：跳过模块启动");
            return;
        }
        let ctx = Arc::new(ModuleContext {
            app_data_dir: self.app_data_dir.clone(),
            ports: self.ports.clone(),
        });
        for (id, r) in self.registry.init_all(ctx).await {
            if let Err(e) = r {
                tracing::error!(module = %id, error = %e, "模块 init 失败");
            }
        }
        for (id, r) in self.registry.start_all().await {
            if let Err(e) = r {
                tracing::error!(module = %id, error = %e, "模块 start 失败");
            }
        }
        // 模块全局快捷键批量注册（失败不阻断，UI 冲突面板可查）
        for provider in self.registry.abilities().get_all::<dyn HotkeyProvider>() {
            let info = provider.info();
            for binding in provider.global_hotkeys() {
                if let Err(e) = self.hotkeys.register(info.id, info.priority, binding.clone()) {
                    tracing::warn!(module = info.id, binding = %binding.id, error = %e, "快捷键注册失败");
                }
            }
        }
        // 托盘聚合（原生 tray-icon 接入在 C1 里程碑；此处验证数据链路）
        let sections = host_core::capability::aggregate_tray(self.registry.abilities());
        tracing::info!(sections = ?sections, "托盘菜单聚合完成");
    }
}

/// 把事件总线全部主题转发到前端窗口（事件名 `nf:event`）
pub fn forward_events(app: tauri::AppHandle, bus: Arc<EventBus>) {
    use tauri::Emitter;
    for (topic, _) in host_core::events::TOPIC_REGISTRY {
        let mut rx = match bus.subscribe(topic) {
            Ok(rx) => rx,
            Err(_) => continue,
        };
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if let Ok(v) = serde_json::to_value(&event) {
                            let _ = app.emit("nf:event", v);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// M1 演示模块（临时，C1 移除）
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct DemoModule {
    state: AtomicU8,
}

impl Module for DemoModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "demo",
            name: "演示模块",
            version: "0.1.0",
            icon: Some("sparkle"),
            priority: 200,
        }
    }
    fn init(&self, _ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
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
                "greeting": { "type": "string", "default": "你好，NexusForge" }
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

impl TrayProvider for DemoModule {
    fn tray_menu_items(&self) -> Vec<TrayMenuItem> {
        vec![TrayMenuItem {
            id: "demo.open".into(),
            label: "打开演示面板".into(),
            enabled: true,
        }]
    }
}

impl HotkeyProvider for DemoModule {
    fn global_hotkeys(&self) -> Vec<host_core::capability::HotkeyBinding> {
        vec![] // 演示模块不占用全局快捷键
    }
}

/// 模块状态 DTO（前端 IPC 返回）
#[derive(Serialize)]
pub struct ModuleStatusDto {
    pub id: String,
    pub name: String,
    pub version: String,
    pub priority: u8,
    pub state: ModuleState,
}
