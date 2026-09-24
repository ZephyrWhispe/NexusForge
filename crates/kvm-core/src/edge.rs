//! K7 屏幕边缘切换（docs/impl/05 K7）。
//!
//! 纯逻辑状态机（无 IO，钩子回调内可安全调用，<5ms 约束）：
//! - 服务端（控制方）维护 [设备→共享边] 映射；鼠标抵达映射边缘（容差 2px、
//!   100ms 冷却、**离开边缘带后重新武装**防边界抖动乒乓）→ 切换控制权：
//!   发 ControlTake，随后本地输入事件全部转发目标设备并抑制本地（K4 接管）。
//! - 切回条件优先级：**边缘回移 > 快捷键 Ctrl+Alt+Shift+Q**。
//!   - 边缘回移：控制方不知道对端注入后的光标位置，但转发的归一化坐标
//!     （0..65535）可换算回对端像素（px = abs × extent / 65536，与 K5 注入
//!     公式互逆）；抵达对端回移边（本端 Right ↔ 对端 Left）即释放。
//!   - 快捷键：钩子键盘事件组合判定，命中即释放并吞掉该键。
//! - 受控端（被动方）：收到 ControlTake 置位，InputEvent 帧 → K5 注入；
//!   收到 ControlRelease 复位。注入事件带 LLMHF_INJECTED，本端钩子
//!   防回环过滤不回调（K4），不会误触本端边缘逻辑。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use host_core::ports::{RawInput, ScreenRect};

/// 共享边（本端视角；T-B7-7 起四值，边角同帧命中由 [`corner_edge`] 对角线裁决）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Left,
    Right,
    Up,
    Down,
}

impl Edge {
    pub fn as_str(self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Right => "right",
            Edge::Up => "up",
            Edge::Down => "down",
        }
    }
}

/// 边角归属唯一裁决（T-B7-7）：`(x, y)` 为屏内坐标（0..w × 0..h），鼠标同帧
/// 落入垂直缘与水平缘两条容差带时，按过该角的 45° 对角线分割归属——到垂直缘
/// 的水平距离 **严格小于** 到水平缘的垂直距离 → Left/Right，否则 Up/Down；
/// 恰在对角线上（距离相等）→ 水平缘取胜（Left/Right）。裁决纯函数、无状态，
/// 两缘同帧触发不可能。
pub fn corner_edge(x: i32, y: i32, w: i32, h: i32) -> Edge {
    let dx = x.min(w - 1 - x);
    let dy = y.min(h - 1 - y);
    if dx <= dy {
        if x <= w - 1 - x {
            Edge::Left
        } else {
            Edge::Right
        }
    } else if y <= h - 1 - y {
        Edge::Up
    } else {
        Edge::Down
    }
}

/// ControlTake 载荷（JSON）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlTakePayload {
    /// 发起控制方 device_id
    pub by: String,
    /// 控制方切换所用的本端边（对端的回移边为其对面）
    pub edge: Edge,
}

/// ControlRelease 载荷（JSON）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlReleasePayload {
    pub by: String,
    /// "edge" | "hotkey" | "manual" | "session"
    pub reason: String,
}

/// 本地输入决策（钩子回调内产生）
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// 本地放行
    Passthrough,
    /// 切换控制权到该设备（本事件仍本地放行）
    SwitchTo(String),
    /// 转发到受控设备并抑制本地
    Forward,
    /// 吞掉（不转发不本地；用于释放组合键的收尾键）
    Suppress,
}

#[derive(Clone, Debug)]
enum ControlState {
    Idle,
    Controlling { device_id: String, edge: Edge },
}

pub struct EdgeSwitchConfig {
    /// 边缘判定容差（docs/impl/05 K7：2px）
    pub tolerance_px: i32,
    /// 两次切换最小间隔（docs/impl/05 K7：100ms）
    pub cooldown: Duration,
    /// 释放组合键 vk（默认 Ctrl+Alt+Shift+Q）
    pub release_combo: [u16; 4],
}

impl Default for EdgeSwitchConfig {
    fn default() -> Self {
        Self {
            tolerance_px: 2,
            cooldown: Duration::from_millis(100),
            release_combo: [0x10, 0x11, 0x12, 0x51],
        }
    }
}

