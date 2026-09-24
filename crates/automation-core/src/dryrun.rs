//! T-B7-15 干跑规划器（09 §7.2）：`plan_rule` 为**纯函数**——不 publish、不 exec、
//! 不触达任何 `ActionHandler`，只把规则的每个动作翻译成可展示的行动计划。
//! 红线正证：`dryRun_emitsNothing_portZeroEffects`（FakeHost 三计数恒 0）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rule::{Action, Rule};

/// 干跑上下文：一个样本事件（用户提供的样例 payload）
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EventCtx {
    pub payload: Value,
}

/// 单动作的干跑计划（IPC DTO 直接复用）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionPlan {
    /// 动作 kind（与 Action 序列化 tag 同名：publish/notify/open_url/ipc_command/run_script）
    pub kind: String,
    /// 人读预览（含参数摘要与 when 判定注记）
    pub preview: String,
    /// 该动作若真跑是否会触达外部（open_url/ipc_command/run_script = 真执行风险；
    /// notify/publish = 仅展示；when 未通过时全部为 false）
    pub will_execute: bool,
}

/// 规划整条规则：逐动作产出计划，纯求值零副作用。
/// when 未通过不拦截产出——清单仍列全动作臂，仅在预览标注"when 未通过"。
pub fn plan_rule(rule: &Rule, ctx: &EventCtx) -> Vec<ActionPlan> {
    let when_passes = rule.when_passes(&ctx.payload);
    rule.then
        .iter()
        .map(|action| {
            let (kind, arg_preview, risky) = match action {
                Action::Publish { topic, payload } => (
                    "publish",
                    format!("publish → {topic} {}", trunc(&payload.to_string())),
                    false,
                ),
                Action::Notify { title, body } => (
                    "notify",
                    format!("notify 标题「{}」正文「{}」", trunc(title), trunc(body)),
                    false,
                ),
                Action::OpenUrl { url } => ("open_url", format!("open_url → {url}"), true),
                Action::IpcCommand { module, cmd, args } => (
                    "ipc_command",
                    format!(
                        "ipc {module}::{cmd} {}",
                        trunc(&serde_json::to_string(args).unwrap_or_default())
                    ),
                    true,
                ),
                Action::RunScript { path, func } => {
                    ("run_script", format!("run_script {path}::{func}"), true)
                }
            };
            let mut preview = arg_preview;
            preview.push_str(if when_passes {
                if risky {
                    " · 真执行风险"
                } else {
                    " · 仅展示"
                }
            } else {
                " · when 未通过（真触发不会执行）"
            });
            ActionPlan {
                kind: kind.to_string(),
                preview,
                will_execute: when_passes && risky,
            }
        })
        .collect()
}

