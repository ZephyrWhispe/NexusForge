//! 屏幕捕获（docs/impl/03 P2）：GDI BitBlt 全屏/区域 + PrintWindow 窗口回退。
//! Windows.Graphics.Capture 在后续迭代替换（帧率敏感的录屏才需要）。

use std::sync::Arc;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDCW, DeleteDC, DeleteObject,
    GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HGDIOBJ, SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, GetWindowRect, SM_CXSCREEN, SM_CYSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, PW_RENDERFULLCONTENT,
};

use host_core::error::AppError;
use host_core::ports::{CapturePort, CaptureTarget, Frame, MonitorInfo};

fn err(code: &str, m: impl std::fmt::Display) -> AppError {
    AppError::module(code, m.to_string(), None)
}

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
        return Err(err("SCREENSHOT_CAPTURE_001", format!("CreateDC 失败 (GetLastError={gle:?})")));
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    let bitmap = CreateCompatibleBitmap(screen_dc, w, h);
    let old = SelectObject(mem_dc, HGDIOBJ(bitmap.0));

    let blit = BitBlt(mem_dc, 0, 0, w, h, screen_dc, x, y, SRCCOPY);
    let mut frame: Option<Frame> = None;
    if blit.is_ok() {
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (w * h * 4) as u32,
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
        return Err(err("SCREENSHOT_CAPTURE_003", format!("CreateDC 失败 (GetLastError={gle:?})")));
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    let bitmap = CreateCompatibleBitmap(screen_dc, w, h);
    let old = SelectObject(mem_dc, HGDIOBJ(bitmap.0));

    let printed = PrintWindow(hwnd, mem_dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT));
    let mut frame: Option<Frame> = None;
    if printed.as_bool() {
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (w * h * 4) as u32,
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
}
