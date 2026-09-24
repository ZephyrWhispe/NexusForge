//! Tauri 集成层（docs/impl/01 S7）：宿主状态组装、事件转发、模块引导

use std::path::PathBuf;
use std::sync::Arc;

use host_core::capability::{HotkeyProvider, TrayProvider};
use host_core::config::ConfigStore;
use host_core::crash;
use host_core::events::EventBus;
use host_core::hotkey::HotkeyManager;
use host_core::module::{Module, ModuleContext, ModuleState};
use host_core::ports::{
    CapturePort, ClipboardPort, ConptyPort, CryptoPort, DockerPipePort, HelloPort, HelperSpawnPort,
    HotkeyWinPort, InputHookPort, InputInjectPort, KeyboardLedPort, MemLockPort, OcrPort, PerfPort,
    Ports, ProcPort, RecycleBinPort, RegistryOps, ScreenInfoPort, ServiceCtlPort, ShellPort,
    SysProxyPort, TaskSchdPort, TaskTogglePort, ThumbPort, UsnIndexPort,
};
use host_core::registry::ModuleRegistry;
use serde::Serialize;
use win_integration::capture::GdiCapture;
use win_integration::clipboard::WindowsClipboard;
use win_integration::envelope::EnvelopeCrypto;
use win_integration::hotkey::HotkeyWin;
use win_integration::input::{InputHookWin, InputInjectWin, ScreenInfoWin};
use win_integration::ocr::WinOcr;
use win_integration::shell::ShellOps;
use win_integration::sysproxy::WindowsSysProxy;

use automation_core::module::AutomationModule;
use clipboard_core::module::ClipboardModule;
use desktop_core::DesktopModule;
use editor_core::EditorModule;
use file_core::FileModule;
use kvm_core::KvmModule;
use notes_core::NotesModule;
use ocr_core::OcrModule;
use proxy_core::ProxyModule;
use screenshot_core::ScreenshotModule;
use sync_core::SyncModule;
use sys_core::SysModule;
use term_core::TermModule;
use vault_core::VaultModule;

/// 命令行启动选项（docs/impl/01 S6.5）
pub struct StartupOptions {
    /// `--safe-mode`：只启动宿主，不 init/start 任何模块
    pub safe_mode: bool,
    /// `--restore-proxy`：紧急还原系统代理后退出（真实实现在 lib.rs，PR4 接入）
    pub restore_proxy: bool,
    /// `--run-rule {id}`：Task Scheduler 触发的独立规则执行（A4，docs/impl/07）
    pub run_rule: Option<String>,
}

