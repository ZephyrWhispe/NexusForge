//! D-26 原生托盘（docs/impl/01 S6.3）：tray-icon 构建 + TrayProvider 菜单聚合。
//!
//! - 菜单 = 宿主固定段（打开主窗口/退出）+ `aggregate_tray` 模块段（Error 置灰在聚合层）
//! - `host.module_state` 事件驱动整建重建（run_on_main_thread + set_menu）
//! - `vault.auto_lock_warning` → `set_title` 预警标题（D-24 接线；tray-icon 0.24.2
//!   无 balloon API，标题告警 + 前端 toast 为实际两条通道）；
//!   `vault.state_changed` → 标题复位

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use host_core::capability::{aggregate_tray, TrayProvider, TraySection};
use host_core::events::{Event, EventBus};
use host_core::ports::Ports;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::state::HostState;

/// 本应用托盘句柄（R = Wry）
type Tray = tauri::tray::TrayIcon<tauri::Wry>;
/// `{module}:{item_id}` → 动作闭包的热查表（菜单事件回调线程取用）
type ActionTable = Arc<Mutex<HashMap<String, Arc<dyn Fn() + Send + Sync>>>>;

/// 宿主固定段 id（不经 TrayProvider 路由）
pub const ID_OPEN_MAIN: &str = "host:open_main";
pub const ID_QUIT: &str = "host:quit";
const TOOLTIP: &str = "NexusForge";

/// 菜单计划行（纯函数输出，形状钉死用；Sep = 分隔线）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRow {
    Sep,
    Item {
        id: String,
        label: String,
        enabled: bool,
    },
}

/// 宿主固定段 + 模块段（priority 升序、段间分隔线）→ 线性菜单计划（D-26 验收④）
pub fn menu_plan(sections: &[TraySection]) -> Vec<PlanRow> {
    let mut rows = vec![PlanRow::Item {
        id: ID_OPEN_MAIN.into(),
        label: "打开主窗口".into(),
        enabled: true,
    }];
    for sec in sections {
        rows.push(PlanRow::Sep);
        for item in &sec.items {
            rows.push(PlanRow::Item {
                id: format!("{}:{}", sec.module, item.id),
                label: item.label.clone(),
                enabled: item.enabled,
            });
        }
    }
    rows.push(PlanRow::Sep);
    rows.push(PlanRow::Item {
        id: ID_QUIT.into(),
        label: "退出".into(),
        enabled: true,
    });
    rows
}

/// `{module}:{item_id}` → 动作闭包查表（重复注册以最后者为准，与 Ports 同语义）
pub fn collect_actions(abilities: &Ports) -> HashMap<String, Arc<dyn Fn() + Send + Sync>> {
    let mut map = HashMap::new();
    for p in abilities.get_all::<dyn TrayProvider>() {
        let module = p.info().id.to_owned();
        for a in p.tray_actions() {
            map.insert(format!("{module}:{}", a.item_id), a.action);
        }
    }
    map
}

/// 查表点击路由：未知/畸形 id（含无冒号）→ None，静默忽略不 panic（D-26 验收②）
pub fn lookup_action(
    table: &HashMap<String, Arc<dyn Fn() + Send + Sync>>,
    id: &str,
) -> Option<Arc<dyn Fn() + Send + Sync>> {
    table.get(id).cloned()
}

/// vault.auto_lock_warning 载荷解析（纯函数）：缺 lock_in_secs/类型错 → None（验收③）
pub fn parse_lock_in_secs(ev: &Event) -> Option<u64> {
    ev.payload.get("lock_in_secs")?.as_u64()
}

/// 预警标题文案（托盘 set_title，Windows 悬浮提示）
pub fn warning_title(lock_in_secs: u64) -> String {
    format!("密码库将在 {lock_in_secs}s 后自动锁定")
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn build_menu(app: &AppHandle, rows: &[PlanRow]) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    for row in rows {
        match row {
            PlanRow::Sep => {
                menu.append(&PredefinedMenuItem::separator(app)?)?;
            }
            PlanRow::Item { id, label, enabled } => {
                menu.append(&MenuItem::with_id(
                    app,
                    id.clone(),
                    label.clone(),
                    *enabled,
                    None::<&str>,
                )?)?;
            }
        }
    }
    Ok(menu)
}