pub struct EdgeSwitch {
    config: EdgeSwitchConfig,
    /// device_id → 本端共享边
    edges: HashMap<String, Edge>,
    state: ControlState,
    last_switch: Option<Instant>,
    /// 鼠标已离开两侧边缘禁区（re-arm：边界抖动防乒乓）
    armed: bool,
    /// 最近一次转发给受控端的归一化绝对坐标（回移判定用）
    last_forwarded: (i32, i32),
    /// 组合键按下集合（含 L/R 变体）
    pressed: HashSet<u16>,
}

// 组合键 vk：Shift/Ctrl/Alt 含通用码与左右变体；Q 固定
const VK_SHIFT: u16 = 0x10;
const VK_CTRL: u16 = 0x11;
const VK_ALT: u16 = 0x12;
const VK_SHIFT_L: u16 = 0xA0;
const VK_SHIFT_R: u16 = 0xA1;
const VK_CTRL_L: u16 = 0xA2;
const VK_CTRL_R: u16 = 0xA3;
const VK_ALT_L: u16 = 0xA4;
const VK_ALT_R: u16 = 0xA5;
const VK_Q: u16 = 0x51;

impl EdgeSwitch {
    pub fn new(edges: HashMap<String, Edge>) -> Self {
        Self {
            config: EdgeSwitchConfig::default(),
            edges,
            state: ControlState::Idle,
            last_switch: None,
            armed: false,
            last_forwarded: (0, 0),
            pressed: HashSet::new(),
        }
    }

    pub fn config_mut(&mut self) -> &mut EdgeSwitchConfig {
        &mut self.config
    }

    pub fn set_edges(&mut self, edges: HashMap<String, Edge>) {
        self.edges = edges;
    }

    pub fn edges(&self) -> &HashMap<String, Edge> {
        &self.edges
    }

    /// 当前受控设备（None = 本机自主）
    pub fn controlling_device(&self) -> Option<&str> {
        match &self.state {
            ControlState::Idle => None,
            ControlState::Controlling { device_id, .. } => Some(device_id),
        }
    }

    /// 归一化到 0..65535（与 win-integration K5 注入同一 Raymond Chen 公式：
    /// abs = px × 65536 / extent；逆向 px = abs × extent / 65536）
    fn normalize(&self, x: i32, own: &ScreenRect) -> i32 {
        let w = own.w.max(1);
        (((x - own.x) as i64 * 65536 / w as i64).clamp(0, 65535)) as i32
    }

    /// 对端像素换算（归一化 abs → 对端虚拟桌面物理像素）
    fn to_peer_px(abs: i32, peer: &ScreenRect, horizontal: bool) -> i32 {
        let extent = if horizontal {
            peer.w.max(1)
        } else {
            peer.h.max(1)
        } as i64;
        let origin = if horizontal { peer.x } else { peer.y };
        (abs as i64 * extent / 65536) as i32 + origin
    }

