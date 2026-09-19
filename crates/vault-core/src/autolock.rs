//! V5 自动锁定策略评估（D-24）：纯函数、时钟外部注入，便于虚拟时间回归测试。
//!
//! 两条独立触发线（docs/impl/05 V5）：空闲 `idle_secs` / 失焦 `blur_secs`，
//! 0 = 禁用该线；任一命中即锁定；锁定前 30s 先发一次预警。

/// 锁定前预警窗口（规格：30s 气泡预警）
pub const WARN_WINDOW_SECS: u64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutolockPolicy {
    /// 无操作多少秒后锁定（0 = 禁用空闲线）
    pub idle_secs: u64,
    /// 窗口失焦多少秒后锁定（0 = 禁用失焦线）
    pub blur_secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutolockAction {
    Lock,
    /// 距锁定还有 lock_in_secs 秒（本轮已发过预警则调用方负责去重）
    Warn {
        lock_in_secs: u64,
    },
}

/// now/last_touch/blur_since 为同一时基的秒级时间戳；blur_since = None 表示聚焦中。
pub fn evaluate(
    now: u64,
    last_touch: u64,
    blur_since: Option<u64>,
    policy: &AutolockPolicy,
) -> Option<AutolockAction> {
    let mut warn: Option<u64> = None;
    let mut check = |elapsed: u64, limit: u64| -> Option<AutolockAction> {
        if limit == 0 {
            return None;
        }
        if elapsed >= limit {
            return Some(AutolockAction::Lock);
        }
        if limit > WARN_WINDOW_SECS && elapsed >= limit - WARN_WINDOW_SECS {
            warn = Some(warn.map_or(limit - elapsed, |w| w.min(limit - elapsed)));
        }
        None
    };
    if let Some(a) = check(now.saturating_sub(last_touch), policy.idle_secs) {
        return Some(a);
    }
    if let Some(b) = blur_since {
        if let Some(a) = check(now.saturating_sub(b), policy.blur_secs) {
            return Some(a);
        }
    }
    warn.map(|remaining| AutolockAction::Warn {
        lock_in_secs: remaining,
    })
}

/// D-24 决策④：定时清除到期时，仅当剪贴板文本仍是刚复制的那条密码才允许清除。
/// `None`（读不到/无法判定）或被替换一律不动（D-05 保守纪律）。
pub fn clipboard_clear_due(current: Option<&str>, secret: &str) -> bool {
    current == Some(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: AutolockPolicy = AutolockPolicy {
        idle_secs: 900,
        blur_secs: 300,
    };

    #[test]
    fn idle_threshold_triggers_lock() {
        assert_eq!(
            evaluate(1000, 100, None, &P),
            Some(AutolockAction::Lock),
            "空闲 900s ≥ idle_secs 必须锁定"
        );
    }

    #[test]
    fn idle_warn_window_before_lock() {
        assert_eq!(
            evaluate(890, 10, None, &P),
            Some(AutolockAction::Warn { lock_in_secs: 20 }),
            "空闲 880s 进入 30s 预警窗"
        );
        assert_eq!(evaluate(700, 10, None, &P), None, "窗口外不预警");
    }

    #[test]
    fn zero_policy_never_fires() {
        let off = AutolockPolicy {
            idle_secs: 0,
            blur_secs: 0,
        };
        assert_eq!(evaluate(u64::MAX, 0, Some(0), &off), None);
    }

    #[test]
    fn blur_line_independent_of_idle() {
        // 刚 touch 过（空闲线远未触发），但失焦已超 blur_secs → 锁定
        assert_eq!(
            evaluate(500, 499, Some(100), &P),
            Some(AutolockAction::Lock)
        );
        // 失焦 280s → 预警剩余 20s
        assert_eq!(
            evaluate(380, 380, Some(100), &P),
            Some(AutolockAction::Warn { lock_in_secs: 20 })
        );
    }

    #[test]
    fn earliest_warning_wins_and_clock_skew_safe() {
        // 双线同时进窗：idle 剩 20s、blur 剩 5s → 取更紧迫的一条
        let act = evaluate(890, 10, Some(595), &P);
        assert_eq!(act, Some(AutolockAction::Warn { lock_in_secs: 5 }));
        // now < last_touch（时基回退）不得下溢 panic，也不误锁
        assert_eq!(evaluate(5, 100, None, &P), None);
    }

    #[test]
    fn clipboard_cleared_only_when_still_the_same_secret() {
        // D-24 验收⑤负例：被替换/无法判定都不得动剪贴板
        assert!(
            clipboard_clear_due(Some("hunter2"), "hunter2"),
            "未变 → 清除"
        );
        assert!(!clipboard_clear_due(Some("别的用户复制的内容"), "hunter2"));
        assert!(
            !clipboard_clear_due(None, "hunter2"),
            "read_text 无法判定（None）必须不清"
        );
        assert!(!clipboard_clear_due(Some(""), "hunter2"), "空文本 ≠ 密钥");
    }
}
