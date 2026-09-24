//! A3 动作执行器（docs/impl/07 A3）：单规则内串行、规则间并行；
//! Action 失败按 retry(2, 指数) 后进入死信（UI 可查/重放）。
//! 风暴防护（docs/impl/07 风险标注）：规则冷却表 + automation 自产事件不再触发规则（深度上限的 v1 等效实现）。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{AutomationError, Result};
use crate::history::{History, RunOutcome, RunRecord};
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

/// 死信条目（IPC DTO）。`#[serde(default)]`：T-B7-15 落盘后旧盘/缺字段
/// 兼容读入（内存旧态无盘 = 首载空，与 runs.json 同裁决）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeadLetter {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub rule_id: String,
    #[serde(default)]
    pub rule_name: String,
    pub action: Action,
    #[serde(default)]
    pub error: String,
    /// 最后尝试时刻毫秒
    #[serde(default)]
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
        let cooldown = if cooldown_secs == 0 {
            DEFAULT_COOLDOWN_SECS
        } else {
            cooldown_secs
        };
        let mut table = self.last_fired.lock();
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

/// 执行单个动作（含 retry 指数退避；全部失败 → DeadLetter）。
/// 返回 `Some(错误文本)` = 该动作最终失败（已入死信草稿），`None` = 成功。
fn run_action(
    handler: &dyn ActionHandler,
    action: &Action,
    rule: &Rule,
    now_ms: i64,
    dead: &mut Vec<DeadLetter>,
) -> Option<String> {
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
                return None;
            }
            Err(e) if attempt < ACTION_RETRIES => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(500u64 << (attempt - 1)));
                tracing::warn!(rule = %rule.id, attempt, error = %e, "动作重试");
            }
            Err(e) => {
                let msg = e.to_string();
                dead.push(DeadLetter {
                    id: uuid::Uuid::now_v7().to_string(),
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    action: action.clone(),
                    error: msg.clone(),
                    at_ms: now_ms,
                });
                return Some(msg);
            }
        }
    }
}

/// 首载死信盘（不存在 = 空；坏盘 = warn + 改名 `.corrupt` 留证 + 空载，与 runs.json 同谱）
fn load_dead(path: &std::path::Path) -> Vec<DeadLetter> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_slice::<Vec<DeadLetter>>(&bytes) {
        Ok(v) => v,
        Err(e) => {
            let corrupt = path.with_extension("json.corrupt");
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "dead_letters.json 不可解析——改名留证后以空队列启动（死信=可观测补救队列，非信任面）"
            );
            let _ = std::fs::rename(path, &corrupt);
            Vec::new()
        }
    }
}

