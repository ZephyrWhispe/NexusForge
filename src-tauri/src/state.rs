//! Tauri 集成层（docs/impl/01 S7）：宿主状态组装、事件转发、模块引导

use std::path::PathBuf;
use std::sync::Arc;

use host_core::capability::HotkeyProvider;
use host_core::config::ConfigStore;
use host_core::crash;
use host_core::events::EventBus;
use host_core::hotkey::HotkeyManager;
use host_core::module::{Module, ModuleContext, ModuleState};
use host_core::ports::{CapturePort, ClipboardPort, CryptoPort, HotkeyWinPort, OcrPort, Ports};
use host_core::registry::ModuleRegistry;
use serde::Serialize;
use win_integration::capture::GdiCapture;
use win_integration::clipboard::WindowsClipboard;
use win_integration::dpapi::Dpapi;
use win_integration::hotkey::HotkeyWin;
use win_integration::ocr::WinOcr;

use clipboard_core::module::ClipboardModule;
use ocr_core::OcrModule;
use screenshot_core::ScreenshotModule;

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
    pub clipboard: Arc<ClipboardModule>,
    pub screenshot: Arc<ScreenshotModule>,
    pub ocr: Arc<OcrModule>,
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
        // 真实 Windows 能力注册（win-integration）
        ports.register::<dyn ClipboardPort>(Arc::new(WindowsClipboard::new()));
        ports.register::<dyn CryptoPort>(Arc::new(Dpapi));
        // 屏幕捕获：GDI BitBlt（docs/impl/03 P2，v1 主路径）
        ports.register::<dyn CapturePort>(Arc::new(GdiCapture::new()));
        // 系统 OCR：Windows.Media.Ocr（docs/impl/04 O2）
        ports.register::<dyn OcrPort>(Arc::new(WinOcr::new()));
        // 全局快捷键 OS 层：创建失败仅告警（应用内快捷键不受影响）
        match HotkeyWin::new() {
            Ok(hk) => {
                ports.register::<dyn HotkeyWinPort>(Arc::new(hk));
            }
            Err(e) => tracing::warn!(error = %e, "全局快捷键 OS 层初始化失败"),
        }

        let config = Arc::new(ConfigStore::new(app_data_dir.join("config"), bus.clone()));
        let _global = config.load()?;
        let registry = Arc::new(ModuleRegistry::new(bus.clone()));
        let hotkeys = Arc::new(HotkeyManager::new(ports.clone()));

        // ---- P0 功能模块 ----
        let clipboard = Arc::new(ClipboardModule::new());
        config.register_schema("clipboard", clipboard.config_schema());
        registry.register(clipboard.clone())?;

        let screenshot = Arc::new(ScreenshotModule::new());
        config.register_schema("screenshot", screenshot.config_schema());
        registry.register(screenshot.clone())?;

        let ocr = Arc::new(OcrModule::new());
        registry.register(ocr.clone())?;

        Ok(Self {
            bus,
            ports,
            config,
            registry,
            hotkeys,
            clipboard,
            screenshot,
            ocr,
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
            event_bus: self.bus.clone(),
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
        // 模块全局快捷键批量注册：binding × action 按 binding_id 配对（失败不阻断，UI 冲突面板可查）
        for provider in self.registry.abilities().get_all::<dyn HotkeyProvider>() {
            let info = provider.info();
            let bindings = provider.global_hotkeys();
            let actions: std::collections::HashMap<String, _> = provider
                .hotkey_actions()
                .into_iter()
                .map(|a| (a.binding_id, a.action))
                .collect();
            for binding in bindings {
                let Some(action) = actions.get(&binding.id).cloned() else {
                    tracing::warn!(module = info.id, binding = %binding.id, "快捷键缺少触发动作，跳过注册");
                    continue;
                };
                if let Err(e) =
                    self.hotkeys
                        .register(info.id, info.priority, binding.clone(), action)
                {
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
// 模块状态 DTO（前端 IPC 返回）
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ModuleStatusDto {
    pub id: String,
    pub name: String,
    pub version: String,
    pub priority: u8,
    pub state: ModuleState,
}
