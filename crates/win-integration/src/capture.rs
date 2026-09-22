//! 屏幕捕获（docs/impl/03 P2）：GDI BitBlt 全屏/区域 + PrintWindow 窗口回退。
//! Windows.Graphics.Capture 在后续迭代替换（帧率敏感的录屏才需要）。

use std::sync::Arc;

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDCW, DeleteDC, DeleteObject,
    GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HGDIOBJ, SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetSystemMetrics, GetWindowRect, GetWindowTextW, IsIconic, IsWindowVisible,
    PW_RENDERFULLCONTENT, SM_CXSCREEN, SM_CYSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use host_core::error::AppError;
use host_core::ports::{CapturePort, CaptureTarget, Frame, MonitorInfo, WindowTarget};

use host_core::util::app_err as err;

/// 黑帧检测（docs/impl/03 P2 潜在问题 3）：采样 16 点全 0 → 受保护窗口
fn black_frame_hint(frame: &Frame) -> bool {
    let w = frame.width as usize;
    let h = frame.height as usize;
    if w == 0 || h == 0 {
        return true;
    }
    let src = frame.bgra.as_ref();
    for i in 0..16usize {
        let fx = ((i % 4 + 1) * w / 5).min(w - 1);
        let fy = ((i / 4 + 1) * h / 5).min(h - 1);
        let off = (fy * w + fx) * 4;
        if src[off] != 0 || src[off + 1] != 0 || src[off + 2] != 0 {
            return false;
        }
    }
    true
}

/// 虚拟桌面 bounds（多显示器，坐标可为负）
fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
        )
    }
}

