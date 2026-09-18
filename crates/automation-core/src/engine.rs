//! A3 动作执行器（docs/impl/07 A3）：单规则内串行、规则间并行；
//! Action 失败按 retry(2, 指数) 后进入死信（UI 可查/重放）。
//! 风暴防护（docs/impl/07 风险标注）：规则冷却表 + automation 自产事件不再触发规则（深度上限的 v1 等效实现）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{AutomationError, Result};
use crate::rule::{Action, Rule};

/// 默认冷却（秒）：cooldown_secs=0 时使用
pub const DEFAULT_COOLDOWN_SECS: u64 = 5;
/// 动作重试次数（指数退避：500ms × 2^n）
pub const ACTION_RETRIES: u32 = 2;
/// 死信队列容量
pub const DEAD_LETTER_CAP: usize = 100;

/// 动作宿主（由宿主注入：OpenUrl 走 ShellPort、Publish 走 EventBus、IpcCommand 映射模块语义、
/// RunScript 走 WASM 沙箱（A5））
pub trait ActionHandler: Send + Sync {
    fn open_url(&self, url: &str) -> Result<()>;
    /// topic 为规则配置字符串；宿主侧查 TOPIC_REGISTRY 还原静态主题（未知主题报错）
    fn publish(&self, topic: &str, payload: serde_json::Value) -> Result<()>;
    /// IpcCommand 语义执行（宿主映射 module+cmd；v1 宿主支持内置白名单）
    fn ipc_command(&self, module: &str, cmd: &str, args: &serde_json::Value) -> Result<()>;
    /// WASM 插件执行（path 支持 "plugin:{id}" 或 wasm 文件路径；func 为空用插件 manifest.func）
    fn run_wasm(&self, path: &str, func: &str) -> Result<()>;
}

/// 死信条目（IPC DTO）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeadLetter {
    pub id: String,
    pub rule_id: String,
    pub rule_name: String,
    pub action: Action,
    pub error: String,
    /// 最后尝试时刻毫秒
    pub at_ms: i64,
}

/// 全局冷却表 + 计数
#[derive(Default)]
pub struct CooldownTable {
    /// rule_id → 上次触发毫秒
    last_fired: Mutex<HashMap<String, i64>>,
    /// 风暴计数：被冷却拦截的次数（超限自动禁用由 module 层判断）
    suppressed: AtomicU64,
}

impl CooldownTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 尝试触发：冷却内返回 false（并计数）；通过则记录本次时间
    pub fn try_fire(&self, rule_id: &str, cooldown_secs: u64, now_ms: i64) -> bool {
        let cooldown = if cooldown_secs == 0 { DEFAULT_COOLDOWN_SECS } else { cooldown_secs };
        let mut table = self.last_fired.lock().expect("冷却表锁污染");
        let gate_ms = cooldown * 1000;
        if let Some(&last) = table.get(rule_id) {
            if now_ms - last < gate_ms as i64 {
                self.suppressed.fetch_add(1, Ordering::SeqCst);
                return false;
            }
        }
        table.insert(rule_id.to_string(), now_ms);
        true
    }
}

/// 执行单个动作（含 retry 指数退避；全部失败 → DeadLetter）
fn run_action(
    handler: &dyn ActionHandler,
    action: &Action,
    rule: &Rule,
    now_ms: i64,
    dead: &mut Vec<DeadLetter>,
) {
    let mut attempt = 0u32;
    loop {
        let action = action.clone();
        let result = match &action {
            Action::Publish { topic, payload } => handler.publish(topic, payload.clone()),
            Action::Notify { title, body } => handler.publish(
                "automation.notify",
                serde_json::json!({ "rule_id": rule.id, "title": title, "body": body }),
            ),
            Action::OpenUrl { url } => handler.open_url(url),
            Action::IpcCommand { module, cmd, args } => handler.ipc_command(module, cmd, args),
            Action::RunScript { path, func } => handler.run_wasm(path, func),
        };
        match result {
            Ok(()) => {
                // 触发审计（前端可显示最近触发）；审计失败不影响动作结果
                let _ = handler.publish(
                    "automation.rule_fired",
                    serde_json::json!({ "rule_id": rule.id, "rule_name": rule.name }),
                );
                return;
            }
            Err(e) if (attempt as u32) < ACTION_RETRIES => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(500u64 << (attempt - 1)));
                tracing::warn!(rule = %rule.id, attempt, error = %e, "动作重试");
            }
            Err(e) => {
                dead.push(DeadLetter {
                    id: uuid::Uuid::now_v7().to_string(),
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    action: action.clone(),
                    error: e.to_string(),
                    at_ms: now_ms,
                });
                return;
            }
        }
    }
}

