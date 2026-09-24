//! D-26 原生托盘（docs/impl/01 S6.3）：tray-icon 构建 + TrayProvider 菜单聚合。
//!
//! - 菜单 = 宿主固定段（打开主窗口/退出）+ `aggregate_tray` 模块段（Error 置灰在聚合层）
//! - `host.module_state` 事件驱动整建重建（run_on_main_thread + set_menu）
//! - `vault.auto_lock_warning` → `set_title` 预警标题（D-24 接线；tray-icon 0.24.2
//!   无 balloon API，标题告警 + 前端 toast 为实际两条通道）；
//!   `vault.state_changed` → 标题复位
//! - T-B7-11（09 §7.2）：`sys.metrics` → 负载标题 "CPU x% · MEM y%"（≥5s 节流，
//!   假时钟可测；配置 `tray.load_display` 默认 false——托盘常态改变须用户显式开）。
//!   标题归属唯一经 `tray_title_plan`（预警 > 负载）：预警在场负载让位、
//!   预警结束负载原样恢复（负载字段从不被预警抹除，恢复是结构性的）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use host_core::capability::{aggregate_tray, TrayProvider, TraySection};
use host_core::config::ConfigStore;
use host_core::events::{Event, EventBus};
use host_core::ports::Ports;
use serde::{Deserialize, Serialize};
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

// ---------------- T-B7-11：托盘负载显示（标题归属 + tray 段配置） ----------------

/// 负载刷新最小间隔（假时钟注入即钉在事件 `ts_ms` 上测）
pub const TRAY_LOAD_GAP_MS: i64 = 5000;
/// 负载文案硬上限（任务书：`CPU x% · MEM y%` 恒 ≤20 字符；满量程 "CPU 100% · MEM 100%" = 19）
pub const TRAY_LOAD_MAX_CHARS: usize = 20;

/// 托盘宿主配置（`{config}/tray.json`，非任何模块段）。唯一读者 = 本文件负载臂；
/// 坏盘/缺键/类型错一律回落默认（fail-closed：托盘保持静默，不自作主张改用户可见面）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TrayConfig {
    /// 是否在托盘标题显示 "CPU x% · MEM y%"（常态改变须用户显式开，故默认 false）
    pub load_display: bool,
}

/// tray 段 schema（设置中心 SchemaForm 渲染 + host_config_set 写侧校验的同一份）
pub fn tray_config_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "load_display": {
                "type": "boolean",
                "title": "托盘显示资源负载",
                "description": "标题显示「CPU x% · MEM y%」，≥5s 节流刷新；预警到达时自动让位",
                "default": false
            }
        },
        "additionalProperties": false
    })
}

fn parse_tray_config(v: serde_json::Value) -> TrayConfig {
    serde_json::from_value(v).unwrap_or_default()
}

fn read_tray_load_display(config: &ConfigStore) -> bool {
    let cfg = match config.get_module("tray") {
        Ok(v) => parse_tray_config(v),
        Err(e) => {
            tracing::warn!(error = %e, "tray 段配置读取失败，按默认（负载显示关）处理");
            TrayConfig::default()
        }
    };
    cfg.load_display
}

/// 负载文案（整数百分比；构造上界 "CPU 100% · MEM 100%" = 19 字符 ≤ 20，debug 断言当场自证）
pub fn tray_load_text(cpu_pct: f64, mem_pct: f64) -> String {
    let s = format!("CPU {}% · MEM {}%", cpu_pct.round(), mem_pct.round());
    debug_assert!(s.chars().count() <= TRAY_LOAD_MAX_CHARS);
    s
}

/// 标题归属唯一裁决（任务书签名）：**预警 > 负载**；两者皆无 → None（调用侧回落到静态 tooltip）
pub fn tray_title_plan(warning: Option<&str>, load: Option<String>) -> Option<String> {
    warning.map(str::to_owned).or(load)
}

/// 消费一条 `sys.metrics` 事件：返回 Some(新负载文案) 当且仅当本拍应刷新。
/// 关配置 → 恒 None 且零副作用（不消耗节流窗口）；缺字段 / `mem_total==0` →
/// None（无事实源不编 0）；距上次放行 <5s → None。放行即推进假时钟窗口。
pub fn tray_metrics_tick(
    last_ms: &mut Option<i64>,
    ev: &Event,
    load_display: bool,
) -> Option<String> {
    if !load_display {
        return None;
    }
    let p = &ev.payload;
    let now_ms = p.get("ts_ms")?.as_i64()?;
    let cpu = p.get("cpu")?.as_f64()?;
    let mem_used = p.get("mem_used")?.as_u64()?;
    let mem_total = p.get("mem_total")?.as_u64()?;
    if mem_total == 0 {
        return None;
    }
    if let Some(last) = *last_ms {
        if now_ms - last < TRAY_LOAD_GAP_MS {
            return None;
        }
    }
    *last_ms = Some(now_ms);
    Some(tray_load_text(
        cpu,
        (mem_used as f64 / mem_total as f64) * 100.0,
    ))
}

