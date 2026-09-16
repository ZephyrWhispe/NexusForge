//! K4/K5 输入捕获与注入（docs/impl/05 K4/K5）。
//!
//! 捕获：WH_KEYBOARD_LL/WH_MOUSE_LL 低级钩子安装在专用线程（该线程必须持续
//! 泵消息），回调契约见 [`InputHookPort`]（<5ms、返回 false 抑制本事件）。
//! 注入：SendInput，鼠标绝对坐标按虚拟桌面归一化到 0..65535。
//!
//! 防回环（docs/impl/05 K4 "不回注本机"）：钩子层过滤带 LLKHF_INJECTED /
//! LLMHF_INJECTED 标记的事件——本机 SendInput 注入不触发捕获回调，杜绝
//! "捕获→发送→注入→再捕获" 回环。
//!
//! 节流：鼠标 move 仅限**转发频率** ≤125Hz（时间门限丢弃），不影响本地
//! 系统行为（被丢弃的事件照常 CallNextHookEx 放行）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, GetSystemMetrics, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, KBDLLHOOKSTRUCT_FLAGS, LLKHF_INJECTED,
    LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN,
    WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use host_core::error::AppError;
use host_core::ports::{InputHookPort, InputInjectPort, RawInput, ScreenInfoPort, ScreenRect};

/// 鼠标 move 转发最小间隔（125Hz 采样上限）
const MOVE_MIN_INTERVAL_MS: u64 = 8;

type Cb = Arc<dyn Fn(&RawInput) -> bool + Send + Sync>;

/// HHOOK 不是 Send（内含裸指针）；钩子句柄由安装线程（本模块专属线程）独占
/// 创建/使用，其它线程仅经 Shared 持有地址用于判空/卸载——Send/Sync 由该
/// 线程独占约束保证（参照 hotkey.rs SendHwnd 先例）。
struct SendHook(HHOOK);
unsafe impl Send for SendHook {}
unsafe impl Sync for SendHook {}

enum Req {
    Start { resp: Sender<Result<(), AppError>> },
    Stop { resp: Sender<Result<(), AppError>> },
}

struct Shared {
    /// 钩子线程 OS tid（就绪信号；PostThreadMessageW 唤醒用）
    thread_id: Mutex<Option<u32>>,
    /// 请求接收端（钩子线程专用；Sender 由 InputHookWin 持有）
    req_rx: Mutex<Option<std::sync::mpsc::Receiver<Req>>>,
    /// 用户回调（start_capture 设置，stop 清除）
    cb: Mutex<Option<Cb>>,
    /// [键盘钩子, 鼠标钩子]
    hooks: Mutex<[Option<SendHook>; 2]>,
    /// 节流时间基准
    epoch: Instant,
    /// 上次转发 move 的相对毫秒（0 = 尚无）
    last_move_ms: AtomicU64,
}

// 钩子过程运行在安装线程（即本模块专属线程），经 thread_local 取 Shared。
thread_local! {
    static SHARED: std::cell::RefCell<Option<Arc<Shared>>> = std::cell::RefCell::new(None);
}

/// HIWORD(mouseData) → 有符号滚轮增量（正=上滚）
fn wheel_delta(mouse_data: u32) -> i32 {
    ((mouse_data >> 16) & 0xFFFF) as u16 as i16 as i32
}

