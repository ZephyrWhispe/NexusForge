//! A1 规则模型 + A2 受限表达式求值器（docs/impl/07 A1–A2）。
//!
//! Expr 为受限表达式（点路径取值 + 比较/逻辑，**禁止任意代码**）；
//! 触发源 v1：Event / Startup / Schedule（每日 HH:MM 简化）；Hotkey 触发随全局快捷键 ability 后续接入。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{AutomationError, Result};

/// 触发源（docs/impl/07 A1）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    /// 事件主题触发（可附加 when 表达式过滤 payload）
    Event { topic: String },
    /// 开机/应用启动触发
    Startup,
    /// 每日定时（HH:MM，24h 制；完整 cron 随 Task Scheduler 集成 A4 深化）
    Schedule { time: String },
}

/// 比较操作
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Lt,
    Contains,
}

/// 受限表达式（A2）：叶子 = 路径取值 + 比较；组合 = and/or/not
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", content = "args", rename_all = "snake_case")]
pub enum Expr {
    /// payload 内点路径取值后与字面量比较
    Leaf { path: String, cmp: CmpOp, value: Value },
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
}

impl Expr {
    /// 求值：value = 事件 payload JSON
    pub fn eval(&self, payload: &Value) -> bool {
        match self {
            Expr::Leaf { path, cmp, value } => {
                let actual = get_path(payload, path);
                compare(actual, *cmp, value)
            }
            Expr::And(items) => items.iter().all(|e| e.eval(payload)),
            Expr::Or(items) => items.iter().any(|e| e.eval(payload)),
            Expr::Not(inner) => !inner.eval(payload),
        }
    }
}