/// 规则引擎：执行单条规则的全部动作（规则内串行），死信收集
pub struct RuleEngine {
    handler: Arc<dyn ActionHandler>,
    dead: Mutex<Vec<DeadLetter>>,
    cooldown: CooldownTable,
}

impl RuleEngine {
    pub fn new(handler: Arc<dyn ActionHandler>) -> Self {
        Self {
            handler,
            dead: Mutex::new(Vec::new()),
            cooldown: CooldownTable::new(),
        }
    }

    pub fn cooldown(&self) -> &CooldownTable {
        &self.cooldown
    }

    /// 触发规则（when 求值 + 冷却通过后串行执行全部动作）
    pub fn fire(&self, rule: &Rule, payload: &serde_json::Value, now_ms: i64) {
        if !rule.when_passes(payload) {
            return;
        }
        if !self.cooldown.try_fire(&rule.id, rule.cooldown_secs, now_ms) {
            return;
        }
        let mut dead = Vec::new();
        for action in &rule.then {
            run_action(self.handler.as_ref(), action, rule, now_ms, &mut dead);
        }
        if !dead.is_empty() {
            let mut queue = self.dead.lock().expect("死信锁污染");
            queue.extend(dead);
            let overflow = queue.len().saturating_sub(DEAD_LETTER_CAP);
            if overflow > 0 {
                queue.drain(0..overflow);
            }
        }
    }

    /// 死信列表（旧 → 新）
    pub fn dead_letters(&self) -> Vec<DeadLetter> {
        self.dead.lock().expect("死信锁污染").clone()
    }