/// 整表原子落盘（tmp + sync_all + rename，沿 B6 profiles 形制）
fn write_dead(path: &std::path::Path, queue: &[DeadLetter]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(AutomationError::Io)?;
    }
    let bytes = serde_json::to_vec_pretty(queue)
        .map_err(|e| AutomationError::BadRule(format!("死信序列化失败: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    use std::io::Write;
    let mut f = std::fs::File::create(&tmp).map_err(AutomationError::Io)?;
    f.write_all(&bytes).map_err(AutomationError::Io)?;
    f.sync_all().map_err(AutomationError::Io)?;
    std::fs::rename(&tmp, path).map_err(AutomationError::Io)
}

/// 规则引擎：执行单条规则的全部动作（规则内串行），死信收集 + 执行历史落环。
/// T-B7-15：死信落 `dead_letters.json`（重启不丢），写形制沿 runs.json
/// （tmp + sync_all + rename 原子替换；坏盘 warn+改名留证+空载）。
pub struct RuleEngine {
    handler: Arc<dyn ActionHandler>,
    dead: Mutex<Vec<DeadLetter>>,
    cooldown: CooldownTable,
    /// T-B7-14：每轮 fire（过 when + 过冷却后）记一条 RunRecord
    history: Arc<History>,
    dead_path: PathBuf,
}

impl RuleEngine {
    pub fn new(handler: Arc<dyn ActionHandler>, history: Arc<History>, dead_path: PathBuf) -> Self {
        Self {
            handler,
            dead: Mutex::new(load_dead(&dead_path)),
            cooldown: CooldownTable::new(),
            history,
            dead_path,
        }
    }

    pub fn cooldown(&self) -> &CooldownTable {
        &self.cooldown
    }

    /// 执行历史（旧 → 新，limit 有界）
    pub fn runs(&self, limit: Option<usize>) -> Vec<RunRecord> {
        self.history.runs(limit)
    }

    /// 触发规则（when 求值 + 冷却通过后串行执行全部动作），死信收集 + 历史落环
    pub fn fire(&self, rule: &Rule, payload: &serde_json::Value, now_ms: i64) {
        if !rule.when_passes(payload) {
            return;
        }
        if !self.cooldown.try_fire(&rule.id, rule.cooldown_secs, now_ms) {
            return;
        }
        let started = std::time::Instant::now();
        let mut dead = Vec::new();
        let mut failed: Vec<u32> = Vec::new();
        let mut first_error: Option<String> = None;
        for (i, action) in rule.then.iter().enumerate() {
            if let Some(err) = run_action(self.handler.as_ref(), action, rule, now_ms, &mut dead) {
                failed.push(i as u32);
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
        let total = rule.then.len() as u32;
        let outcome = if failed.is_empty() {
            RunOutcome::Success
        } else if failed.len() as u32 == total {
            RunOutcome::Failure
        } else {
            RunOutcome::Partial {
                failed_indices: failed,
            }
        };
        self.history.push(RunRecord {
            rule_id: rule.id.clone(),
            fired_ms: now_ms,
            outcome,
            actions_executed: total,
            duration_ms: started.elapsed().as_millis() as u64,
            error: first_error,
        });
        if !dead.is_empty() {
            {
                let mut queue = self.dead.lock();
                queue.extend(dead);
                let overflow = queue.len().saturating_sub(DEAD_LETTER_CAP);
                if overflow > 0 {
                    queue.drain(0..overflow);
                }
            }
            self.persist_dead();
        }
    }

    /// 死信列表（旧 → 新）
    pub fn dead_letters(&self) -> Vec<DeadLetter> {
        self.dead.lock().clone()
    }

    /// 重放死信（重试成功则移除；失败保留）
    pub fn replay(&self, dead_id: &str, rule: &Rule) -> std::result::Result<(), AutomationError> {
        let action = {
            let queue = self.dead.lock();
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
            let mut queue = self.dead.lock();
            queue.retain(|d| d.id != dead_id);
        }
        // 重放不另记历史：历史环记的是规则触发轮次，死信重放是人工补救动作
        let _ = run_action(self.handler.as_ref(), &action, rule, now_ms, &mut dead);
        if !dead.is_empty() {
            self.dead.lock().extend(dead);
        }
        self.persist_dead();
        Ok(())
    }

    /// 内存队列快照落盘（落盘失败只 warn，不影响内存态）
    fn persist_dead(&self) {
        let snapshot = self.dead.lock().clone();
        if let Err(e) = write_dead(&self.dead_path, &snapshot) {
            tracing::warn!(path = %self.dead_path.display(), error = %e, "死信落盘失败（内存队列不受影响）");
        }
    }

    /// 一钮全清死信（T-B7-15）：内存 + 盘同清空
    pub fn clear_dead(&self) {
        self.dead.lock().clear();
        self.persist_dead();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::Expr;
    use serde_json::json;
    use std::path::Path;
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
            self.publish_log.lock().push(format!("{topic}:{payload}"));
            Ok(())
        }
        fn ipc_command(&self, _module: &str, _cmd: &str, _args: &serde_json::Value) -> Result<()> {
            Ok(())
        }
        fn run_wasm(&self, _path: &str, _func: &str) -> Result<()> {
            Ok(())
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("nf-eng-{tag}-{}", uuid::Uuid::now_v7()))
    }

    fn engine_in(h: Arc<FakeHandler>, dir: &Path) -> RuleEngine {
        RuleEngine::new(
            h,
            Arc::new(History::open(dir)),
            dir.join("dead_letters.json"),
        )
    }

    fn engine(h: Arc<FakeHandler>, tag: &str) -> RuleEngine {
        engine_in(h, &tmp(tag))
    }

    fn rule(id: &str, actions: Vec<Action>) -> Rule {
        Rule {
            id: id.into(),
            name: format!("规则{id}"),
            on: crate::rule::Trigger::Event {
                topic: "clipboard.captured".into(),
            },
            when: Some(Expr::Leaf {
                path: "entry.kind".into(),
                cmp: crate::rule::CmpOp::Eq,
                value: json!("url"),
            }),
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
        let engine = engine(h.clone(), "fire-serial");
        let r = rule(
            "r1",
            vec![
                Action::Notify {
                    title: "标题".into(),
                    body: "正文".into(),
                },
                Action::OpenUrl {
                    url: "https://ok".into(),
                },
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
        let engine = engine(h.clone(), "when-block");
        let r = rule(
            "r2",
            vec![Action::OpenUrl {
                url: "https://ok".into(),
            }],
        );
        // when 不通过（kind != url）
        engine.fire(&r, &json!({ "entry": { "kind": "text" } }), 0);
        assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failing_action_retries_then_dead_letter() {
        let h = Arc::new(FakeHandler::new());
        let engine = engine(h.clone(), "retry-dead");
        let r = rule(
            "r3",
            vec![Action::OpenUrl {
                url: "https://fail/x".into(),
            }],
        );
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
        let engine = engine(h.clone(), "replay");
        let r = rule(
            "r4",
            vec![Action::OpenUrl {
                url: "https://fail".into(),
            }],
        );
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

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-14）字面测试名优先于 rustc 命名惯例
    fn history_partialOutcome_recordsFailedIndices() {
        let h = Arc::new(FakeHandler::new());
        let engine = engine(h.clone(), "partial");
        let r = rule(
            "r5",
            vec![
                Action::OpenUrl {
                    url: "https://ok/1".into(),
                },
                Action::OpenUrl {
                    url: "https://fail/x".into(),
                },
                Action::Notify {
                    title: "还活着".into(),
                    body: "".into(),
                },
            ],
        );
        engine.fire(&r, &json!({ "entry": { "kind": "url" } }), 7000);
        let runs = engine.runs(None);
        assert_eq!(runs.len(), 1, "一轮 fire 恰记一条");
        let rec = &runs[0];
        assert_eq!(rec.rule_id, "r5");
        assert_eq!(rec.fired_ms, 7000);
        assert_eq!(
            rec.actions_executed, 3,
            "串行全试=then 长度（部分失败不中断后续）"
        );
        match &rec.outcome {
            RunOutcome::Partial { failed_indices } => {
                assert_eq!(failed_indices, &[1], "部分成功必须点名失败位序");
            }
            other => panic!("期望 Partial，实得 {other:?}"),
        }
        assert!(rec
            .error
            .as_deref()
            .is_some_and(|e| e.contains("open 失败")));
        // 全成臂收口：Success 无 error
        let ok_rule = rule(
            "r6",
            vec![Action::OpenUrl {
                url: "https://ok/2".into(),
            }],
        );
        engine.fire(&ok_rule, &json!({ "entry": { "kind": "url" } }), 8000);
        let ok_rec = &engine.runs(Some(1))[0];
        assert_eq!(ok_rec.outcome, RunOutcome::Success);
        assert_eq!(ok_rec.error, None);
        // r5 冷却窗（默认 5s=5000ms）后再来一轮：仍失败 → 仍 Partial 同位点名
        engine.fire(&r, &json!({ "entry": { "kind": "url" } }), 13_000);
        let last = &engine.runs(Some(1))[0];
        assert_eq!(last.rule_id, "r5", "13s 已过冷却，本轮必须落环");
        assert!(
            matches!(&last.outcome, RunOutcome::Partial { failed_indices } if failed_indices == &[1])
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-15）字面测试名优先于 rustc 命名惯例
    fn deadClear_emptiesDiskAndMemory() {
        let h = Arc::new(FakeHandler::new());
        let dir = tmp("dead-clear");
        let r = rule(
            "r7",
            vec![Action::OpenUrl {
                url: "https://fail".into(),
            }],
        );
        let engine = engine_in(h.clone(), &dir);
        engine.fire(&r, &json!({ "entry": { "kind": "url" } }), 0);
        assert_eq!(engine.dead_letters().len(), 1);
        // 重启不丢（落盘正证）：同路径重开引擎读回同一死信
        let reopened = engine_in(h.clone(), &dir);
        assert_eq!(reopened.dead_letters().len(), 1);
        assert_eq!(reopened.dead_letters()[0].rule_id, "r7");
        drop(reopened);
        // 一钮全清：内存 + 盘同空
        engine.clear_dead();
        assert!(engine.dead_letters().is_empty(), "清后内存必须空");
        let after_restart = engine_in(h, &dir);
        assert!(
            after_restart.dead_letters().is_empty(),
            "清后盘态必须空——抹证据一钮是真清而非内存遮眼"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