/// 点路径取值（a.b.0.c 数组下标用数字段）；缺失返回 Null
fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for seg in path.split('.') {
        match cur {
            Value::Object(map) => match map.get(seg) {
                Some(next) => cur = next,
                None => return None,
            },
            Value::Array(items) => {
                let idx: usize = seg.parse().ok()?;
                cur = items.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(cur)
}

fn compare<'a>(actual: Option<&'a Value>, cmp: CmpOp, expect: &Value) -> bool {
    let Some(a) = actual else {
        return cmp == CmpOp::Ne;
    };
    match cmp {
        CmpOp::Eq => json_eq(a, expect),
        CmpOp::Ne => !json_eq(a, expect),
        CmpOp::Gt | CmpOp::Lt => {
            // 数值或字符串比较
            let ord = match (a, expect) {
                (Value::Number(x), Value::Number(y)) => {
                    (x.as_f64()).partial_cmp(&y.as_f64())
                }
                (Value::String(x), Value::String(y)) => Some(x.as_str().cmp(y.as_str())),
                _ => None,
            };
            match ord {
                Some(std::cmp::Ordering::Greater) => cmp == CmpOp::Gt,
                Some(std::cmp::Ordering::Less) => cmp == CmpOp::Lt,
                _ => false,
            }
        }
        CmpOp::Contains => match (a, expect) {
            (Value::String(h), Value::String(n)) => h.contains(n.as_str()),
            (Value::Array(items), _) => items.iter().any(|i| json_eq(i, expect)),
            _ => false,
        },
    }
}

fn json_eq(a: &Value, b: &Value) -> bool {
    // 数值跨 int/float 宽松比较
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

/// 动作（docs/impl/07 A3；RunScript 随 WASM A5 接入）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// 转发事件（可修饰 payload；防自环：source=automation 的事件不再触发规则）
    Publish { topic: String, payload: Value },
    /// 前端通知（toast）
    Notify { title: String, body: String },
    /// 打开 URL/路径
    OpenUrl { url: String },
    /// 调用模块 IPC 语义（由宿主注入的 ActionHandler 执行；v1 宿主实现映射内置动作）
    IpcCommand { module: String, cmd: String, args: Value },
    /// 执行 WASM 插件导出函数（A5 沙箱：fuel + 内存限额；path 支持 "plugin:{id}" 或 wasm 文件路径）
    RunScript { path: String, func: String },
}

/// 单条自动化规则（docs/impl/07 A1）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub on: Trigger,
    /// 条件（None = 触发即执行）
    #[serde(default)]
    pub when: Option<Expr>,
    pub then: Vec<Action>,
    /// 触发冷却（秒；防规则风暴，0 = 用全局默认）
    #[serde(default)]
    pub cooldown_secs: u64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Rule {
    /// 校验规则（topic 非空 / 动作非空 / Schedule 时间格式）
    pub fn validate(&self) -> Result<()> {
        match &self.on {
            Trigger::Event { topic } if topic.trim().is_empty() => {
                return Err(AutomationError::BadRule("事件主题为空".into()))
            }
            Trigger::Schedule { time } => {
                let parts: Vec<&str> = time.split(':').collect();
                let ok = parts.len() == 2
                    && parts[0].len() == 2
                    && parts[1].len() == 2
                    && parts[0].parse::<u8>().map(|h| h < 24).unwrap_or(false)
                    && parts[1].parse::<u8>().map(|m| m < 60).unwrap_or(false);
                if !ok {
                    return Err(AutomationError::BadRule(format!(
                        "Schedule 时间格式应为 HH:MM: {time}"
                    )));
                }
            }
            _ => {}
        }
        if self.then.is_empty() {
            return Err(AutomationError::BadRule("动作列表为空".into()));
        }
        if self.name.trim().is_empty() {
            return Err(AutomationError::BadRule("规则名为空".into()));
        }
        Ok(())
    }

    /// 该规则是否由事件触发且主题匹配
    pub fn matches_event(&self, topic: &str) -> bool {
        matches!(&self.on, Trigger::Event { topic: t } if t == topic)
    }

    /// 条件求值
    pub fn when_passes(&self, payload: &Value) -> bool {
        self.when.as_ref().map(|e| e.eval(payload)).unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn path_getter_and_leaf_compare() {
        let payload = json!({ "entry": { "kind": "url", "len": 42 }, "tags": ["a", "b"] });
        let e = Expr::Leaf { path: "entry.kind".into(), cmp: CmpOp::Eq, value: json!("url") };
        assert!(e.eval(&payload));
        let e = Expr::Leaf { path: "entry.len".into(), cmp: CmpOp::Gt, value: json!(40) };
        assert!(e.eval(&payload));
        let e = Expr::Leaf { path: "tags.1".into(), cmp: CmpOp::Eq, value: json!("b") };
        assert!(e.eval(&payload));
        let e = Expr::Leaf { path: "missing.x".into(), cmp: CmpOp::Eq, value: json!("x") };
        assert!(!e.eval(&payload));
        // 缺失路径 Ne = true
        let e = Expr::Leaf { path: "missing.x".into(), cmp: CmpOp::Ne, value: json!("x") };
        assert!(e.eval(&payload));
    }

    #[test]
    fn logic_combinators_and_contains() {
        let payload = json!({ "content": "hello rust", "n": 3 });
        let e = Expr::And(vec![
            Expr::Leaf { path: "content".into(), cmp: CmpOp::Contains, value: json!("rust") },
            Expr::Not(Box::new(Expr::Leaf { path: "n".into(), cmp: CmpOp::Gt, value: json!(5) })),
        ]);
        assert!(e.eval(&payload));
        let e = Expr::Or(vec![
            Expr::Leaf { path: "n".into(), cmp: CmpOp::Lt, value: json!(1) },
            Expr::Leaf { path: "content".into(), cmp: CmpOp::Contains, value: json!("nope") },
        ]);
        assert!(!e.eval(&payload));
    }

    #[test]
    fn rule_validate_and_match() {
        let r = Rule {
            id: "r1".into(),
            name: "示例".into(),
            on: Trigger::Event { topic: "clipboard.captured".into() },
            when: Some(Expr::Leaf { path: "entry.kind".into(), cmp: CmpOp::Eq, value: json!("url") }),
            then: vec![Action::Notify { title: "t".into(), body: "b".into() }],
            cooldown_secs: 0,
            enabled: true,
        };
        r.validate().unwrap();
        assert!(r.matches_event("clipboard.captured"));
        assert!(!r.matches_event("ocr.completed"));
        assert!(r.when_passes(&json!({ "entry": { "kind": "url" } })));
        assert!(!r.when_passes(&json!({ "entry": { "kind": "text" } })));

        let bad = Rule { on: Trigger::Schedule { time: "25:00".into() }, ..r };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn schedule_format_checked() {
        let base = Rule {
            id: "s".into(),
            name: "定时".into(),
            on: Trigger::Schedule { time: "08:30".into() },
            when: None,
            then: vec![Action::OpenUrl { url: "https://x".into() }],
            cooldown_secs: 0,
            enabled: true,
        };
        assert!(base.validate().is_ok());
        let bad_time = Rule { on: Trigger::Schedule { time: " 8:30".into() }, ..base.clone() };
        assert!(bad_time.validate().is_err());
        let bad_time2 = Rule { on: Trigger::Schedule { time: "25:00".into() }, ..base.clone() };
        assert!(bad_time2.validate().is_err());
        let bad_len = Rule { on: Trigger::Schedule { time: "8:5".into() }, ..base.clone() };
        assert!(bad_len.validate().is_err());
        let empty_actions = Rule { then: vec![], ..base };
        assert!(empty_actions.validate().is_err());
    }
}