    /// 重放死信（重试成功则移除；失败保留）
    pub fn replay(&self, dead_id: &str, rule: &Rule) -> std::result::Result<(), AutomationError> {
        let action = {
            let queue = self.dead.lock().expect("死信锁污染");
            queue
                .iter()
                .find(|d| d.id == dead_id)
                .map(|d| d.action.clone())
                .ok_or_else(|| AutomationError::DeadLetter(format!("死信 {dead_id} 不存在")))?
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let mut dead = Vec::new();
        // 先移除旧条目再执行（执行失败会以新 id 重新入队）
        {
            let mut queue = self.dead.lock().expect("死信锁污染");
            queue.retain(|d| d.id != dead_id);
        }
        run_action(self.handler.as_ref(), &action, rule, now_ms, &mut dead);
        if !dead.is_empty() {
            self.dead.lock().expect("死信锁污染").extend(dead);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::Expr;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// 假宿主：记录调用；对含 "fail" 的 URL 报错（fail_open_url=false 时成功）
    struct FakeHandler {
        calls: AtomicU32,
        publish_log: Mutex<Vec<String>>,
        fail_open_url: AtomicBool,
    }
    impl FakeHandler {
        fn new() -> Self {
            Self {
                calls: AtomicU32::new(0),
                publish_log: Mutex::new(vec![]),
                fail_open_url: AtomicBool::new(true),
            }
        }
    }
    impl ActionHandler for FakeHandler {
        fn open_url(&self, url: &str) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_open_url.load(Ordering::SeqCst) && url.contains("fail") {
                Err(AutomationError::Action(format!("open 失败: {url}")))
            } else {
                Ok(())
            }
        }
        fn publish(&self, topic: &str, payload: serde_json::Value) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.publish_log
                .lock()
                .unwrap()
                .push(format!("{topic}:{payload}"));
            Ok(())
        }
        fn ipc_command(&self, _module: &str, _cmd: &str, _args: &serde_json::Value) -> Result<()> {
            Ok(())
        }
        fn run_wasm(&self, _path: &str, _func: &str) -> Result<()> {
            Ok(())
        }
    }

    fn rule(id: &str, actions: Vec<Action>) -> Rule {
        Rule {
            id: id.into(),
            name: format!("规则{id}"),
            on: crate::rule::Trigger::Event { topic: "clipboard.captured".into() },
            when: Some(Expr::Leaf { path: "entry.kind".into(), cmp: crate::rule::CmpOp::Eq, value: json!("url") }),
            then: actions,
            cooldown_secs: 0,
            enabled: true,
        }
    }

    #[test]
    fn cooldown_gates_rapid_refires() {
        let table = CooldownTable::new();
        assert!(table.try_fire("r", 5, 10_000));
        assert!(!table.try_fire("r", 5, 12_000)); // 2s < 5s 冷却
        assert!(table.try_fire("r", 5, 16_000)); // 6s 后放行
        assert_eq!(table.suppressed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fire_runs_serially_and_success_swallows() {
        let h = Arc::new(FakeHandler::new());
        let engine = RuleEngine::new(h.clone());
        let r = rule(
            "r1",
            vec![
                Action::Notify { title: "标题".into(), body: "正文".into() },
                Action::OpenUrl { url: "https://ok".into() },
            ],
        );
        engine.fire(&r, &json!({ "entry": { "kind": "url" } }), 0);
        // 每个成功动作发动作调用 + rule_fired 审计 = 2×2
        assert_eq!(h.calls.load(Ordering::SeqCst), 4);
        assert!(engine.dead_letters().is_empty());
        // 冷却内第二次触发被拦截
        engine.fire(&r, &json!({ "entry": { "kind": "url" } }), 100);
        assert_eq!(h.calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn when_filter_blocks_execution() {
        let h = Arc::new(FakeHandler::new());
        let engine = RuleEngine::new(h.clone());
        let r = rule("r2", vec![Action::OpenUrl { url: "https://ok".into() }]);
        // when 不通过（kind != url）
        engine.fire(&r, &json!({ "entry": { "kind": "text" } }), 0);
        assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failing_action_retries_then_dead_letter() {
        let h = Arc::new(FakeHandler::new());
        let engine = RuleEngine::new(h.clone());
        let r = rule("r3", vec![Action::OpenUrl { url: "https://fail/x".into() }]);
        engine.fire(&r, &json!({ "entry": { "kind": "url" } }), 0);
        // 1 次原始 + 2 次重试 = 3
        assert_eq!(h.calls.load(Ordering::SeqCst), 3);
        let dead = engine.dead_letters();
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].rule_id, "r3");
        assert!(dead[0].error.contains("open 失败"));
    }

    #[test]
    fn replay_removes_on_success_keeps_on_failure() {
        let h = Arc::new(FakeHandler::new());
        let engine = RuleEngine::new(h.clone());
        let r = rule("r4", vec![Action::OpenUrl { url: "https://fail".into() }]);
        engine.fire(&r, &json!({ "entry": { "kind": "url" } }), 0);
        let dead_id = engine.dead_letters()[0].id.clone();
        // 重放仍失败 → 移除旧条目，失败动作重新入队（新 id）
        engine.replay(&dead_id, &r).unwrap();
        assert_eq!(engine.dead_letters().len(), 1);
        assert_ne!(engine.dead_letters()[0].id, dead_id);
        // 关闭失败开关 → 重放原动作成功 → 移除
        h.fail_open_url.store(false, Ordering::SeqCst);
        let new_id = engine.dead_letters()[0].id.clone();
        engine.replay(&new_id, &r).unwrap();
        assert!(engine.dead_letters().is_empty());
    }
}
