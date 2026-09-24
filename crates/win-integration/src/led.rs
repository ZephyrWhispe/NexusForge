//! 锁键灯态读写（docs/impl/09 §7.2 T-B7-9；[`KeyboardLedPort`] 的 Windows 实现）。
//!
//! - 读：GetKeyboardState 键盘状态表，toggle 键取 bit0（灯位；VK_CAPITAL/NUMLOCK/SCROLL）。
//! - 做：keybd_event VK_ 一次（down+up 模拟轻按）——**读回校验**：keybd_event 异步
//!   入输入队列，短重试轮询读位直至达标或超限；超限返回 Err 点名（调用方 warn 上报，
//!   绝不谎报已对齐）。差值判定（lock_diff）在 kvm-core 侧，本文件只提供"做"的原语。

use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, GetKeyboardState, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VK_CAPITAL, VK_NUMLOCK,
    VK_SCROLL,
};

use host_core::ports::{KeyboardLedPort, LockKey, LockStates};

/// 读回轮询：最多 5 次 × 20ms（超限即不符；总阻塞 ≤100ms，调用方在 spawn_blocking）
const READBACK_RETRIES: usize = 5;
const READBACK_GAP: Duration = Duration::from_millis(20);

fn vk_of(key: LockKey) -> u8 {
    // VIRTUAL_KEY.0 为 u16，锁键三值（0x14/0x90/0x91）恒 <256，窄化安全
    match key {
        LockKey::Caps => VK_CAPITAL.0 as u8,
        LockKey::Num => VK_NUMLOCK.0 as u8,
        LockKey::Scroll => VK_SCROLL.0 as u8,
    }
}

fn led_bit(state: &[u8; 256], key: LockKey) -> bool {
    state[vk_of(key) as usize] & 0x01 != 0
}

fn read_state_table() -> [u8; 256] {
    let mut state = [0u8; 256];
    // API 失败（无桌面交互会话）⇒ 全零表：读回位恒 false，apply 侧由读回校验
    // 兜底如实报错，比 panic/谎报诚实
    unsafe { GetKeyboardState(&mut state).ok() };
    state
}

pub struct KeyboardLedWin;

/// 施加 + 读回校验的 seam 形制：read_bit/toggle 可注入（任务书
/// `applyLockState_readBackMismatch_warnsNotSilent` 即测此函数假 readback 臂）
fn apply_with_readback(
    key: LockKey,
    on: bool,
    mut read_bit: impl FnMut() -> bool,
    mut toggle: impl FnMut(),
) -> Result<(), String> {
    if read_bit() == on {
        return Ok(());
    }
    toggle();
    for _ in 0..READBACK_RETRIES {
        if read_bit() == on {
            return Ok(());
        }
        std::thread::sleep(READBACK_GAP);
    }
    Err(format!(
        "锁键 {key:?} 读回校验不符：目标态 {on}，keybd_event 施加后灯位仍为 {}",
        read_bit()
    ))
}

impl KeyboardLedPort for KeyboardLedWin {
    fn read_lock_states(&self) -> LockStates {
        let state = read_state_table();
        LockStates {
            caps: led_bit(&state, LockKey::Caps),
            num: led_bit(&state, LockKey::Num),
            scroll: led_bit(&state, LockKey::Scroll),
        }
    }

    fn apply_lock_state(&self, key: LockKey, on: bool) -> Result<(), String> {
        apply_with_readback(
            key,
            on,
            || led_bit(&read_state_table(), key),
            || unsafe {
                let vk = vk_of(key);
                keybd_event(vk, 0, KEYBD_EVENT_FLAGS(0), 0);
                keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(non_snake_case)]
    // 任务书（09 §7.2 T-B7-9）字面测试名优先于 rustc 命名惯例
    fn applyLockState_readBackMismatch_warnsNotSilent() {
        // seam：假 readback 恒反位——施加后必得 Err 点名（不谎报成功），toggle 恰一次
        let mut toggles = 0usize;
        let r = apply_with_readback(LockKey::Caps, true, || false, || toggles += 1);
        let e = match r {
            Err(e) => e,
            Ok(()) => panic!("读回恒不符却报成功 = 谎报"),
        };
        assert_eq!(toggles, 1, "keybd_event VK_ 一次：超限不重按");
        assert!(e.contains("Caps"), "必须点名锁键: {e}");
        assert!(e.contains("目标态 true"), "必须点名目标态: {e}");
    }

    #[test]
    #[allow(non_snake_case)]
    fn applyLockState_alreadyTarget_noToggle() {
        // 正对照防空洞：达标臂必须 Ok 且零按键（幂等防反相双拍）
        let mut toggles = 0usize;
        apply_with_readback(LockKey::Num, false, || false, || toggles += 1)
            .expect("已达目标态应直接 Ok");
        assert_eq!(toggles, 0);
    }

    #[test]
    #[allow(non_snake_case)]
    fn applyLockState_readbackFlipsAfterToggle_okOnce() {
        // toggle 后翻位 ⇒ Ok，且 toggle 恰一次
        let lit = std::cell::Cell::new(false);
        let mut toggles = 0usize;
        apply_with_readback(
            LockKey::Scroll,
            true,
            || lit.get(),
            || {
                lit.set(true);
                toggles += 1;
            },
        )
        .expect("读回达标应成功");
        assert_eq!(toggles, 1);
    }
}
