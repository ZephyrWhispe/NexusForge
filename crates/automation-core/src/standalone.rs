//! 独立规则执行入口（docs/impl/07 A4）：Windows Task Scheduler 触发
//! `nexusforge.exe --run-rule {id}` 时无完整宿主——从 rules.json 取启用规则
//! 经注入的最小 ActionHandler 执行（冷却表独立进程内为空，不拦截）。

use std::path::Path;
use std::sync::Arc;

use serde_json::json;

use crate::engine::{ActionHandler, RuleEngine};
use crate::error::Result;
use crate::rule::Rule;

/// 从 rules.json 执行指定规则（规则不存在/停用返回 false；fire 后动作失败不报错——死信随进程退出丢弃）
pub fn run_rule_standalone(
    rules_path: &Path,
    rule_id: &str,
    handler: Arc<dyn ActionHandler>,
) -> Result<bool> {
    let rules: Vec<Rule> = std::fs::read(rules_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let Some(rule) = rules.iter().find(|r| r.id == rule_id && r.enabled) else {
        return Ok(false);
    };
    let engine = RuleEngine::new(handler);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    // Schedule 规则 payload 恒 {}（无 when 场景为主；带 when 的定时规则按空 payload 求值）
    engine.fire(rule, &json!({}), now_ms);
    Ok(true)
}
