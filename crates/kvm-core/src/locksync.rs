//! 修饰键状态同步（docs/impl/09 §7.2 T-B7-9；蓝本 §3.7 Lan Mouse ④）。
//!
//! 拆"算"与"做"（B4 纪律）：本模块只做纯差值运算与同步驱动（算/调度），
//! LED 读写与读回校验在 win-integration 的 [`KeyboardLedPort`] 实现（做）。
//! 同步经 ControlTake/PairAccept 载荷顺带携带 locks 字段——零新帧型；
//! `Option` 未知态（旧对端不发）⇒ 不动作，诚实 no-op 而非按默认 false 对拍。

use host_core::ports::{KeyboardLedPort, LockKey, LockStates};

/// 差值：把 local 对齐到 remote 所需的逐键目标态列表（键恒 caps→num→scroll 序）。
/// 两态相等 ⇒ 空表零动作（对称性：diff 只看相等性，不看"谁切换"）。
pub fn lock_diff(local: &LockStates, remote: &LockStates) -> Vec<(LockKey, bool)> {
    let mut ops = Vec::new();
    if local.caps != remote.caps {
        ops.push((LockKey::Caps, remote.caps));
    }
    if local.num != remote.num {
        ops.push((LockKey::Num, remote.num));
    }
    if local.scroll != remote.scroll {
        ops.push((LockKey::Scroll, remote.scroll));
    }
    ops
}

/// 驱动：读本机灯态 → 差值 → 逐键施加（读回校验在端口内）→ 不符 warn 点名不谎报。
pub fn sync_toward(led: &dyn KeyboardLedPort, remote: &LockStates) {
    let local = led.read_lock_states();
    for (key, on) in lock_diff(&local, remote) {
        if let Err(e) = led.apply_lock_state(key, on) {
            tracing::warn!(key = ?key, target = on, error = %e, "修饰键读回校验不符（不谎报对齐）");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ls(caps: bool, num: bool, scroll: bool) -> LockStates {
        LockStates { caps, num, scroll }
    }

    #[test]
    #[allow(non_snake_case)]
    // 任务书（09 §7.2 T-B7-9）字面测试名优先于 rustc 命名惯例
    fn lockDiff_symmetric_empty() {
        // 正对照防空洞：先证"不同⇒非空"这一半，再证相等⇒空表
        let a = ls(true, false, true);
        let b = ls(false, true, true);
        assert!(
            !lock_diff(&a, &b).is_empty(),
            "正对照：差异态必须产出动作，否则本测的空表断言是空洞"
        );
        for same in [
            ls(true, true, true),
            ls(false, false, false),
            ls(true, false, true),
        ] {
            assert!(
                lock_diff(&same, &same).is_empty(),
                "目标态相等必须空表零动作: {same:?}"
            );
        }
    }

    #[test]
    #[allow(non_snake_case)]
    fn lockDiff_capsOnly_emitsOneToggle() {
        let ops = lock_diff(&ls(false, true, false), &ls(true, true, false));
        assert_eq!(ops, vec![(LockKey::Caps, true)]);
        // 反向：目标 off 同样一枚，目标值取 remote
        let ops = lock_diff(&ls(true, true, false), &ls(false, true, false));
        assert_eq!(ops, vec![(LockKey::Caps, false)]);
    }

    #[test]
    #[allow(non_snake_case)]
    fn lockDiff_threeWayAllCombination() {
        type DiffCase = (LockStates, LockStates, Vec<(LockKey, bool)>);
        // 参数表五臂：全反相 / 单键 / 双键 / 全同 / 全反相另一向
        let cases: &[DiffCase] = &[
            (
                ls(false, false, false),
                ls(true, true, true),
                vec![
                    (LockKey::Caps, true),
                    (LockKey::Num, true),
                    (LockKey::Scroll, true),
                ],
            ),
            (
                ls(true, true, true),
                ls(true, false, true),
                vec![(LockKey::Num, false)],
            ),
            (
                ls(false, false, true),
                ls(true, false, false),
                vec![(LockKey::Caps, true), (LockKey::Scroll, false)],
            ),
            (ls(true, false, true), ls(true, false, true), vec![]),
            (
                ls(true, true, false),
                ls(false, false, true),
                vec![
                    (LockKey::Caps, false),
                    (LockKey::Num, false),
                    (LockKey::Scroll, true),
                ],
            ),
        ];
        for (local, remote, want) in cases {
            assert_eq!(&lock_diff(local, remote), want, "臂 {local:?}→{remote:?}");
        }
    }
}