fn trunc(s: &str) -> String {
    let cut: String = s.chars().take(80).collect();
    if cut.chars().count() < s.chars().count() {
        format!("{}…", cut)
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ActionHandler, RuleEngine};
    use crate::error::Result;
    use crate::history::History;
    use crate::rule::{CmpOp, Expr, Trigger};
    use parking_lot::Mutex;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// 假宿主：三端口计数（open_url / publish / ipc+run_wasm 合并前两个足以证零触达）
    struct FakeHost {
        open_calls: AtomicU32,
        publish_calls: AtomicU32,
        exec_calls: AtomicU32,
        log: Mutex<Vec<String>>,
    }
    impl FakeHost {
        fn new() -> Self {
            Self {
                open_calls: AtomicU32::new(0),
                publish_calls: AtomicU32::new(0),
                exec_calls: AtomicU32::new(0),
                log: Mutex::new(vec![]),
            }
        }
        fn total(&self) -> u32 {
            self.open_calls.load(Ordering::SeqCst)
                + self.publish_calls.load(Ordering::SeqCst)
                + self.exec_calls.load(Ordering::SeqCst)
        }
    }
    impl ActionHandler for FakeHost {
        fn open_url(&self, url: &str) -> Result<()> {
            self.open_calls.fetch_add(1, Ordering::SeqCst);
            self.log.lock().push(format!("open:{url}"));
            Ok(())
        }
        fn publish(&self, topic: &str, _payload: Value) -> Result<()> {
            self.publish_calls.fetch_add(1, Ordering::SeqCst);
            self.log.lock().push(format!("pub:{topic}"));
            Ok(())
        }
        fn ipc_command(&self, module: &str, cmd: &str, _args: &Value) -> Result<()> {
            self.exec_calls.fetch_add(1, Ordering::SeqCst);
            self.log.lock().push(format!("ipc:{module}:{cmd}"));
            Ok(())
        }
        fn run_wasm(&self, path: &str, func: &str) -> Result<()> {
            self.exec_calls.fetch_add(1, Ordering::SeqCst);
            self.log.lock().push(format!("wasm:{path}:{func}"));
            Ok(())
        }
    }

    fn tmp_dead(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("nf-dry-{tag}-{}", uuid::Uuid::now_v7()))
            .join("dead_letters.json")
    }

    fn engine(h: Arc<FakeHost>, tag: &str) -> RuleEngine {
        let dir = tmp_dead(tag);
        RuleEngine::new(h, Arc::new(History::open(dir.parent().unwrap())), dir)
    }

    fn full_rule(id: &str) -> Rule {
        Rule {
            id: id.into(),
            name: format!("规则{id}"),
            on: Trigger::Event {
                topic: "clipboard.captured".into(),
            },
            when: Some(Expr::Leaf {
                path: "entry.kind".into(),
                cmp: CmpOp::Eq,
                value: json!("url"),
            }),
            then: vec![
                Action::Publish {
                    topic: "x".into(),
                    payload: json!({ "a": 1 }),
                },
                Action::Notify {
                    title: "T".into(),
                    body: "B".into(),
                },
                Action::OpenUrl {
                    url: "https://e".into(),
                },
                Action::IpcCommand {
                    module: "m".into(),
                    cmd: "c".into(),
                    args: json!({}),
                },
                Action::RunScript {
                    path: "plugin:p".into(),
                    func: "f".into(),
                },
            ],
            cooldown_secs: 0,
            enabled: true,
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-15）字面测试名优先于 rustc 命名惯例
    fn dryRun_emitsNothing_portZeroEffects() {
        let h = Arc::new(FakeHost::new());
        let _engine = engine(h.clone(), "zero");
        // 即便引擎在侧、动作含五臂（publish/notify 是事件口，open_url/ipc/run_script 是执行口），
        // plan_rule 也不经引擎、不触 handler——三计数与死信口必须恒零。
        let plans = plan_rule(
            &full_rule("r1"),
            &EventCtx {
                payload: json!({ "entry": { "kind": "url" } }),
            },
        );
        assert_eq!(plans.len(), 5);
        assert_eq!(h.total(), 0, "红线正证：干跑零端口触达");
        assert!(h.log.lock().is_empty());
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-15）字面测试名优先于 rustc 命名惯例
    fn dryRun_listsAllActionKinds() {
        let plans = plan_rule(
            &full_rule("r2"),
            &EventCtx {
                payload: json!({ "entry": { "kind": "url" } }),
            },
        );
        let kinds: Vec<&str> = plans.iter().map(|p| p.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["publish", "notify", "open_url", "ipc_command", "run_script"],
            "五动作臂全覆盖含 ipc"
        );
        let flags: Vec<bool> = plans.iter().map(|p| p.will_execute).collect();
        assert_eq!(flags, vec![false, false, true, true, true]);
        assert!(plans[0].preview.contains("仅展示"));
        assert!(plans[3].preview.contains("真执行风险"));
        assert!(plans[4].preview.contains("plugin:p::f"));

        // when 未通过：清单仍全列，逐条注记且 will_execute 全灭
        let blocked = plan_rule(
            &full_rule("r2"),
            &EventCtx {
                payload: json!({ "entry": { "kind": "text" } }),
            },
        );
        assert_eq!(blocked.len(), 5);
        assert!(
            blocked.iter().all(|p| !p.will_execute),
            "when 未通过时不得有动作被标为将执行"
        );
        assert!(blocked.iter().all(|p| p.preview.contains("when 未通过")));
    }
}