impl StartupOptions {
    pub fn from_env() -> Self {
        let args: Vec<String> = std::env::args().collect();
        // --run-rule {id}：取下一个参数
        let run_rule = args
            .iter()
            .position(|a| a == "--run-rule")
            .and_then(|i| args.get(i + 1))
            .cloned();
        Self {
            safe_mode: args.iter().any(|a| a == "--safe-mode"),
            restore_proxy: args.iter().any(|a| a == "--restore-proxy"),
            run_rule,
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
    pub desktop: Arc<DesktopModule>,
    pub editor: Arc<EditorModule>,
    pub notes: Arc<NotesModule>,
    pub term: Arc<TermModule>,
    pub sys: Arc<SysModule>,
    pub automation: Arc<AutomationModule>,
    pub sync: Arc<SyncModule>,
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
        ports.register::<dyn CryptoPort>(Arc::new(EnvelopeCrypto::with_dpapi()));
        // V4 Hello 校验门（docs/impl/05 V4 / D-24）：UserConsentVerifier；
        // 机器无 Hello 时 availability 查询在 verify 内报错，不影响密码路径
        ports.register::<dyn HelloPort>(Arc::new(win_integration::hello::WindowsHello::new()));
        // D-24：VirtualLock 锁页端口注册 + 注入 vault 全局槽（未注入时锁页静默跳过）
        let mem_lock: Arc<dyn MemLockPort> = Arc::new(win_integration::memlock::WinMemLock);
        ports.register::<dyn MemLockPort>(mem_lock.clone());
        vault_core::crypto::set_mem_lock(mem_lock);
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
        // T-B7-9 锁键灯态（kvm-core 修饰键同步的"做"侧）
        ports.register::<dyn KeyboardLedPort>(Arc::new(win_integration::led::KeyboardLedWin));
        // F4 Shell 缩略图 / F2 回收站 / F5 USN 索引（file-core，docs/impl/05 F）
        ports.register::<dyn ThumbPort>(Arc::new(win_integration::shell::ShellThumb));
        ports.register::<dyn RecycleBinPort>(Arc::new(win_integration::shell::RecycleBin));
        ports.register::<dyn UsnIndexPort>(Arc::new(win_integration::usn::UsnIndex::new()));
        // D1 Shell 启动（desktop-core 启动器，docs/impl/05 D）
        ports.register::<dyn ShellPort>(Arc::new(ShellOps));
        // T1 ConPTY（term-core，docs/impl/06 T）
        ports.register::<dyn ConptyPort>(Arc::new(win_integration::conpty::ConptyWin::new()));
        // SY4 性能采样（sys-core，docs/impl/06 SY）
        ports.register::<dyn PerfPort>(Arc::new(win_integration::perf::PdhWin::new()?));
        // T-B7-10 进程枚举/终结（sys-core 进程页红线；OS 面在 win-integration）
        ports.register::<dyn ProcPort>(Arc::new(win_integration::process::WinProc));
        // T6 Docker Engine named pipe（docs/impl/06 T6）
        ports.register::<dyn DockerPipePort>(Arc::new(
            win_integration::docker::DockerPipeWin::new(),
        ));
        // A4 Task Scheduler（automation-core，docs/impl/07）：schtasks 封装
        ports.register::<dyn TaskSchdPort>(Arc::new(win_integration::taskschd::TaskSchdOps::new()));
        // WinOps W0 注册表数据面（docs/impl/08）：HKCU 进程内直写
        ports.register::<dyn RegistryOps>(Arc::new(
            win_integration::registry::RegistryOpsWin::new(),
        ));
        // WinOps W2 扩展数据面：计划任务启停 + 服务控制（同一个 TaskSchdOps 实现两 trait）
        ports.register::<dyn TaskTogglePort>(Arc::new(
            win_integration::taskschd::TaskSchdOps::new(),
        ));
        ports.register::<dyn ServiceCtlPort>(Arc::new(win_integration::service::ServiceOps::new()));
        // WinOps W3 提权 Helper 拉起（docs/impl/08 §4）：ShellExecuteExW runas → UAC
        ports.register::<dyn HelperSpawnPort>(Arc::new(
            win_integration::helper::HelperSpawnWin::new(),
        ));
        // WinOps W4 系统维护（docs/impl/08）：clean_dir 本地实现（提权场景走 HelperMaintenance）
        ports.register::<dyn host_core::ports::MaintenancePort>(Arc::new(
            win_integration::maintenance::MaintenanceWin::new(),
        ));
        // WinOps W5 Appx 包管理（docs/impl/08）：WinRT 当前用户 + PS provisioned（提权走 HelperAppx）
        ports.register::<dyn host_core::ports::AppxPort>(Arc::new(
            win_integration::appx::AppxOps::new(),
        ));
        // PR4 系统代理（proxy-core，docs/impl/05 PR）：注册表 + WinINET 广播
        let sys_proxy: Arc<dyn SysProxyPort> = Arc::new(WindowsSysProxy);
        ports.register::<dyn SysProxyPort>(sys_proxy.clone());
        // D-02 存储驱动端口：notes-core 等消费方经 ctx.ports 取驱动（实现在 file-core）
        ports.register::<dyn host_core::storage::StoragePort>(Arc::new(
            file_core::FileStoragePort::new(Arc::new(file_core::DriverRegistry::new())),
        ));

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
        let clipboard = Arc::new(ClipboardModule::new_with_config(config.clone()));
        config.register_schema("clipboard", clipboard.config_schema());
        registry.register(clipboard.clone())?;
        registry.register_ability::<dyn HotkeyProvider>(clipboard.clone());
        registry.register_ability::<dyn TrayProvider>(clipboard.clone());

        let screenshot = Arc::new(ScreenshotModule::new());
        // T-B6-12 截图 WebDAV 上传宿主桥（09 §6.0 方向裁定 (b)：跨模块只经
        // src-tauri）——screenshot-core 交出纯数据请求，装配与提交走 file-core
        // 唯一腿；两 crate 互不依赖的边长在这里。缺这段装配 webdav 档不注册。
        {
            let send: screenshot_core::upload::WebDavSend =
                Arc::new(|req: screenshot_core::upload::WebDavPutRequest| {
                    Box::pin(async move {
                        let asm = file_core::remote::webdav::assemble_put(
                            &req.endpoint_base,
                            &req.filename,
                            req.header_value.as_deref(),
                        )?;
                        file_core::remote::webdav::send_put(&asm, req.bytes).await?;
                        Ok(asm.url)
                    })
                });
            screenshot.set_upload_sender(send);
        }
        config.register_schema("screenshot", screenshot.config_schema());
        registry.register(screenshot.clone())?;
        registry.register_ability::<dyn HotkeyProvider>(screenshot.clone());
        registry.register_ability::<dyn TrayProvider>(screenshot.clone());

        let ocr = Arc::new(OcrModule::new());
        config.register_schema("ocr", ocr.config_schema());
        registry.register(ocr.clone())?;
        registry.register_ability::<dyn HotkeyProvider>(ocr.clone());

        // ---- P1 键鼠共享（M4，docs/impl/05 K1–K7）----
        // T-B7-8：带 ConfigStore 句柄构造——边缘映射 config set 即落 kvm 段
        let kvm = Arc::new(KvmModule::new_with_config(config.clone()));
        config.register_schema("kvm", kvm.config_schema());
        registry.register(kvm.clone())?;

        // ---- P1 安全与凭据（M5，docs/impl/05 V1–V7）----
        // D-24 V5：看门狗需要配置句柄（ModuleContext 无 config，经构造函数注入）
        let vault = Arc::new(VaultModule::new_with_config(config.clone()));
        config.register_schema("vault", vault.config_schema());
        registry.register(vault.clone())?;
        registry.register_ability::<dyn TrayProvider>(vault.clone());

        // ---- P1 文件与存储（M6，docs/impl/05 F1–F7）----
        let file = Arc::new(FileModule::new());
        config.register_schema("file", file.config_schema());
        registry.register(file.clone())?;

        // ---- P1 网络代理（M7，docs/impl/05 PR1–PR6；合规：不内置节点/订阅）----
        let proxy = Arc::new(ProxyModule::new());
        config.register_schema("proxy", proxy.config_schema());
        registry.register(proxy.clone())?;

        // ---- P1 桌面效率（M8，docs/impl/05 D1–D4）----
        let desktop = Arc::new(DesktopModule::new(&app_data_dir));
        config.register_schema("desktop", desktop.config_schema());
        registry.register(desktop.clone())?;
        registry.register_ability::<dyn HotkeyProvider>(desktop.clone());

        // ---- P2 文本与 PDF（M9，docs/impl/06 E1–E4）----
        let editor = Arc::new(EditorModule::new(&app_data_dir));
        config.register_schema("editor", editor.config_schema());
        registry.register(editor.clone())?;

        // ---- P2 笔记与知识（M10，docs/impl/06 N1–N5）----
        let notes = Arc::new(NotesModule::new(&app_data_dir));
        config.register_schema("notes", notes.config_schema());
        registry.register(notes.clone())?;

        // ---- P2 终端与运维（M11，docs/impl/06 T1–T6）----
        let term = Arc::new(TermModule::new(&app_data_dir));
        config.register_schema("term", term.config_schema());
        registry.register(term.clone())?;

        // ---- P2 系统管理（M12，docs/impl/06 SY1–SY4）----
        let sys = Arc::new(SysModule::new(&app_data_dir));
        config.register_schema("sys", sys.config_schema());
        registry.register(sys.clone())?;

        // ---- 阶段四自动化（M14，docs/impl/07 A1–A3）----
        let automation = Arc::new(AutomationModule::new(&app_data_dir));
        config.register_schema("automation", automation.config_schema());
        registry.register(automation.clone())?;

        // ---- 阶段四同步（M15，docs/impl/07 SYNC1–SYNC4）：信任根复用 KVM 配对 ----
        let sync = Arc::new(SyncModule::new(&app_data_dir));
        // 装配口点名数据集（T-B5-5 双参）：白名单外的 entity 在此 Err ⇒ 启动失败，
        // 而不是"接上了但没人用"——密码库根本没有这条通路。
        sync.attach_applier(
            sync_core::ENTITY_NOTE,
            Arc::new(NotesApplier {
                notes: notes.clone(),
            }),
        )
        .map_err(|e| format!("SYNC 数据集装配失败：{e}"))?;
        config.register_schema("sync", sync.config_schema());
        registry.register(sync.clone())?;

        // ---- T-B5-7 免手输地址：两颗方向相反的螺丝（都必须在模块 init 之前）----
        // ① 宣告：sync 端口的事实源在 sync-core，kvm 只是搬运工。读 `port()` 现值而不是
        //    用常量——常量会过期，现值不会；宣告一个本机没在听的口，等于让邻居白拨。
        kvm.set_sync_port(sync.port());
        // ② 解析：sync 需要地址时问发现层。sync-core 不 import kvm-core，桥就搭在这层。
        //    没搭桥的后果不是崩溃而是退化（addr_source=false ⇒ 面板整列不显在线/离线），
        //    所以这里不需要"装配失败即启动失败"的粗门。
        sync.set_addr_resolver(Arc::new(KvmPeerAddrResolver { kvm: kvm.clone() }));

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
            desktop,
            editor,
            notes,
            term,
            sys,
            automation,
            sync,
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
        // 动态 schema 重刷：register_schema 发生在构造期（init 之前），而 ocr 的引擎词表
        // enum 只有注册表建好才是真值——写侧校验与设置中心必须读同一份词表，否则
        // "配置里可选的引擎"与"运行期真实存在的引擎"会各说一套。
        for info in self.registry.infos() {
            if let Some(m) = self.registry.get(info.id) {
                self.config.register_schema(info.id, m.config_schema());
            }
        }
        for (id, r) in self.registry.start_all().await {
            if let Err(e) = r {
                tracing::error!(module = %id, error = %e, "模块 start 失败");
            }
        }
        // 缺陷① 启动半边：start_all 之后把盘上持久值派发给运行期模块。
        // 失败仅记日志不阻断——单个模块配置坏掉不得带走其余模块（S5 启动不互相阻断）
        for (id, r) in self.registry.apply_configs(&self.config).await {
            if let Err(e) = r {
                tracing::error!(module = %id, error = %e, "启动配置派发失败");
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
        // D-26：托盘菜单聚合的原生消费端在 tray.rs（host.module_state 事件驱动重建）；
        // 此处日志保留作引导期观测点
        let sections = host_core::capability::aggregate_tray(self.registry.abilities());
        tracing::info!(sections = ?sections, "托盘菜单聚合完成");
    }
}

/// 缺陷① 运行期半边：订阅 `host.config_changed` 并把新值派发给对应模块。
/// 与 [`forward_events`] 同段 setup 调用；派发循环在 host-core（可在无 Tauri
/// 上下文下直测），此处只负责通道与 spawn。
pub fn spawn_config_feed(
    registry: Arc<ModuleRegistry>,
    bus: Arc<EventBus>,
    config: Arc<ConfigStore>,
) {
    let Ok(rx) = bus.subscribe("host.config_changed") else {
        tracing::warn!("host.config_changed 订阅失败，运行期配置变更不会送达模块");
        return;
    };
    tauri::async_runtime::spawn(host_core::registry::run_config_feed(rx, registry, config));
}

/// 把事件总线全部主题转发到前端窗口（事件名 `nf:event`）
pub fn forward_events(app: tauri::AppHandle, bus: Arc<EventBus>) {
    use tauri::Emitter;
    for (topic, ..) in host_core::events::TOPIC_REGISTRY {
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

/// SYNC 变更应用器（docs/impl/07 SYNC2）：notes-core NoteLibrary 投影。
/// 动态取 library（NotesModule::init 后可用）；apply 走直调不发 notes.changed（防同步自环）。
struct NotesApplier {
    notes: Arc<NotesModule>,
}

impl NotesApplier {
    /// 本应用器只服务笔记库：注册表查表守卫（T-B5-5 之前这里是三处与裸字符串
    /// `"note"` 的相等比较——泛化后它既不是单一真源、也不会随白名单演进）。
    ///
    /// 两个条件都要：`ENTITY_NOTE` 说"我是笔记应用器"，`is_sync_entity` 说"笔记仍在
    /// 可同步名单里"——将来从白名单摘掉某数据集时，这里跟着一起关门，
    /// 而不是留下一个还能被会话调到的活应用器。
    fn serve(entity: &str) -> sync_core::Result<()> {
        if entity == sync_core::ENTITY_NOTE && sync_core::is_sync_entity(entity) {
            return Ok(());
        }
        Err(sync_core::SyncError::Entity(format!(
            "笔记应用器仅服务 {:?}，拒收数据集 {entity}",
            sync_core::ENTITY_NOTE,
        )))
    }
}

impl sync_core::ChangeApplier for NotesApplier {
    fn snapshot(
        &self,
        entity: &str,
        entity_id: &str,
    ) -> sync_core::Result<Option<serde_json::Value>> {
        Self::serve(entity)?;
        let Some(lib) = self.notes.library() else {
            return Ok(None);
        };
        match lib.read(entity_id) {
            Ok((content, meta)) => Ok(Some(
                serde_json::json!({ "content": content, "title": meta.title }),
            )),
            Err(_) => Ok(None),
        }
    }

    fn apply_upsert(
        &self,
        entity: &str,
        entity_id: &str,
        value: &serde_json::Value,
    ) -> sync_core::Result<()> {
        Self::serve(entity)?;
        let Some(lib) = self.notes.library() else {
            return Err(sync_core::SyncError::NotReady("笔记库未就绪".into()));
        };
        let content = value.get("content").and_then(|v| v.as_str()).unwrap_or("");
        // 存在 → write；不存在 → create（远端新建的笔记）
        let exists = lib.read(entity_id).is_ok();
        let r = if exists {
            lib.write(entity_id, content)
        } else {
            lib.create(entity_id, content).map(|_| ())
        };
        r.map_err(|e| sync_core::SyncError::Apply(e.to_string()))
    }

    fn apply_delete(&self, entity: &str, entity_id: &str) -> sync_core::Result<()> {
        Self::serve(entity)?;
        let Some(lib) = self.notes.library() else {
            return Err(sync_core::SyncError::NotReady("笔记库未就绪".into()));
        };
        // 已不存在视为成功（幂等）
        let _ = lib.delete(entity_id);
        Ok(())
    }
}

/// SYNC ← 发现层的地址桥（09 §10.2 T-B5-7）。
///
/// 方向：sync 只问"这台设备的拨号地址是什么"，不认识 kvm；kvm 只回答"发现表里有没有
/// 它"，不认识 sync。两边因此可以各测各的，桥本身薄到只值一次查表。
/// 答不出来（发现层未就绪 / 设备离线 / 未配对）一律 `None`＝"不知道"，
/// 由 sync-core 的分派口决定下一步是回落还是如实失败——这里绝不塞占位地址。
struct KvmPeerAddrResolver {
    kvm: Arc<KvmModule>,
}

impl sync_core::PeerAddrResolver for KvmPeerAddrResolver {
    fn resolve(&self, device_id: &str) -> Option<String> {
        let Ok(peers) = self.kvm.discovered_peers() else {
            // 发现服务还没装配（KVM 未 init / 已 stop）：答"不知道"，让上层回落或如实失败
            return None;
        };
        sync_addr_of(&peers, device_id)
    }
}

/// 从邻居快照里取某设备的 sync 地址（纯函数：桥的形状可以脱离装配单测）
fn sync_addr_of(peers: &[kvm_core::PeerInfo], device_id: &str) -> Option<String> {
    peers
        .iter()
        .find(|p| p.device_id == device_id)
        .map(|p| p.sync_addr())
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::ports::ScreenRect;
    use sync_core::PeerAddrResolver;

    fn peer(device_id: &str, ip: &str, sync_port: u16) -> kvm_core::PeerInfo {
        kvm_core::PeerInfo {
            device_id: device_id.into(),
            device_name: device_id.into(),
            pubkey_fingerprint: String::new(),
            tcp_port: 49600,
            caps: vec![],
            addr: format!("{ip}:49601").parse().unwrap(),
            screen: ScreenRect::default(),
            sync_port,
        }
    }

    /// 两枚同名常量必须相等（09 §10.2 T-B5-7）：kvm-core 不 import sync-core，
    /// 所以"对端未宣告端口时按哪个口试"这件事在两边各写了一份。写死两次是没办法的
    /// 架构代价，等值漂移则是没人报警的 bug——故在**同时看得见两枚常量**的这一层钉住。
    #[test]
    #[allow(non_snake_case)]
    fn syncDefaultPort_agreesBetweenKvmAndSync() {
        assert_eq!(
            kvm_core::discovery::DEFAULT_SYNC_PORT,
            sync_core::DEFAULT_SYNC_PORT,
            "发现层的回落端口与 sync 的默认监听端口飘了：邻居表里未宣告端口的设备会被拨向错误的口"
        );
    }

    /// 桥的查表形状：命中给 sync_addr、未命中给 None（未宣告端口时按默认口组合）
    #[test]
    #[allow(non_snake_case)]
    fn kvmAddrBridge_hitMissFromDiscoveryTable() {
        let table = vec![
            peer("laptop-b", "192.168.1.12", 49821),
            peer("pc-c", "192.168.1.13", 0),
        ];
        assert_eq!(
            sync_addr_of(&table, "laptop-b").as_deref(),
            Some("192.168.1.12:49821")
        );
        assert_eq!(
            sync_addr_of(&table, "pc-c").as_deref(),
            Some("192.168.1.13:49820"),
            "旧端未宣告端口 ⇒ 按两端同默认口的运维约定组合，猜错会连接失败并记 last_error（不静默）"
        );
        assert_eq!(
            sync_addr_of(&table, "phone-d"),
            None,
            "不在表里＝不知道，不是 127.0.0.1"
        );
        assert_eq!(sync_addr_of(&[], "laptop-b"), None, "空表同样如实");
    }

    /// 发现层未就绪（KVM 模块 init 之前）：桥必须答 None 而不是 panic／占位地址
    #[test]
    #[allow(non_snake_case)]
    fn kvmAddrBridge_notReadyYieldsNone() {
        let kvm = Arc::new(KvmModule::new());
        let bridge = KvmPeerAddrResolver { kvm: kvm.clone() };
        assert_eq!(bridge.resolve("whatever"), None);
        // 同一方向的另一颗螺丝：端口注入读的是 sync-core 现值，不是 kvm 自己的猜测
        let sync = Arc::new(sync_core::SyncModule::new(
            &std::env::temp_dir().join(format!("nf_state_port_{}", std::process::id())),
        ));
        assert_eq!(sync.port(), sync_core::DEFAULT_SYNC_PORT);
        sync.set_port(49860);
        assert_eq!(
            sync.port(),
            49860,
            "端口注入后宣告必须跟着现值走（常量会过期，现值不会）"
        );
        kvm.set_sync_port(sync.port());
        // 注入口本身可调用即合规（宣告真值走 kvm-core 的单测），这里只锁装配方向
    }
}
