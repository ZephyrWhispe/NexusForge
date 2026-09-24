use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

// ======================== 自动化与拓展（M14 A1–A3，docs/impl/07） ========================

fn auto_err(e: automation_core::AutomationError) -> AppError {
    AppError::module(e.code(), e.to_string(), None)
}

/// 规则清单
#[tauri::command]
pub async fn automation_rules_list(
    state: State<'_, HostState>,
) -> Result<Vec<automation_core::rule::Rule>, AppError> {
    Ok(state.automation.rules())
}

/// 保存规则（新增/覆盖；校验 + rules.json 持久化）
#[tauri::command]
pub async fn automation_save_rule(
    rule: automation_core::rule::Rule,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state.automation.save_rule(rule).map_err(auto_err)
}

/// 删除规则
#[tauri::command]
pub async fn automation_delete_rule(
    id: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    state.automation.delete_rule(&id).map_err(auto_err)
}

/// 启停规则
#[tauri::command]
pub async fn automation_toggle_rule(
    id: String,
    enabled: bool,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    state.automation.toggle_rule(&id, enabled).map_err(auto_err)
}

/// 死信队列（UI 死信面板）
#[tauri::command]
pub async fn automation_dead_letters(
    state: State<'_, HostState>,
) -> Result<Vec<automation_core::engine::DeadLetter>, AppError> {
    Ok(state
        .automation
        .engine()
        .map(|e| e.dead_letters())
        .unwrap_or_default())
}

/// 重放死信（动作成功移除；仍失败以新 id 重新入队）
#[tauri::command]
pub async fn automation_replay(
    dead_id: String,
    rule_id: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let engine = state
        .automation
        .engine()
        .ok_or_else(|| AppError::module("AUTO_EXEC_001", "规则引擎未初始化", None))?;
    // 规则已删除的死信无法重放（动作语义随规则上下文）
    let rule = state
        .automation
        .rules()
        .into_iter()
        .find(|r| r.id == rule_id)
        .ok_or_else(|| {
            AppError::module(
                "AUTO_RULE_001",
                format!("规则 {rule_id} 已删除，无法重放"),
                None,
            )
        })?;
    engine.replay(&dead_id, &rule).map_err(auto_err)
}

/// 执行历史（T-B7-14：旧→新；limit 有界取最近 n 条，缺省整环 ≤500）
#[tauri::command]
pub async fn automation_runs_get(
    limit: Option<u32>,
    state: State<'_, HostState>,
) -> Result<Vec<automation_core::RunRecord>, AppError> {
    Ok(state.automation.runs(limit.map(|l| l as usize)))
}

/// 干跑预览（T-B7-15）：纯规划零端口触达——不发事件、不 exec、不触 handler；
/// 清单逐动作标注"仅展示/真执行风险"，供"干跑→清单确认→执行"闸门前展示。
#[tauri::command]
pub async fn automation_dry_run(
    rule_id: String,
    sample_event: serde_json::Value,
    state: State<'_, HostState>,
) -> Result<Vec<automation_core::ActionPlan>, AppError> {
    let rule = state
        .automation
        .rules()
        .into_iter()
        .find(|r| r.id == rule_id)
        .ok_or_else(|| AppError::module("AUTO_RULE_001", format!("规则 {rule_id} 不存在"), None))?;
    Ok(automation_core::plan_rule(
        &rule,
        &automation_core::EventCtx {
            payload: sample_event,
        },
    ))
}

/// 一钮全清死信（T-B7-15：内存 + dead_letters.json 同清）。
/// 权限面注：清历史 = 抹证据，此命令**不给辅助窗**（security_config 负例钉住）。
#[tauri::command]
pub async fn automation_dead_clear(state: State<'_, HostState>) -> Result<(), AppError> {
    if let Some(engine) = state.automation.engine() {
        engine.clear_dead();
    }
    Ok(())
}

/// 插件清单（A6：扫描插件库）
#[tauri::command]
pub async fn automation_plugins_list(
    state: State<'_, HostState>,
) -> Result<Vec<automation_core::PluginInfo>, AppError> {
    Ok(state
        .automation
        .plugin_store()
        .map(|s| s.list())
        .unwrap_or_default())
}

/// 从本地目录安装插件（读 {src}/manifest.json + entry → 校验 → 入库；A6 v1 市场客户端 = 本地导入）
#[tauri::command]
pub async fn automation_plugin_install(
    src_dir: String,
    state: State<'_, HostState>,
) -> Result<automation_core::PluginManifest, AppError> {
    let store = state
        .automation
        .plugin_store()
        .ok_or_else(|| AppError::module("AUTO_EXEC_001", "插件库未初始化", None))?;
    tauri::async_runtime::spawn_blocking(move || {
        store.install_from_dir(std::path::Path::new(&src_dir))
    })
    .await
    .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))?
    .map_err(auto_err)
}

/// 删除插件
#[tauri::command]
pub async fn automation_plugin_remove(
    id: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let store = state
        .automation
        .plugin_store()
        .ok_or_else(|| AppError::module("AUTO_EXEC_001", "插件库未初始化", None))?;
    tauri::async_runtime::spawn_blocking(move || store.remove(&id))
        .await
        .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))?
        .map_err(auto_err)
}
