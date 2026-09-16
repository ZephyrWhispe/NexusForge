//! 全局快捷键 OS 层（docs/impl/01 S6.2 / docs/UI-PLAN.md U2-4）
//!
//! 专用线程：隐藏窗口 + GetMessage 循环。register/unregister 请求经通道投递，
//! PostMessage(WM_APP) 唤醒消息循环处理；WM_HOTKEY → dispatcher(os_id) + handlers。

use std::collections::HashMap;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
    PostMessageW, RegisterClassW, SetWindowLongPtrW, TranslateMessage, GWLP_USERDATA,
    HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_HOTKEY, WNDCLASSW,
};

use host_core::error::AppError;
use host_core::ports::HotkeyWinPort;

type Dispatcher = Arc<Mutex<Option<Arc<dyn Fn(i32) + Send + Sync>>>>;
type Handlers = Arc<Mutex<HashMap<i32, Arc<dyn Fn() + Send + Sync>>>>;

enum Req {
    Register { id: i32, mods: u32, vk: u32, resp: Sender<Result<(), AppError>> },
    Unregister { id: i32, resp: Sender<Result<(), AppError>> },
}

/// HWND 不是 Send（内含裸指针）；此处 HWND 由本模块专属线程独占创建/使用，
/// 其它线程仅持有地址用于 PostMessage——Send/Sync 由该约束保证。
struct SendHwnd(usize);
unsafe impl Send for SendHwnd {}
unsafe impl Sync for SendHwnd {}
impl SendHwnd {
    fn get(&self) -> HWND {
        HWND(self.0 as *mut core::ffi::c_void)
    }
}

struct Shared {
    hwnd: Mutex<Option<SendHwnd>>,
    /// 请求接收端（wndproc 专用；Sender 由 HotkeyWin 持有）
    req_rx: Mutex<Option<std::sync::mpsc::Receiver<Req>>>,
    dispatcher: Dispatcher,
    handlers: Handlers,
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_HOTKEY {
        let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if raw != 0 {
            let shared = &*(raw as *const Shared);
            let os_id = wparam.0 as i32;
            tracing::info!(os_id, "WM_HOTKEY 到达");
            if let Ok(d) = shared.dispatcher.lock() {
                if let Some(f) = d.as_ref() {
                    f(os_id);
                } else {
                    tracing::warn!("WM_HOTKEY 到达但 dispatcher 未设置");
                }
            }
            if let Ok(h) = shared.handlers.lock() {
                if let Some(f) = h.get(&os_id) {
                    f();
                }
            }
        }
        return LRESULT(0);
    }
    if msg == WM_APP {
        // 排空注册请求队列
        let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if raw != 0 {
            let shared = &*(raw as *const Shared);
            let pending: Vec<Req> = {
                let mut guard = shared.req_rx.lock().unwrap();
                let mut v = Vec::new();
                while let Ok(req) = guard.as_mut().expect("req_rx 已被线程取走").try_recv() {
                    v.push(req);
                }
                v
            };
            for req in pending {
                match req {
                    Req::Register { id, mods, vk, resp } => {
                        let r = RegisterHotKey(hwnd, id, HOT_KEY_MODIFIERS(mods), vk)
                            .map_err(|e| AppError::module("WIN_HOTKEY_001", e.to_string(), None));
                        let _ = resp.send(r);
                    }
                    Req::Unregister { id, resp } => {
                        let r = UnregisterHotKey(hwnd, id)
                            .map_err(|e| AppError::module("WIN_HOTKEY_002", e.to_string(), None));
                        let _ = resp.send(r);
                    }
                }
            }
        }
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

pub struct HotkeyWin {
    shared: Arc<Shared>,
    /// 保持发送端存活（ dropping 会导致请求通道断开）
    _req_tx: Sender<Req>,
}

impl HotkeyWin {
    pub fn new() -> Result<Self, AppError> {
        let (req_tx, req_rx) = channel::<Req>();
        let shared = Arc::new(Shared {
            hwnd: Mutex::new(None),
            req_rx: Mutex::new(Some(req_rx)),
            dispatcher: Arc::new(Mutex::new(None)),
            handlers: Arc::new(Mutex::new(HashMap::new())),
        });
        let shared_thread = shared.clone();
        std::thread::Builder::new()
            .name("hotkey-loop".into())
            .spawn(move || unsafe {
                let name: Vec<u16> = format!("NexusForgeHotkeyWnd{}\0", std::process::id())
                    .encode_utf16()
                    .collect();
                let class_name = PCWSTR(name.as_ptr());
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    lpszClassName: class_name,
                    hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
                    ..Default::default()
                };
                if RegisterClassW(&wc) == 0 {
                    return;
                }
                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class_name,
                    PCWSTR::null(),
                    WINDOW_STYLE::default(),
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    None,
                    wc.hInstance,
                    None,
                )
                .unwrap_or_default();
                if hwnd.is_invalid() {
                    return;
                }
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, Arc::into_raw(shared_thread.clone()) as isize);
                // 就绪信号：hwnd 必须写回 shared（register 依赖它投递 WM_APP 唤醒消息循环）
                if let Ok(mut g) = shared_thread.hwnd.lock() {
                    *g = Some(SendHwnd(hwnd.0 as usize));
                }
                let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
                while GetMessageW(&mut msg, hwnd, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            })
            .map_err(|e| AppError::module("WIN_HOTKEY_003", e.to_string(), None))?;
        // 等待窗口就绪（超时不致命：后续 register 仍会失败并给出错误码）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if shared.hwnd.lock().unwrap().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Ok(Self { shared, _req_tx: req_tx })
    }
}

impl Default for HotkeyWin {
    fn default() -> Self {
        Self::new().expect("HotkeyWin 初始化失败")
    }
}

impl HotkeyWinPort for HotkeyWin {
    fn register(&self, hotkey_id: i32, modifiers: u32, vk: u32) -> Result<(), AppError> {
        let (tx, rx) = channel();
        self._req_tx
            .send(Req::Register { id: hotkey_id, mods: modifiers, vk, resp: tx })
            .map_err(|e| AppError::module("WIN_HOTKEY_004", e.to_string(), None))?;
        if let Some(h) = self.shared.hwnd.lock().unwrap().as_ref() {
            let _ = unsafe { PostMessageW(h.get(), WM_APP, WPARAM(0), LPARAM(0)) };
        }
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|e| AppError::module("WIN_HOTKEY_005", e.to_string(), None))?
    }

    fn unregister(&self, hotkey_id: i32) -> Result<(), AppError> {
        let (tx, rx) = channel();
        self._req_tx
            .send(Req::Unregister { id: hotkey_id, resp: tx })
            .map_err(|e| AppError::module("WIN_HOTKEY_004", e.to_string(), None))?;
        if let Some(h) = self.shared.hwnd.lock().unwrap().as_ref() {
            let _ = unsafe { PostMessageW(h.get(), WM_APP, WPARAM(0), LPARAM(0)) };
        }
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|e| AppError::module("WIN_HOTKEY_005", e.to_string(), None))?
    }

    fn set_dispatcher(&self, dispatcher: Arc<dyn Fn(i32) + Send + Sync>) {
        if let Ok(mut d) = self.shared.dispatcher.lock() {
            *d = Some(dispatcher);
        }
    }
}