    /// 本地钩子事件决策（纯逻辑，钩子线程内调用）
    pub fn on_local_event(&mut self, ev: &RawInput, own: &ScreenRect) -> Decision {
        match ev {
            RawInput::KeyDown { vk, .. } => {
                self.pressed.insert(*vk);
            }
            RawInput::KeyUp { vk, .. } => {
                self.pressed.remove(vk);
            }
            _ => {}
        }
        match &self.state {
            ControlState::Controlling { .. } => {
                // 组合键释放（优先级低于边缘回移，此处为主动兜底）
                if self.release_combo_pressed() {
                    self.force_release("hotkey");
                    return Decision::Suppress;
                }
                if let RawInput::MouseMove { x, .. } = ev {
                    self.last_forwarded = (self.normalize(*x, own), self.normalize_y(ev, own));
                }
                Decision::Forward
            }
            ControlState::Idle => {
                let RawInput::MouseMove { x, y } = ev else {
                    return Decision::Passthrough;
                };
                // re-arm：离开四侧边缘禁区后才允许再次切换
                let tol = self.config.tolerance_px;
                let left_edge = own.x;
                let right_edge = own.x + own.w - 1;
                let top_edge = own.y;
                let bottom_edge = own.y + own.h - 1;
                let hit_left = *x <= left_edge + tol;
                let hit_right = *x >= right_edge - tol;
                let hit_up = *y <= top_edge + tol;
                let hit_down = *y >= bottom_edge - tol;
                if !hit_left && !hit_right && !hit_up && !hit_down {
                    self.armed = true;
                }
                // T-B7-7：垂直缘与水平缘同帧命中 → corner_edge 对角线唯一裁决
                let v_edge = if hit_left {
                    Some(Edge::Left)
                } else if hit_right {
                    Some(Edge::Right)
                } else {
                    None
                };
                let h_edge = if hit_up {
                    Some(Edge::Up)
                } else if hit_down {
                    Some(Edge::Down)
                } else {
                    None
                };
                let candidate = match (v_edge, h_edge) {
                    (Some(e), None) | (None, Some(e)) => Some(e),
                    (Some(_), Some(_)) => Some(corner_edge(
                        *x - left_edge,
                        *y - top_edge,
                        own.w.max(1),
                        own.h.max(1),
                    )),
                    (None, None) => None,
                };
                let Some(edge) = candidate else {
                    return Decision::Passthrough;
                };
                if !self.armed || !self.cooldown_elapsed() {
                    return Decision::Passthrough;
                }
                let Some(device_id) = self
                    .edges
                    .iter()
                    .find_map(|(id, e)| (*e == edge).then(|| id.clone()))
                else {
                    return Decision::Passthrough;
                };
                self.state = ControlState::Controlling {
                    device_id: device_id.clone(),
                    edge,
                };
                self.last_switch = Some(Instant::now());
                self.armed = false;
                Decision::SwitchTo(device_id)
            }
        }
    }

    fn normalize_y(&self, ev: &RawInput, own: &ScreenRect) -> i32 {
        match ev {
            RawInput::MouseMove { y, .. } => {
                let h = own.h.max(1);
                (((y - own.y) as i64 * 65536 / h as i64).clamp(0, 65535)) as i32
            }
            _ => self.last_forwarded.1,
        }
    }

    fn cooldown_elapsed(&self) -> bool {
        self.last_switch
            .is_none_or(|t| t.elapsed() >= self.config.cooldown)
    }

    fn release_combo_pressed(&self) -> bool {
        let (shift, ctrl, alt, q) = (
            self.pressed.contains(&VK_SHIFT)
                || self.pressed.contains(&VK_SHIFT_L)
                || self.pressed.contains(&VK_SHIFT_R),
            self.pressed.contains(&VK_CTRL)
                || self.pressed.contains(&VK_CTRL_L)
                || self.pressed.contains(&VK_CTRL_R),
            self.pressed.contains(&VK_ALT)
                || self.pressed.contains(&VK_ALT_L)
                || self.pressed.contains(&VK_ALT_R),
            self.pressed.contains(&VK_Q),
        );
        shift && ctrl && alt && q
    }

    /// 边缘回移判定：最近转发坐标换算到对端像素，抵达对端回移边（容差内）即释放。
    /// 本端 Right ↔ 对端 Left；本端 Left ↔ 对端 Right；本端 Up ↔ 对端 Down；
    /// 本端 Down ↔ 对端 Up（T-B7-7 扩上下）。
    pub fn should_release(&self, peer_screen: &ScreenRect) -> bool {
        let ControlState::Controlling { edge, .. } = &self.state else {
            return false;
        };
        // 对端屏幕信息缺失（旧端心跳）：不判定，仅快捷键/手动切回
        if peer_screen.w <= 0 || peer_screen.h <= 0 {
            return false;
        }
        let tol = self.config.tolerance_px;
        let px = Self::to_peer_px(self.last_forwarded.0, peer_screen, true);
        match edge {
            Edge::Right => px <= peer_screen.x + tol,
            Edge::Left => px >= peer_screen.x + peer_screen.w - 1 - tol,
            Edge::Up => {
                let py = Self::to_peer_px(self.last_forwarded.1, peer_screen, false);
                py >= peer_screen.y + peer_screen.h - 1 - tol
            }
            Edge::Down => {
                let py = Self::to_peer_px(self.last_forwarded.1, peer_screen, false);
                py <= peer_screen.y + tol
            }
        }
    }

