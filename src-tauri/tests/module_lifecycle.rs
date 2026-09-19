//! M7 模块生命周期回归：
//! ① 空 Ports 下每个硬依赖端口模块 init 必须失败且错误点名缺失端口
//! （旧库迁移测试已在各自 crate：sync legacy 迁移、proxy/term/vault 重开保留，
//! 本文件补的是宿主装配层"端口缺失 = 可操作报错"这一环）；
//! ② 无硬端口模块（vault/editor/file/desktop）完整走一遍
//! init_all→start_all→stop_all→restart，状态迁移同时经 status_all（唯一源）
//! 与 host.module_state 事件可观测。

use std::sync::Arc;

use host_core::error::ModuleError;
use host_core::events::{Event, EventBus};
use host_core::module::{Module, ModuleContext, ModuleState};
use host_core::ports::Ports;
use host_core::registry::ModuleRegistry;

use automation_core::module::AutomationModule;
use desktop_core::DesktopModule;
use editor_core::EditorModule;
use file_core::FileModule;
use notes_core::NotesModule;
use proxy_core::ProxyModule;
use sys_core::SysModule;
use term_core::TermModule;
use vault_core::VaultModule;

fn ctx(dir: &std::path::Path) -> Arc<ModuleContext> {
    Arc::new(ModuleContext {
        app_data_dir: dir.to_path_buf(),
        ports: Arc::new(Ports::new()),
        event_bus: Arc::new(EventBus::new()),
    })
}

/// ① 硬端口缺失：init 必须 Err 且消息点名端口（宿主未注册 win-integration 时的可操作报错）
#[test]
fn empty_ports_init_names_missing_port() {
    let tmp = tempfile::tempdir().unwrap();
    let c = ctx(tmp.path());
    let cases: Vec<(&str, Arc<dyn Module>, &str)> = vec![
        (
            "clipboard",
            Arc::new(clipboard_core::module::ClipboardModule::new()),
            "ClipboardPort",
        ),
        (
            "screenshot",
            Arc::new(screenshot_core::module::ScreenshotModule::new()),
            "CapturePort",
        ),
        (
            "ocr",
            Arc::new(ocr_core::module::OcrModule::new()),
            "OcrPort",
        ),
        ("proxy", Arc::new(ProxyModule::new()), "SysProxyPort"),
        (
            "notes",
            Arc::new(NotesModule::new(tmp.path())),
            "StoragePort",
        ),
        ("term", Arc::new(TermModule::new(tmp.path())), "ConptyPort"),
        ("sys", Arc::new(SysModule::new(tmp.path())), "PerfPort"),
        (
            "automation",
            Arc::new(AutomationModule::new(tmp.path())),
            "ShellPort",
        ),
    ];
    for (id, module, port) in cases {
        let e: ModuleError = module
            .init(c.clone())
            .err()
            .unwrap_or_else(|| panic!("{id} 在空 Ports 下 init 不应成功"));
        assert!(
            e.to_string().contains(port),
            "{id} 的 init 错误应点名 {port}，实际：{e}"
        );
        // 失败即 Error 态由注册表 apply_state 负责；模块自感知路径不得谎报 Running
        assert!(!matches!(module.status(), ModuleState::Running), "{id}");
    }
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push((
            ev.payload["module"].as_str().unwrap_or_default().into(),
            ev.payload["state"].as_str().unwrap_or_default().into(),
        ));
    }
    out
}

#[tokio::test]
async fn portfree_roundtrip_and_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = Arc::new(EventBus::new());
    let c = Arc::new(ModuleContext {
        app_data_dir: tmp.path().to_path_buf(),
        ports: Arc::new(Ports::new()),
        event_bus: bus.clone(),
    });
    let registry = Arc::new(ModuleRegistry::new(bus.clone()));
    registry.register(Arc::new(VaultModule::new())).unwrap();
    registry
        .register(Arc::new(EditorModule::new(tmp.path())))
        .unwrap();
    registry.register(Arc::new(FileModule::new())).unwrap();
    registry
        .register(Arc::new(DesktopModule::new(tmp.path())))
        .unwrap();

    let mut rx = bus.subscribe("host.module_state").unwrap();

    // init：全部成功 → Stopped，事件含 4 个 Stopped
    let init = registry.init_all(c).await;
    for (id, r) in &init {
        r.as_ref().unwrap_or_else(|e| panic!("{id} init: {e}"));
    }
    let states = registry.status_all();
    assert!(states.iter().all(|(_, s)| *s == ModuleState::Stopped));
    for ev in drain(&mut rx) {
        // init 成功迁移事件同样发布（D-16），不再只有 start
        assert_eq!(ev.1, "Stopped");
    }

    // start：全部 Running
    let start = registry.start_all().await;
    for (id, r) in &start {
        r.as_ref().unwrap_or_else(|e| panic!("{id} start: {e}"));
    }
    assert!(registry
        .status_all()
        .iter()
        .all(|(_, s)| *s == ModuleState::Running));
    assert!(drain(&mut rx).iter().all(|(_, s)| s == "Running"));

    // restart 单模块：vault 回到 Running，其余不受扰
    registry.restart("vault").await.unwrap();
    let after: Vec<_> = registry.status_all().into_iter().collect();
    assert_eq!(
        after.iter().find(|(id, _)| id == "vault").unwrap().1,
        ModuleState::Running
    );
    assert_eq!(
        after
            .iter()
            .filter(|(_, s)| *s == ModuleState::Running)
            .count(),
        4
    );
    // 重启链路：Stopped（stop 后）→ Stopped（init 不迁移不发布）→ Running（start 发布）
    assert!(drain(&mut rx)
        .iter()
        .any(|(m, s)| m == "vault" && s == "Running"));

    // 未注册 id 的 restart 报错走 AppError 码面（前端 parseAppError 依赖）
    let e = registry.restart("nope").await.unwrap_err();
    assert_eq!(e.code(), "HOST_REGISTRY_001");

    // stop_all 逆序停机 → Stopped
    registry.stop_all().await;
    assert!(registry
        .status_all()
        .iter()
        .all(|(_, s)| *s == ModuleState::Stopped));
}