/// GDI BitBlt 抓取屏幕指定区域，返回 top-down BGRA 帧
unsafe fn grab_region(x: i32, y: i32, w: i32, h: i32) -> Result<Frame, AppError> {
    if w <= 0 || h <= 0 {
        return Err(err("SCREENSHOT_CAPTURE_001", "区域尺寸无效"));
    }
    let screen_dc = CreateDCW(windows::core::w!("DISPLAY"), None, None, None);
    if screen_dc.is_invalid() {
        let gle = windows::Win32::Foundation::GetLastError();
        return Err(err(
            "SCREENSHOT_CAPTURE_001",
            format!("CreateDC 失败 (GetLastError={gle:?})"),
        ));
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    let bitmap = CreateCompatibleBitmap(screen_dc, w, h);
    let old = SelectObject(mem_dc, HGDIOBJ(bitmap.0));

    let blit = BitBlt(mem_dc, 0, 0, w, h, screen_dc, x, y, SRCCOPY);
    let mut frame: Option<Frame> = None;
    if blit.is_ok() {
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (w * h * 4) as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(
            mem_dc,
            bitmap,
            0,
            h as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bi,
            DIB_RGB_COLORS,
        );
        if lines == h {
            frame = Some(Frame {
                width: w as u32,
                height: h as u32,
                bgra: Arc::from(pixels.into_boxed_slice()),
                dpi_scale: 1.0,
                monitor_id: 0,
            });
        }
    }
    SelectObject(mem_dc, old);
    let _ = DeleteObject(HGDIOBJ(bitmap.0));
    let _ = DeleteDC(mem_dc);
    ReleaseDC(HWND::default(), screen_dc);
    frame.ok_or_else(|| err("SCREENSHOT_CAPTURE_001", "BitBlt/GetDIBits 失败"))
}

/// PrintWindow 窗口内容抓取（PW_RENDERFULLCONTENT：含 DWM 合成内容，
/// 被遮挡窗口也可抓全，docs/impl/03 P2 回退路径）
unsafe fn grab_window(hwnd: isize) -> Result<Frame, AppError> {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    let mut rect = RECT::default();
    GetWindowRect(hwnd, &mut rect)
        .map_err(|e| err("SCREENSHOT_CAPTURE_003", format!("GetWindowRect 失败: {e}")))?;
    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;
    if w <= 0 || h <= 0 {
        return Err(err("SCREENSHOT_CAPTURE_003", "窗口尺寸无效（最小化？）"));
    }

    let screen_dc = CreateDCW(windows::core::w!("DISPLAY"), None, None, None);
    if screen_dc.is_invalid() {
        let gle = windows::Win32::Foundation::GetLastError();
        return Err(err(
            "SCREENSHOT_CAPTURE_003",
            format!("CreateDC 失败 (GetLastError={gle:?})"),
        ));
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    let bitmap = CreateCompatibleBitmap(screen_dc, w, h);
    let old = SelectObject(mem_dc, HGDIOBJ(bitmap.0));

    let printed = PrintWindow(hwnd, mem_dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT));
    let mut frame: Option<Frame> = None;
    if printed.as_bool() {
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (w * h * 4) as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(
            mem_dc,
            bitmap,
            0,
            h as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bi,
            DIB_RGB_COLORS,
        );
        if lines == h {
            frame = Some(Frame {
                width: w as u32,
                height: h as u32,
                bgra: Arc::from(pixels.into_boxed_slice()),
                dpi_scale: 1.0,
                monitor_id: 0,
            });
        }
    }
    SelectObject(mem_dc, old);
    let _ = DeleteObject(HGDIOBJ(bitmap.0));
    let _ = DeleteDC(mem_dc);
    ReleaseDC(HWND::default(), screen_dc);
    frame.ok_or_else(|| err("SCREENSHOT_CAPTURE_003", "PrintWindow/GetDIBits 失败"))
}

/// 窗口是否进枚举表（纯函数，四格真值表可测，与 Windows API 无关）。
///
/// `minimized` **刻意不作为过滤条件**：最小化窗的 `GetWindowRect` 返回的是系统摆渡坐标
/// （典型 -32000, -32000）配一个正常尺寸，拿尺寸去挡根本挡不住它；而"表里有没有"和
/// "能不能选"是两回事——下拉表把它列出来并打标（用户看得见"有这么个窗口，现在截不了"），
/// 真正拒它的是 `start_capture`（拿到 SCREENSHOT_WINDOW_001 的明说文案，而不是静默无反应）。
/// 塞进这里只会得到一张"莫名少了几行"的表。
fn window_kept(title: &str, w: i32, h: i32, minimized: bool) -> bool {
    let _ = minimized;
    !title.trim().is_empty() && w > 0 && h > 0
}

/// 标题取宽字符（GetWindowTextW 返回写入字符数，不含终止符；失败即 0）
unsafe fn window_title(hwnd: HWND) -> String {
    const TITLE_MAX: usize = 256;
    let mut buf = [0u16; TITLE_MAX];
    let n = GetWindowTextW(hwnd, &mut buf);
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

/// 单个 HWND → 窗口目标（不可见/无标题/零尺寸一律 None）
unsafe fn window_target(hwnd: HWND) -> Option<WindowTarget> {
    if !IsWindowVisible(hwnd).as_bool() {
        return None;
    }
    let mut rect = RECT::default();
    // 取不到矩形（窗口刚关闭）不是错误，是这张表少一行
    GetWindowRect(hwnd, &mut rect).ok()?;
    let title = window_title(hwnd);
    let minimized = IsIconic(hwnd).as_bool();
    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;
    if !window_kept(&title, w, h, minimized) {
        return None;
    }
    Some(WindowTarget {
        hwnd: hwnd.0 as isize as i64,
        title,
        x: rect.left,
        y: rect.top,
        width: w as u32,
        height: h as u32,
        minimized,
    })
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // 唯一调用点是本文件下方的 EnumWindows，LPARAM 恒为该栈上 Vec 的裸指针
    let out = &mut *(lparam.0 as *mut Vec<WindowTarget>);
    if let Some(t) = window_target(hwnd) {
        out.push(t);
    }
    true.into()
}

pub struct GdiCapture;

impl GdiCapture {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GdiCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl CapturePort for GdiCapture {
    fn enumerate_monitors(&self) -> Result<Vec<MonitorInfo>, AppError> {
        // v1：虚拟桌面作为单一逻辑屏（选区覆盖层单窗口方案）
        let (x, y, w, h) = virtual_screen();
        Ok(vec![MonitorInfo {
            id: 0,
            x,
            y,
            width: w as u32,
            height: h as u32,
            dpi_scale: 1.0,
        }])
    }

    fn capture(&self, target: CaptureTarget) -> Result<Frame, AppError> {
        unsafe {
            let frame = match target {
                CaptureTarget::FullScreen { .. } => {
                    let (x, y, w, h) = virtual_screen();
                    grab_region(x, y, w, h)?
                }
                CaptureTarget::Region { monitor: _, rect } => {
                    grab_region(rect.x, rect.y, rect.w, rect.h)?
                }
                CaptureTarget::Window { hwnd } => grab_window(hwnd)?,
            };
            // 受保护窗口黑帧检测（docs/impl/03 P2 潜在问题 3）
            if black_frame_hint(&frame) {
                return Err(AppError::module(
                    "SCREENSHOT_CAPTURE_002",
                    "捕获到黑帧：目标窗口受系统保护",
                    Some("受 DRM 保护的内容无法截取，请改用区域截图"),
                ));
            }
            Ok(frame)
        }
    }

    fn list_windows(&self) -> Vec<WindowTarget> {
        // EnumWindows 只给顶层窗口（子窗口不在表里是对的：它们不是用户的"截取目标"）
        let mut out: Vec<WindowTarget> = Vec::new();
        unsafe {
            match EnumWindows(Some(collect_window), LPARAM(&mut out as *mut _ as isize)) {
                Ok(()) => {}
                Err(e) => {
                    // 枚举失败不是"没有窗口"：清空半截结果，消费侧的引导文案才不撒谎
                    tracing::warn!("EnumWindows 失败，窗口表按空处理: {e}");
                    out.clear();
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::window_kept;

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-4）字面测试名优先于 rustc 命名惯例
    fn listWindows_filtersZeroSizeAndNoTitle() {
        // 正对照：正常窗口进表
        assert!(window_kept("此电脑", 800, 600, false));
        // 最小化**不过滤**：尺寸照常、坐标是系统摆渡值，过滤它只会得到"莫名少几行"的表，
        // 拒它是 start_capture 的活（用户要看到明说的原因）
        assert!(
            window_kept("此电脑", 800, 600, true),
            "最小化窗口须带 minimized 标记进表，而不是凭空消失"
        );
        // 无标题 / 纯空白标题（消息类隐形窗口）不进表
        assert!(!window_kept("", 800, 600, false));
        assert!(!window_kept("  \t ", 800, 600, false));
        // 零/负尺寸不进表（0 宽或 0 高都截不出东西，负值是矩形反转）
        assert!(!window_kept("此电脑", 0, 600, false));
        assert!(!window_kept("此电脑", 800, 0, false));
        assert!(!window_kept("此电脑", -32000, -32000, false));
    }
}