/// 构建原生托盘并挂两个刷新协程（模块段随 bootstrap 的 host.module_state 迁移出现）。
/// 失败仅 warn 不阻断启动（托盘是增强面，非启动关键路径）。
pub fn build(app: &AppHandle, host: &HostState) -> Result<(), String> {
    // Windows 下 Shell_NotifyIcon 必须有 hIcon：无默认窗口图标时直接放弃本次托盘
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| "应用无默认窗口图标（bundle.icon 为空），跳过托盘构建".to_string())?;

    let actions: ActionTable = Arc::new(Mutex::new(collect_actions(host.registry.abilities())));

    let rows = menu_plan(&aggregate_tray(host.registry.abilities()));
    let menu = build_menu(app, &rows).map_err(|e| e.to_string())?;
    let tray = TrayIconBuilder::with_id("nf-main-tray")
        .menu(&menu)
        .icon(icon)
        .tooltip(TOOLTIP)
        .on_menu_event({
            let actions = actions.clone();
            move |app: &AppHandle, event: MenuEvent| {
                let id = event.id().0.clone();
                match id.as_str() {
                    ID_OPEN_MAIN => show_main(app),
                    ID_QUIT => app.exit(0),
                    _ => {
                        let f = lookup_action(&actions.lock().unwrap(), &id);
                        match f {
                            Some(f) => f(),
                            None => {
                                tracing::debug!(id, "托盘菜单项无注册动作，忽略");
                            }
                        }
                    }
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            // 左键抬起 = 显示并聚焦主窗口（托盘最小可用面，D-26 决策①）
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)
        .map_err(|e| e.to_string())?;

    spawn_rebuild(app.clone(), host.bus.clone(), actions, tray.clone());
    spawn_warning_title(app.clone(), host.bus.clone(), tray);
    Ok(())
}

/// host.module_state / clipboard.capture_state → 整建重建菜单并刷新动作表
/// （重建代价 ≤10 项，忽略增量同步）。后者是 §8-④ 暂停项的动态标签所需：
/// 标签在 `tray_menu_items()` 读运行态原子位，不重建则托盘停留在旧文案。
fn spawn_rebuild(app: AppHandle, bus: Arc<EventBus>, actions: ActionTable, tray: Tray) {
    let Ok(mut rx) = bus.subscribe("host.module_state") else {
        tracing::warn!("host.module_state 订阅失败，托盘菜单不会随模块状态刷新");
        return;
    };
    let Ok(mut rx_capture) = bus.subscribe("clipboard.capture_state") else {
        tracing::warn!("clipboard.capture_state 订阅失败，托盘暂停项标签将不翻转");
        return;
    };
    tauri::async_runtime::spawn(async move {
        loop {
            match tokio::select! {
                r = rx.recv() => r,
                r = rx_capture.recv() => r,
            } {
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
            let app2 = app.clone();
            let actions2 = actions.clone();
            let tray2 = tray.clone();
            let _ = app.clone().run_on_main_thread(move || {
                let (built, fresh) = {
                    let host = app2.state::<HostState>();
                    let abilities = host.registry.abilities();
                    let rows = menu_plan(&aggregate_tray(abilities));
                    (build_menu(&app2, &rows), collect_actions(abilities))
                };
                match built {
                    Ok(menu) => {
                        *actions2.lock().unwrap() = fresh;
                        if let Err(e) = tray2.set_menu(Some(menu)) {
                            tracing::warn!(error = %e, "托盘菜单重建失败");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "托盘菜单构建失败，保留旧菜单"),
                }
            });
        }
    });
}

/// D-24 接线：预警 → 托盘标题告警（tray-icon 0.24.2 无 balloon API，标题即 Windows 悬浮提示文案）；
/// vault 状态变化（含锁定完成）→ 标题复位
fn spawn_warning_title(app: AppHandle, bus: Arc<EventBus>, tray: Tray) {
    let Ok(mut rx_warn) = bus.subscribe("vault.auto_lock_warning") else {
        tracing::warn!("vault.auto_lock_warning 订阅失败，托盘标题预警不可用");
        return;
    };
    let Ok(mut rx_state) = bus.subscribe("vault.state_changed") else {
        tracing::warn!("vault.state_changed 订阅失败，托盘标题将不复位");
        return;
    };
    {
        let app = app.clone();
        let tray = tray.clone();
        tauri::async_runtime::spawn(async move {
            while let Ok(ev) = rx_warn.recv().await {
                let Some(secs) = parse_lock_in_secs(&ev) else {
                    tracing::warn!("vault.auto_lock_warning 载荷非法，忽略");
                    continue;
                };
                let (app2, tray2) = (app.clone(), tray.clone());
                let _ = app2.run_on_main_thread(move || {
                    if let Err(e) = tray2.set_title(Some(warning_title(secs))) {
                        tracing::warn!(error = %e, "托盘预警标题设置失败");
                    }
                });
            }
        });
    }
    tauri::async_runtime::spawn(async move {
        while rx_state.recv().await.is_ok() {
            let (app2, tray2) = (app.clone(), tray.clone());
            let _ = app2.run_on_main_thread(move || {
                let _ = tray2.set_title(Some(TOOLTIP));
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::capability::TrayMenuItem;
    use host_core::events::Event;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn item(id: &str, label: &str, enabled: bool) -> TrayMenuItem {
        TrayMenuItem {
            id: id.into(),
            label: label.into(),
            enabled,
        }
    }

    fn section(module: &str, priority: u8, items: Vec<TrayMenuItem>) -> TraySection {
        TraySection {
            module: module.into(),
            priority,
            items,
        }
    }

    #[test]
    fn menu_plan_host_only_when_sections_empty() {
        // 验收④：模块段为空时宿主固定段完整（打开主窗口 / 分隔 / 退出）
        assert_eq!(
            menu_plan(&[]),
            vec![
                PlanRow::Item {
                    id: ID_OPEN_MAIN.into(),
                    label: "打开主窗口".into(),
                    enabled: true
                },
                PlanRow::Sep,
                PlanRow::Item {
                    id: ID_QUIT.into(),
                    label: "退出".into(),
                    enabled: true
                },
            ]
        );
    }

    #[test]
    fn menu_plan_prefixes_module_ids_and_keeps_enabled_flags() {
        let sections = vec![
            section(
                "clipboard",
                20,
                vec![item("quick_panel", "打开剪切板面板", true)],
            ),
            section(
                "vault",
                30,
                vec![item("lock_now", "立即锁定密码库", false)], // Error 置灰由聚合层写入
            ),
        ];
        let rows = menu_plan(&sections);
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, PlanRow::Item { .. }))
                .count(),
            4,
            "宿主 2 项 + 模块 2 项"
        );
        assert!(rows.iter().any(
            |r| matches!(r, PlanRow::Item { id, enabled: true, .. } if id == "clipboard:quick_panel")
        ));
        assert!(rows.iter().any(
            |r| matches!(r, PlanRow::Item { id, enabled: false, .. } if id == "vault:lock_now")
        ));
        // 每段前恰一条分隔线 + 尾段前一条 = 3 条 Sep
        assert_eq!(rows.iter().filter(|r| **r == PlanRow::Sep).count(), 3);
    }

    #[test]
    fn lookup_action_hits_and_misses_without_panic() {
        // 验收②：命中触发闭包；未知/畸形 id（无冒号、空串、错 item）→ None 不 panic
        let fired = Arc::new(AtomicU32::new(0));
        let f = fired.clone();
        let mut table: HashMap<String, Arc<dyn Fn() + Send + Sync>> = HashMap::new();
        table.insert(
            "clipboard:quick_panel".into(),
            Arc::new(move || {
                f.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let hit = lookup_action(&table, "clipboard:quick_panel").unwrap();
        hit();
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        for miss in ["", "nocolon", "clipboard:nope", "host:open_main", ":x"] {
            assert!(lookup_action(&table, miss).is_none(), "{miss} 必须 None");
        }
    }

    #[test]
    fn parse_lock_in_secs_positive_and_negatives() {
        // 验收③：合法载荷 → Some；缺字段/类型错/非对象 → None
        let good = Event::new(
            "vault.auto_lock_warning",
            "vault",
            json!({ "lock_in_secs": 30 }),
        );
        assert_eq!(parse_lock_in_secs(&good), Some(30));
        for bad in [
            json!({}),
            json!({ "lock_in_secs": "30" }),
            json!({ "lock_in_secs": -5 }),
            json!(null),
        ] {
            let ev = Event::new("vault.auto_lock_warning", "vault", bad);
            assert_eq!(parse_lock_in_secs(&ev), None, "非法载荷必须 None");
        }
        assert_eq!(warning_title(30), "密码库将在 30s 后自动锁定");
    }
}