/// 托盘标题三方（预警/负载/节流窗口）共享态——三个事件臂共写一份，
/// 归属永远经 `tray_title_plan` 现算，不存在"谁最后 set_title 谁赢"的时序竞态。
#[derive(Default)]
struct TrayTitles {
    warning: Option<String>,
    load: Option<String>,
    last_load_ms: Option<i64>,
}

fn apply_titles(app: &AppHandle, tray: &Tray, t: &TrayTitles) {
    let title =
        tray_title_plan(t.warning.as_deref(), t.load.clone()).unwrap_or_else(|| TOOLTIP.to_owned());
    let (app2, tray2) = (app.clone(), tray.clone());
    let _ = app2.run_on_main_thread(move || {
        if let Err(e) = tray2.set_title(Some(title)) {
            tracing::warn!(error = %e, "托盘标题设置失败");
        }
    });
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
    // tray 段 schema 先于图标守卫登记：图标缺失只放弃托盘本体，设置中心的
    // 宿主段（读写 tray.json）不该因此 404
    host.config.register_schema("tray", tray_config_schema());
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
    spawn_titles(app.clone(), host.bus.clone(), host.config.clone(), tray);
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

/// D-24 接线 + T-B7-11：三事件臂共写一份 `TrayTitles`，标题归属现算（预警 > 负载）。
/// - `vault.auto_lock_warning` → 预警位（负载让位；tray-icon 0.24.2 无 balloon API，
///   标题即 Windows 悬浮提示文案）
/// - `vault.state_changed`（含锁定完成）→ 预警位清空 → 负载（若开）结构性恢复
/// - `sys.metrics`（1s 合并节拍，零新主题）→ ≥5s 节流消费，`tray.load_display`
///   关时恒不更新；盘读为 1KB 小文件、1Hz，读取代价远低于缓存复杂度
fn spawn_titles(app: AppHandle, bus: Arc<EventBus>, config: Arc<ConfigStore>, tray: Tray) {
    let Ok(mut rx_warn) = bus.subscribe("vault.auto_lock_warning") else {
        tracing::warn!("vault.auto_lock_warning 订阅失败，托盘标题预警不可用");
        return;
    };
    let Ok(mut rx_state) = bus.subscribe("vault.state_changed") else {
        tracing::warn!("vault.state_changed 订阅失败，托盘标题将不复位");
        return;
    };
    let Ok(mut rx_metrics) = bus.subscribe("sys.metrics") else {
        tracing::warn!("sys.metrics 订阅失败，托盘负载显示不可用");
        return;
    };
    let titles = Arc::new(Mutex::new(TrayTitles::default()));
    tauri::async_runtime::spawn(async move {
        enum Msg {
            Warn(Event),
            State,
            Metrics(Event),
        }
        loop {
            let msg = tokio::select! {
                r = rx_warn.recv() => r.map(Msg::Warn),
                r = rx_state.recv() => r.map(|_| Msg::State),
                r = rx_metrics.recv() => r.map(Msg::Metrics),
            };
            let msg = match msg {
                Ok(m) => m,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            match msg {
                Msg::Warn(ev) => {
                    let Some(secs) = parse_lock_in_secs(&ev) else {
                        tracing::warn!("vault.auto_lock_warning 载荷非法，忽略");
                        continue;
                    };
                    let mut t = titles.lock().unwrap();
                    t.warning = Some(warning_title(secs));
                    apply_titles(&app, &tray, &t);
                }
                Msg::State => {
                    let mut t = titles.lock().unwrap();
                    t.warning = None;
                    apply_titles(&app, &tray, &t);
                }
                Msg::Metrics(ev) => {
                    let load_display = read_tray_load_display(&config);
                    let mut t = titles.lock().unwrap();
                    if let Some(text) = tray_metrics_tick(&mut t.last_load_ms, &ev, load_display) {
                        if t.load.as_deref() != Some(text.as_str()) {
                            t.load = Some(text);
                            apply_titles(&app, &tray, &t);
                        }
                    }
                }
            }
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

    fn metrics_event(ts_ms: i64, cpu: f64, mem_used: u64, mem_total: u64) -> Event {
        Event::new(
            "sys.metrics",
            "sys",
            json!({ "ts_ms": ts_ms, "cpu": cpu, "mem_used": mem_used, "mem_total": mem_total }),
        )
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-11）字面测试名优先于 rustc 命名惯例
    fn trayPlan_warningOverridesLoad_thenRestores() {
        let text = tray_load_text(12.4, 34.6);
        assert_eq!(text, "CPU 12% · MEM 35%");
        assert!(
            text.chars().count() <= TRAY_LOAD_MAX_CHARS,
            "满量程亦不得超界：CPU 100%/MEM 100% 恰 19 字符"
        );
        let load = Some(text.clone());
        // 预警 > 负载
        assert_eq!(
            tray_title_plan(Some("密码库将在 30s 后自动锁定"), load.clone()).as_deref(),
            Some("密码库将在 30s 后自动锁定")
        );
        // 预警结束后负载恢复——负载字段从不被预警抹除，"让位/恢复"是归属裁决的结构性推论
        assert_eq!(tray_title_plan(None, load.clone()), load);
        assert_eq!(tray_title_plan(Some("x"), None).as_deref(), Some("x"));
        assert_eq!(tray_title_plan(None, None), None);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-11）字面测试名优先于 rustc 命名惯例
    fn trayPlan_throttleGapFiveSeconds() {
        // 假时钟注入 = 事件 payload 的 ts_ms 直接构造，无真实等待
        let mut last: Option<i64> = None;
        assert_eq!(
            tray_metrics_tick(&mut last, &metrics_event(1_000, 10.0, 4, 8), true).as_deref(),
            Some("CPU 10% · MEM 50%")
        );
        assert_eq!(last, Some(1_000));
        // 差 1ms 到 5s：拒，且不消耗窗口（last 原值）
        assert!(tray_metrics_tick(&mut last, &metrics_event(5_999, 20.0, 4, 8), true).is_none());
        assert_eq!(last, Some(1_000));
        // 恰 5s：放行并推进窗口
        assert!(tray_metrics_tick(&mut last, &metrics_event(6_000, 20.0, 4, 8), true).is_some());
        assert_eq!(last, Some(6_000));
        // 缺字段 / mem_total=0：None 且不消耗窗口（无事实源不编假值）
        let bad = Event::new("sys.metrics", "sys", json!({ "cpu": 1.0 }));
        assert!(tray_metrics_tick(&mut last, &bad, true).is_none());
        assert!(tray_metrics_tick(&mut last, &metrics_event(999_999, 1.0, 0, 0), true).is_none());
        assert_eq!(last, Some(6_000));
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-11）字面测试名优先于 rustc 命名惯例
    fn trayPlan_offConfig_emitsNoneAlways() {
        // 托盘常态改变须用户显式开：关位下任何时钟位置都不得产出负载文案
        let mut last: Option<i64> = None;
        for ts in [0, 10_000, 20_000, 30_000] {
            assert_eq!(
                tray_metrics_tick(&mut last, &metrics_event(ts, 42.0, 1, 2), false),
                None,
                "load_display=false 必须恒 None（ts={ts}）"
            );
        }
        assert_eq!(last, None, "关位零副作用：不得消耗节流窗口");
        // 配置解析同纪律：缺键/坏类型/坏盘一律默认 false
        assert!(!parse_tray_config(json!({})).load_display);
        assert!(!parse_tray_config(json!({"load_display": "yes"})).load_display);
        assert!(!parse_tray_config(json!(null)).load_display);
        assert!(parse_tray_config(json!({"load_display": true})).load_display);
    }

    /// 任务书（09 §7.2 T-B7-11）随批：tray 段死键机检。键集恰等（防 schema 假键）、
    /// default 同源；键的"非死"最强机检是行为测双向消费——offConfig 恒 None（上一枚）
    /// 与 onConfig 产出文案（throttle 枚）已成对钉住，这里再防滑出。
    #[test]
    #[allow(non_snake_case)]
    fn dead_config_keys_are_revived_or_removed() {
        let schema = tray_config_schema();
        let props = schema["properties"]
            .as_object()
            .expect("tray schema 应为对象 properties");
        let mut keys: Vec<String> = props.keys().cloned().collect();
        keys.sort();
        let mut fields: Vec<String> = serde_json::to_value(TrayConfig::default())
            .unwrap()
            .as_object()
            .expect("TrayConfig 可序列化")
            .keys()
            .cloned()
            .collect();
        fields.sort();
        assert_eq!(keys, fields, "schema 键集必须恰等 TrayConfig 字段集");
        assert_eq!(keys, vec!["load_display".to_string()]);
        assert_eq!(
            props["load_display"]["default"],
            serde_json::json!(TrayConfig::default().load_display),
            "schema default 必须与结构体默认逐值同源"
        );
        // 真读者在场：读臂按字段取用（注释里的键名不算消费）
        let src = include_str!("tray.rs");
        assert!(
            src.lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .any(|l| l.contains("cfg.load_display")),
            "load_display 必须有 module 外的运行期读者"
        );
    }
}