    /// 强制释放（边缘回移命中 / 快捷键 / 手动 / 会话断开）。释放后鼠标须先
    /// 离开边缘带才能再次切换（armed 复位，防乒乓）。
    pub fn force_release(&mut self, _reason: &str) {
        self.state = ControlState::Idle;
        self.armed = false;
        self.last_switch = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own() -> ScreenRect {
        ScreenRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        }
    }

    fn peer_right() -> ScreenRect {
        // 对端虚拟桌面 (-1920,0,1920,1080)：在本端右侧
        ScreenRect {
            x: -1920,
            y: 0,
            w: 1920,
            h: 1080,
        }
    }

    fn es_right(map_device: &str) -> EdgeSwitch {
        let mut m = HashMap::new();
        m.insert(map_device.to_string(), Edge::Right);
        EdgeSwitch::new(m)
    }

    fn mv(x: i32) -> RawInput {
        RawInput::MouseMove { x, y: 540 }
    }

    fn key(vk: u16) -> RawInput {
        RawInput::KeyDown { vk, scan: 0 }
    }

    #[test]
    fn switch_requires_arm_cooldown_and_mapped_edge() {
        let mut es = es_right("dev-b");
        // 初始停在边缘不切（未武装）
        assert_eq!(es.on_local_event(&mv(1919), &own()), Decision::Passthrough);
        // 移入中间 → 武装
        assert_eq!(es.on_local_event(&mv(960), &own()), Decision::Passthrough);
        // 回到右缘（容差 2px：1917..=1919）→ 切换
        assert_eq!(
            es.on_local_event(&mv(1918), &own()),
            Decision::SwitchTo("dev-b".into())
        );
        assert_eq!(es.controlling_device(), Some("dev-b"));
        // 受控中：本地事件全部 Forward（抑制）
        assert_eq!(es.on_local_event(&mv(500), &own()), Decision::Forward);
        assert_eq!(
            es.on_local_event(&RawInput::KeyDown { vk: 0x41, scan: 0 }, &own()),
            Decision::Forward
        );
    }

    #[test]
    fn unmapped_edge_never_switches() {
        let mut es = EdgeSwitch::new(HashMap::new()); // 空映射
        assert_eq!(es.on_local_event(&mv(500), &own()), Decision::Passthrough);
        assert_eq!(es.on_local_event(&mv(1919), &own()), Decision::Passthrough);
        assert_eq!(es.controlling_device(), None);
    }

    #[test]
    fn rearm_and_cooldown_prevent_pingpong() {
        let mut es = es_right("dev-b");
        es.config_mut().cooldown = Duration::from_millis(50);
        assert_eq!(es.on_local_event(&mv(960), &own()), Decision::Passthrough);
        assert_eq!(
            es.on_local_event(&mv(1919), &own()),
            Decision::SwitchTo("dev-b".into())
        );
        // 快捷键切回
        assert_eq!(es.on_local_event(&key(0x11), &own()), Decision::Forward);
        assert_eq!(es.on_local_event(&key(0x12), &own()), Decision::Forward);
        assert_eq!(es.on_local_event(&key(0x10), &own()), Decision::Forward);
        assert_eq!(es.on_local_event(&key(0x51), &own()), Decision::Suppress);
        assert_eq!(es.controlling_device(), None);
        // 释放后未武装：即使冷却已过，停在边缘也不会立刻再切
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(es.on_local_event(&mv(1919), &own()), Decision::Passthrough);
        // 移入中间武装 → 回边缘 → 再次切换
        assert_eq!(es.on_local_event(&mv(960), &own()), Decision::Passthrough);
        assert_eq!(
            es.on_local_event(&mv(1919), &own()),
            Decision::SwitchTo("dev-b".into())
        );
    }