/// 绝对坐标 → 0..65535 归一化（虚拟桌面，副屏可为负坐标）。
/// 权威公式（Raymond Chen）：abs = px * 65536 / extent，可逆（px = abs * extent / 65536）。
fn normalize_with(x: i32, y: i32, vx: i32, vy: i32, cx: i32, cy: i32) -> (i32, i32) {
    let cx = cx.max(1);
    let cy = cy.max(1);
    let nx = ((x - vx) as i64 * 65536 / cx as i64).clamp(0, 65535) as i32;
    let ny = ((y - vy) as i64 * 65536 / cy as i64).clamp(0, 65535) as i32;
    (nx, ny)
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let mut suppress = false;
    if code >= 0 {
        SHARED.with(|s| {
            let borrow = s.borrow();
            let Some(shared) = &*borrow else { return };
            let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            // 注入事件不回调（防回环）
            if kb.flags & LLKHF_INJECTED != KBDLLHOOKSTRUCT_FLAGS(0) {
                return;
            }
            let ev = match wparam.0 as u32 {
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    RawInput::KeyDown { vk: kb.vkCode as u16, scan: kb.scanCode }
                }
                WM_KEYUP | WM_SYSKEYUP => RawInput::KeyUp { vk: kb.vkCode as u16, scan: kb.scanCode },
                _ => return,
            };
            // 临时守卫显式绑定（if-let 判定式临时值存活到块尾，会压长 borrow 生命周期）
            let cb_guard = shared.cb.lock().expect("输入回调锁");
            if let Some(cb) = cb_guard.as_ref() {
                if !cb(&ev) {
                    suppress = true;
                }
            }
        });
    }
    if suppress {
        return LRESULT(1); // 不交还钩子链：本地应用收不到该事件
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let mut suppress = false;
    if code >= 0 {
        SHARED.with(|s| {
            let borrow = s.borrow();
            let Some(shared) = &*borrow else { return };
            let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            // 注入事件不回调（防回环）；MSLLHOOKSTRUCT.flags 是普通 u32
            if ms.flags & LLMHF_INJECTED != 0 {
                return;
            }
            let ev = match wparam.0 as u32 {
                WM_MOUSEMOVE => {
                    // 节流：仅限转发频率；被丢弃的事件照常放行本地
                    let now = shared.epoch.elapsed().as_millis() as u64;
                    let last = shared.last_move_ms.load(Ordering::Relaxed);
                    if last != 0 && now.saturating_sub(last) < MOVE_MIN_INTERVAL_MS {
                        return;
                    }
                    shared.last_move_ms.store(now, Ordering::Relaxed);
                    RawInput::MouseMove { x: ms.pt.x, y: ms.pt.y }
                }
                WM_LBUTTONDOWN => RawInput::MouseDown { button: 0, x: ms.pt.x, y: ms.pt.y },
                WM_LBUTTONUP => RawInput::MouseUp { button: 0, x: ms.pt.x, y: ms.pt.y },
                WM_RBUTTONDOWN => RawInput::MouseDown { button: 1, x: ms.pt.x, y: ms.pt.y },
                WM_RBUTTONUP => RawInput::MouseUp { button: 1, x: ms.pt.x, y: ms.pt.y },
                WM_MBUTTONDOWN => RawInput::MouseDown { button: 2, x: ms.pt.x, y: ms.pt.y },
                WM_MBUTTONUP => RawInput::MouseUp { button: 2, x: ms.pt.x, y: ms.pt.y },
                WM_MOUSEWHEEL => {
                    RawInput::Wheel { delta: wheel_delta(ms.mouseData), x: ms.pt.x, y: ms.pt.y }
                }
                // 水平滚轮等 v1 不捕获（透传）
                _ => return,
            };
            let cb_guard = shared.cb.lock().expect("输入回调锁");
            if let Some(cb) = cb_guard.as_ref() {
                if !cb(&ev) {
                    suppress = true;
                }
            }
        });
    }
    if suppress {
        return LRESULT(1);
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn hook_thread(shared: Arc<Shared>) {
    SHARED.with(|s| *s.borrow_mut() = Some(shared.clone()));
    if let Ok(mut g) = shared.thread_id.lock() {
        *g = Some(unsafe { GetCurrentThreadId() });
    }
    let mut msg = MSG::default();
    unsafe {
        // 无窗口：GetMessageW(null hwnd) 收线程消息即可驱动钩子泵
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            if msg.message == WM_APP {
                let reqs: Vec<Req> = {
                    let mut guard = shared.req_rx.lock().expect("req 锁");
                    let mut v = Vec::new();
                    while let Ok(req) = guard.as_mut().expect("req_rx 已被线程取走").try_recv() {
                        v.push(req);
                    }
                    v
                };
                for req in reqs {
                    handle_req(&shared, req);
                }
            }
        }
    }
    SHARED.with(|s| *s.borrow_mut() = None);
}

fn handle_req(shared: &Shared, req: Req) {
    match req {
        Req::Start { resp } => {
            let mut hooks = shared.hooks.lock().expect("hooks 锁");
            if hooks[0].is_some() || hooks[1].is_some() {
                let _ = resp.send(Err(AppError::module("WIN_INPUT_001", "输入捕获已启动", None)));
                return;
            }
            let hmod: HINSTANCE = unsafe { GetModuleHandleW(None).unwrap_or_default().into() };
            let kb = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), hmod, 0) };
            let ms = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hmod, 0) };
            match (kb, ms) {
                (Ok(k), Ok(m)) => {
                    hooks[0] = Some(SendHook(k));
                    hooks[1] = Some(SendHook(m));
                    shared.last_move_ms.store(0, Ordering::Relaxed);
                    tracing::info!("低级输入钩子已安装");
                    let _ = resp.send(Ok(()));
                }
                (k, m) => {
                    if let Ok(h) = k {
                        let _ = unsafe { UnhookWindowsHookEx(h) };
                    }
                    if let Ok(h) = m {
                        let _ = unsafe { UnhookWindowsHookEx(h) };
                    }
                    let err = k.err().or_else(|| m.err()).map(|e| e.to_string()).unwrap_or_default();
                    let _ = resp.send(Err(AppError::module(
                        "WIN_INPUT_002",
                        format!("安装低级钩子失败: {err}"),
                        None,
                    )));
                }
            }
        }
        Req::Stop { resp } => {
            let mut hooks = shared.hooks.lock().expect("hooks 锁");
            if let Some(h) = hooks[0].take() {
                let _ = unsafe { UnhookWindowsHookEx(h.0) };
            }
            if let Some(h) = hooks[1].take() {
                let _ = unsafe { UnhookWindowsHookEx(h.0) };
            }
            *shared.cb.lock().expect("输入回调锁") = None;
            let _ = resp.send(Ok(()));
            // 不退出消息循环：钩子线程须保活以支持 stop 后重新 start（PostQuitMessage
            // 会让 GetMessageW 返回 0 → 线程死亡，后续 Start 请求永久无响应）
        }
    }
}

