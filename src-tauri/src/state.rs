//! Tauri 集成层（docs/impl/01 S7）：宿主状态组装、事件转发、模块引导

use std::path::PathBuf;
use std::sync::Arc;

use host_core::capability::HotkeyProvider;
use host_core::config::ConfigStore;
use host_core::crash;
use host_core::events::EventBus;
use host_core::hotkey::HotkeyManager;
use host_core::module::{Module, ModuleContext, ModuleState};
use host_core::ports::{
    CapturePort, ClipboardPort, CryptoPort, HotkeyWinPort, InputHookPort, InputInjectPort, OcrPort,
    Ports, RecycleBinPort, ScreenInfoPort, SysProxyPort, SysProxyState, ThumbPort, UsnIndexPort,
};
use host_core::registry::ModuleRegistry;
use serde::Serialize;
use win_integration::capture::GdiCapture;
use win_integration::clipboard::WindowsClipboard;
use win_integration::dpapi::Dpapi;
use win_integration::hotkey::HotkeyWin;
use win_integration::input::{InputHookWin, InputInjectWin, ScreenInfoWin};
use win_integration::ocr::WinOcr;
use win_integration::sysproxy::WindowsSysProxy;

use clipboard_core::module::ClipboardModule;
use file_core::FileModule;
use kvm_core::KvmModule;
use ocr_core::OcrModule;
use proxy_core::ProxyModule;
use screenshot_core::ScreenshotModule;
use vault_core::VaultModule;

/// 命令行启动选项（docs/impl/01 S6.5）
pub struct StartupOptions {
    /// `--safe-mode`：只启动宿主，不 init/start 任何模块
    pub safe_mode: bool,
    /// `--restore-proxy`：紧急还原系统代理后退出（真实实现在 lib.rs，PR4 接入）
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
    pub kvm: Arc<KvmModule>,
    pub vault: Arc<VaultModule>,
    pub file: Arc<FileModule>,
    pub proxy: Arc<ProxyModule>,
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
        // K4/K5/K7 输入捕获与注入 + 虚拟桌面信息（kvm-core 键鼠共享）
        ports.register::<dyn InputHookPort>(Arc::new(InputHookWin::new()?));
        ports.register::<dyn InputInjectPort>(Arc::new(InputInjectWin::new()));
        ports.register::<dyn ScreenInfoPort>(Arc::new(ScreenInfoWin::new()));
        // F4 Shell 缩略图 / F2 回收站 / F5 USN 索引（file-core，docs/impl/05 F）
        ports.register::<dyn ThumbPort>(Arc::new(win_integration::shell::ShellThumb));
        ports.register::<dyn RecycleBinPort>(Arc::new(win_integration::shell::RecycleBin));
        ports.register::<dyn UsnIndexPort>(Arc::new(win_integration::usn::UsnIndex::new()));
        // PR4 系统代理（proxy-core，docs/impl/05 PR）：注册表 + WinINET 广播
        let sys_proxy: Arc<dyn SysProxyPort> = Arc::new(WindowsSysProxy);
        ports.register::<dyn SysProxyPort>(sys_proxy.clone());

        // 崩溃恢复钩子：panic 时还原系统代理（断网最高危场景兜底，docs/impl/05 PR 风险标注）
        // 另两处还原：ProxyModule::stop（正常退出）+ lib.rs `--restore-proxy`（紧急抢救）
        {
            let hook_dir = app_data_dir.join("proxy");
            let hook_sp = sys_proxy;
            crash::add_recovery_hook(Arc::new(move || {
                proxy_core::sysproxy::restore_quiet(&hook_dir, hook_sp.as_ref());
            }));
        }

        let config = Arc::new(ConfigStore::new(app_data_dir.join("config"), bus.clone()));
        let _global = config.load()?;
        let registry = Arc::new(ModuleRegistry::new(bus.clone()));
        let hotkeys = Arc::new(HotkeyManager::new(ports.clone()));

        // ---- P0 功能模块 ----
        // register_ability：把模块的 HotkeyProvider/TrayProvider 能力登记进注册表
        // （修复：此前从未调用，abilities() 恒为空，全部全局快捷键静默未注册）
        let clipboard = Arc::new(ClipboardModule::new());
        config.register_schema("clipboard", clipboard.config_schema());
        registry.register(clipboard.clone())?;
        registry.register_ability::<dyn HotkeyProvider>(clipboard.clone());

        let screenshot = Arc::new(ScreenshotModule::new());
        config.register_schema("screenshot", screenshot.config_schema());
        registry.register(screenshot.clone())?;
        registry.register_ability::<dyn HotkeyProvider>(screenshot.clone());

        let ocr = Arc::new(OcrModule::new());
        registry.register(ocr.clone())?;
        registry.register_ability::<dyn HotkeyProvider>(ocr.clone());

        // ---- P1 键鼠共享（M4，docs/impl/05 K1–K7）----
        let kvm = Arc::new(KvmModule::new());
        config.register_schema("kvm", kvm.config_schema());
        registry.register(kvm.clone())?;

        // ---- P1 安全与凭据（M5，docs/impl/05 V1–V7）----
        let vault = Arc::new(VaultModule::new());
        config.register_schema("vault", vault.config_schema());
        registry.register(vault.clone())?;

        // ---- P1 文件与存储（M6，docs/impl/05 F1–F7）----
        let file = Arc::new(FileModule::new());
        config.register_schema("file", file.config_schema());
        registry.register(file.clone())?;

        // ---- P1 网络代理（M7，docs/impl/05 PR1–PR6；合规：不内置节点/订阅）----
        let proxy = Arc::new(ProxyModule::new());
        config.register_schema("proxy", proxy.config_schema());
        registry.register(proxy.clone())?;

        Ok(Self {
            bus,
            ports,
            config,
            registry,
            hotkeys,
            clipboard,
            screenshot,
            ocr,
            kvm,
            vault,
            file,
            proxy,
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
        let mut registered = 0usize;
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
                } else {
                    registered += 1;
                }
            }
        }
        tracing::info!(count = registered, "全局快捷键注册完成");
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
            Err(e) => {
                tracing::warn!(topic, error = %e, "事件主题订阅失败，前端将收不到该主题");
                continue;
            }
        };
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        tracing::info!(topic = event.topic, "事件转发到前端");
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