    #[test]
    fn remote_edge_return_releases() {
        let mut es = es_right("dev-b");
        assert_eq!(es.on_local_event(&mv(960), &own()), Decision::Passthrough);
        assert_eq!(
            es.on_local_event(&mv(1919), &own()),
            Decision::SwitchTo("dev-b".into())
        );
        // 转发的归一化坐标 x=0（本机 1919 ≈ 右缘 → abs ≈ 65535）……
        // 直接模拟"转发坐标位于对端左缘"：abs=0 → 对端像素 = -1920 + 0
        es.last_forwarded = (0, 32768);
        assert!(es.should_release(&peer_right()), "对端左缘应释放");
        // 中间位置不释放
        es.last_forwarded = (32768, 32768);
        assert!(!es.should_release(&peer_right()));
        // 对端屏幕信息缺失（旧端）：不判定
        es.last_forwarded = (0, 0);
        assert!(!es.should_release(&ScreenRect::default()));
    }

    #[test]
    fn edge_serde_roundtrip() {
        for e in [Edge::Left, Edge::Right, Edge::Up, Edge::Down] {
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<Edge>(&json).unwrap(), e);
            assert_eq!(json, format!("\"{}\"", e.as_str()));
        }
        assert_eq!(
            serde_json::from_str::<Edge>("\"left\"").unwrap(),
            Edge::Left
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-8）字面测试名优先于 rustc 命名惯例
    fn cornerEdge_diagonalSplit_deterministic() {
        let (w, h) = (1920i32, 1080i32);
        // 四角坐标臂：恰在角点（dx=dy=0）→ tie 归水平缘
        assert_eq!(corner_edge(0, 0, w, h), Edge::Left);
        assert_eq!(corner_edge(w - 1, 0, w, h), Edge::Right);
        assert_eq!(corner_edge(0, h - 1, w, h), Edge::Left);
        assert_eq!(corner_edge(w - 1, h - 1, w, h), Edge::Right);
        // 左上角邻域：贴垂直缘 → Left，贴水平缘 → Up
        assert_eq!(corner_edge(1, 5, w, h), Edge::Left);
        assert_eq!(corner_edge(5, 1, w, h), Edge::Up);
        // 右上角邻域
        assert_eq!(corner_edge(w - 2, 6, w, h), Edge::Right);
        assert_eq!(corner_edge(w - 6, 2, w, h), Edge::Up);
        // 左下角邻域
        assert_eq!(corner_edge(2, h - 6, w, h), Edge::Left);
        assert_eq!(corner_edge(6, h - 2, w, h), Edge::Down);
        // 右下角邻域
        assert_eq!(corner_edge(w - 2, h - 6, w, h), Edge::Right);
        assert_eq!(corner_edge(w - 6, h - 2, w, h), Edge::Down);
        // 对角线上（dx==dy）恒归水平缘：重复调用逐位一致（确定性）
        assert_eq!(corner_edge(5, 5, w, h), corner_edge(5, 5, w, h));
        assert_eq!(corner_edge(5, 5, w, h), Edge::Left);
    }

    #[test]
    fn vertical_edge_switch_and_return_release() {
        // 对端屏在本端上方 (0,-1080,1920,1080)：本端上缘交出控制权
        let mut m = HashMap::new();
        m.insert("dev-up".to_string(), Edge::Up);
        let mut es = EdgeSwitch::new(m);
        let own = own();
        let mv2 = |x: i32, y: i32| RawInput::MouseMove { x, y };
        assert_eq!(
            es.on_local_event(&mv2(960, 540), &own),
            Decision::Passthrough
        );
        assert_eq!(
            es.on_local_event(&mv2(960, 1), &own),
            Decision::SwitchTo("dev-up".into())
        );
        assert_eq!(es.on_local_event(&mv2(960, 500), &own), Decision::Forward);
        // 回移判定：转发 y 归一化到底 → 对端下缘（本端 Up ↔ 对端 Down 对面）
        let peer_above = ScreenRect {
            x: 0,
            y: -1080,
            w: 1920,
            h: 1080,
        };
        es.last_forwarded = (32768, 65535);
        assert!(es.should_release(&peer_above), "对端下缘应释放");
        es.last_forwarded = (32768, 32768);
        assert!(!es.should_release(&peer_above));
    }
}