/// 低级输入捕获实现（K4）
pub struct InputHookWin {
    shared: Arc<Shared>,
    _req_tx: Sender<Req>,
}

impl InputHookWin {
    pub fn new() -> Result<Self, AppError> {
        let (req_tx, req_rx) = channel::<Req>();
        let shared = Arc::new(Shared {
            thread_id: Mutex::new(None),
            req_rx: Mutex::new(Some(req_rx)),
            cb: Mutex::new(None),
            hooks: Mutex::new([None, None]),
            epoch: Instant::now(),
            last_move_ms: AtomicU64::new(0),
        });
        let s = shared.clone();
        std::thread::Builder::new()
            .name("input-hook".into())
            .spawn(move || hook_thread(s))
            .map_err(|e| AppError::module("WIN_INPUT_003", e.to_string(), None))?;
        // 等待钩子线程就绪（tid 写回）
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if shared.thread_id.lock().expect("tid 锁").is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(Self { shared, _req_tx: req_tx })
    }

    fn call(&self, make: impl FnOnce(Sender<Result<(), AppError>>) -> Req) -> Result<(), AppError> {
        let (tx, rx) = channel();
        self._req_tx
            .send(make(tx))
            .map_err(|e| AppError::module("WIN_INPUT_004", e.to_string(), None))?;
        let tid = self
            .shared
            .thread_id
            .lock()
            .expect("tid 锁")
            .ok_or_else(|| AppError::module("WIN_INPUT_005", "钩子线程未就绪", None))?;
        unsafe { PostThreadMessageW(tid, WM_APP, WPARAM(0), LPARAM(0)) }
            .map_err(|e| AppError::module("WIN_INPUT_004", e.to_string(), None))?;
        rx.recv_timeout(Duration::from_secs(2))
            .map_err(|e| AppError::module("WIN_INPUT_005", e.to_string(), None))?
    }
}

impl InputHookPort for InputHookWin {
    fn start_capture(&self, cb: Box<dyn Fn(&RawInput) -> bool + Send + Sync>) -> Result<(), AppError> {
        // 先挂回调再启动（首帧即有回调）
        *self.shared.cb.lock().expect("输入回调锁") = Some(Arc::from(cb));
        self.call(|resp| Req::Start { resp })
    }

    fn stop_capture(&self) -> Result<(), AppError> {
        self.call(|resp| Req::Stop { resp })
    }
}

// ---------------------------------------------------------------------------
// K5 输入注入（SendInput，绝对坐标虚拟桌面归一化）
// ---------------------------------------------------------------------------

/// 输入注入实现（K5）
pub struct InputInjectWin;

impl InputInjectWin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for InputInjectWin {
    fn default() -> Self {
        Self::new()
    }
}

impl InputInjectPort for InputInjectWin {
    fn inject(&self, events: &[RawInput]) -> Result<(), AppError> {
        for ev in events {
            let input = to_input(ev)?;
            // windows 0.58：SendInput(&[INPUT], cbsize) 两参数签名，返回已注入数
            let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
            if sent != 1 {
                return Err(AppError::module("WIN_INPUT_007", "SendInput 被系统拦截", None));
            }
        }
        Ok(())
    }
}

fn to_input(ev: &RawInput) -> Result<INPUT, AppError> {
    match ev {
        RawInput::KeyDown { vk, scan } => Ok(key_input(*vk, *scan, false)),
        RawInput::KeyUp { vk, scan } => Ok(key_input(*vk, *scan, true)),
        RawInput::MouseMove { x, y } => Ok(mouse_move(*x, *y)),
        RawInput::MouseDown { button, .. } => mouse_button(*button, true),
        RawInput::MouseUp { button, .. } => mouse_button(*button, false),
        RawInput::Wheel { delta, .. } => Ok(mouse_wheel(*delta)),
    }
}

fn key_input(vk: u16, scan: u32, up: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_move(x: i32, y: i32) -> INPUT {
    // GetSystemMetrics 无失败路径（返回 0），unwrap_or 值仅防御性
    let (nx, ny) = unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let cx = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let cy = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        normalize_with(x, y, vx, vy, cx, cy)
    };
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: nx,
                dy: ny,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_button(button: u8, down: bool) -> Result<INPUT, AppError> {
    let flags: MOUSE_EVENT_FLAGS = match (button, down) {
        (0, true) => MOUSEEVENTF_LEFTDOWN,
        (0, false) => MOUSEEVENTF_LEFTUP,
        (1, true) => MOUSEEVENTF_RIGHTDOWN,
        (1, false) => MOUSEEVENTF_RIGHTUP,
        (2, true) => MOUSEEVENTF_MIDDLEDOWN,
        (2, false) => MOUSEEVENTF_MIDDLEUP,
        _ => return Err(AppError::module("WIN_INPUT_006", format!("未知鼠标键 {button}"), None)),
    };
    Ok(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    })
}

fn mouse_wheel(delta: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: delta as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

// ---------------------------------------------------------------------------
// 虚拟桌面信息（K7 边缘切换 / 归一化共用）
// ---------------------------------------------------------------------------

/// 虚拟桌面矩形（SM_X/YVIRTUALSCREEN 原点 + SM_CX/CYVIRTUALSCREEN 尺寸）
pub struct ScreenInfoWin;

impl ScreenInfoWin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ScreenInfoWin {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenInfoPort for ScreenInfoWin {
    fn virtual_desktop(&self) -> Result<ScreenRect, AppError> {
        let (x, y, w, h) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        if w <= 0 || h <= 0 {
            return Err(AppError::module("WIN_SCREEN_001", format!("虚拟桌面尺寸非法: {w}x{h}"), None));
        }
        Ok(ScreenRect { x, y, w, h })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_delta_extracts_hiword_signed() {
        assert_eq!(wheel_delta(120u32 << 16), 120);
        assert_eq!(wheel_delta(((-120i16) as u16 as u32) << 16), -120);
        assert_eq!(wheel_delta(1u32 << 16), 1);
    }

    #[test]
    fn normalize_maps_virtual_desktop_to_u16_range() {
        // 主屏 (0,0,1920,1080)；期望值按 abs = px*65536/extent 精确计算
        let (nx, ny) = normalize_with(0, 0, 0, 0, 1920, 1080);
        assert_eq!((nx, ny), (0, 0));
        let (nx, ny) = normalize_with(1919, 1079, 0, 0, 1920, 1080);
        assert_eq!((nx, ny), (65501, 65475));
        let (nx, ny) = normalize_with(960, 540, 0, 0, 1920, 1080);
        assert_eq!((nx, ny), (32768, 32768));
        // 副屏负坐标：虚拟桌面 (-1920,0,3840,1080)
        let (nx, ny) = normalize_with(-1920, 0, -1920, 0, 3840, 1080);
        assert_eq!((nx, ny), (0, 0));
        let (nx, ny) = normalize_with(1919, 1079, -1920, 0, 3840, 1080);
        assert_eq!((nx, ny), (65518, 65475));
        // 越界钳制
        let (nx, ny) = normalize_with(5000, -100, 0, 0, 1920, 1080);
        assert_eq!((nx, ny), (65535, 0));
    }

    /// 钩子安装/卸载往返（不注入事件：注入会被防回环过滤器吞掉，属预期）
    #[test]
    fn hook_start_stop_roundtrip() {
        let hook = InputHookWin::new().unwrap();
        hook.start_capture(Box::new(|_| true)).unwrap();
        // 重复启动必须报错
        assert!(hook.start_capture(Box::new(|_| true)).is_err());
        hook.stop_capture().unwrap();
        // 停止后可重新启动
        hook.start_capture(Box::new(|_| true)).unwrap();
        hook.stop_capture().unwrap();
    }

    /// 虚拟桌面矩形冒烟（有显示器的机器必然 w/h > 0）
    #[test]
    fn virtual_desktop_reports_positive_extent() {
        let rect = ScreenInfoWin::new().virtual_desktop().unwrap();
        assert!(rect.w > 0 && rect.h > 0, "虚拟桌面尺寸非法: {rect:?}");
    }
}
