//! Windows 剪贴板能力：多格式监听 + 回写 + 来源应用识别（docs/impl/02 C3）
//!
//! 读取优先级：CF_UNICODETEXT → CF_DIB（图片）→ CF_HDROP（文件）；
//! 来源应用：GetClipboardOwner → QueryFullProcessImageNameW → 进程文件名。

use std::sync::{Arc, Mutex};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    GetClipboardOwner, OpenClipboard, RegisterClipboardFormatW, RemoveClipboardFormatListener,
    SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::DragQueryFileW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, GetWindowThreadProcessId, PostMessageW, PostQuitMessage, RegisterClassW,
    SetWindowLongPtrW, TranslateMessage, GWLP_USERDATA, HWND_MESSAGE, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_CLIPBOARDUPDATE, WNDCLASSW,
};

use host_core::error::AppError;
use host_core::ports::{ClipContent, ClipboardPort};

/// 回调以 Arc 包装挂到 GWLP_USERDATA（线程内单消费者）
type Cb = Arc<Mutex<Option<Box<dyn Fn(ClipContent, Option<String>) + Send + Sync>>>>;

unsafe fn set_cb(hwnd: HWND, cb: Cb) {
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(Box::new(cb)) as isize);
}

unsafe fn cb_of(hwnd: HWND) -> Option<Cb> {
    let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if raw == 0 {
        return None;
    }
    Some((&*(raw as *const Cb)).clone())
}

/// 自定义停止消息：外部经 PostMessageW 投递，wndproc 收到后结束消息循环（S3 卸载监听）
const WM_STOP_LISTENER: u32 = WM_APP + 7;

/// 窗口类进程内注册一次（重启时复用同一类，避免 RegisterClassW 返回 0 误判失败）
static CLASS_ONCE: std::sync::Once = std::sync::Once::new();

/// 消息循环退出时回收 GWLP_USERDATA 里的回调盒：清空内部 Box<dyn Fn>，
/// 从而释放其捕获的管线 Sender（使 worker 通道收到 Disconnected，双保险）。
unsafe fn release_cb(hwnd: HWND) {
    let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if raw == 0 {
        return;
    }
    let cb: Cb = *Box::from_raw(raw as *mut Cb);
    if let Ok(mut g) = cb.lock() {
        *g = None;
    }
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_STOP_LISTENER {
        // 结束本线程消息循环：GetMessageW 返回 0 → 循环退出 → 线程收尾清理
        PostQuitMessage(0);
        return LRESULT(0);
    }
    if msg == WM_CLIPBOARDUPDATE {
        if let Some(cb) = cb_of(hwnd) {
            let content = read_clipboard_content();
            let app = read_source_app();
            if let Some(content) = content {
                if let Ok(guard) = cb.lock() {
                    if let Some(f) = guard.as_ref() {
                        f(content, app);
                    }
                }
            }
        }
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// 锁定 HGLOBAL 并以切片执行 f
unsafe fn with_global<T>(h: HGLOBAL, f: impl FnOnce(&[u8]) -> Option<T>) -> Option<T> {
    let size = GlobalSize(h);
    let ptr = GlobalLock(h) as *const u8;
    if ptr.is_null() {
        return None;
    }
    let out = f(std::slice::from_raw_parts(ptr, size));
    let _ = GlobalUnlock(h);
    out
}

/// 读取当前剪贴板内容：文本 → DIB 图片 → HDROP 文件
unsafe fn read_clipboard_content() -> Option<ClipContent> {
    if OpenClipboard(HWND::default()).is_err() {
        return None; // 被其它进程占用，本帧放弃
    }
    let out = (|| {
        // ① 文本
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT.0 as u32) {
            if let Some(c) = with_global(HGLOBAL(h.0), |bytes| {
                let wide: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .take_while(|p| p[0] != 0)
                    .map(|p| u16::from_le_bytes([p[0], p[1]]))
                    .collect();
                if wide.is_empty() {
                    return None;
                }
                let text = String::from_utf16_lossy(&wide);
                if text.trim().is_empty() {
                    None
                } else {
                    Some(ClipContent::Text { text, html: None })
                }
            }) {
                return Some(c);
            }
        }
        // ② 图片（DIB：BITMAPINFOHEADER + 像素；v1 存原始 DIB，前端 canvas 解码预览）
        if let Ok(h) = GetClipboardData(CF_DIB.0 as u32) {
            if let Some(c) = with_global(HGLOBAL(h.0), |bytes| {
                if bytes.len() < 40 {
                    return None;
                }
                let width = i32::from_le_bytes(bytes[4..8].try_into().ok()?) as u32;
                let height_raw = i32::from_le_bytes(bytes[8..12].try_into().ok()?);
                let bpp = u16::from_le_bytes(bytes[14..16].try_into().ok()?);
                if width == 0 || bpp < 16 {
                    return None; // 仅支持 16bpp 及以上直接渲染
                }
                Some(ClipContent::Image {
                    format: "dib".into(),
                    width,
                    height: height_raw.unsigned_abs(),
                    bytes: Arc::from(bytes.to_vec().into_boxed_slice()),
                })
            }) {
                return Some(c);
            }
        }
        // ③ 文件列表
        if let Ok(h) = GetClipboardData(CF_HDROP.0 as u32) {
            let hdrop = windows::Win32::UI::Shell::HDROP(h.0);
            let count = DragQueryFileW(hdrop, u32::MAX, None);
            if count > 0 {
                let mut paths = Vec::with_capacity(count as usize);
                for i in 0..count {
                    let len = DragQueryFileW(hdrop, i, None) as usize + 1;
                    let mut buf = vec![0u16; len];
                    DragQueryFileW(hdrop, i, Some(&mut buf));
                    paths.push(std::path::PathBuf::from(
                        String::from_utf16_lossy(&buf)
                            .trim_end_matches('\0')
                            .to_string(),
                    ));
                }
                return Some(ClipContent::Files { paths });
            }
        }
        None
    })();
    let _ = CloseClipboard();
    out
}

/// 来源应用进程名（如 "chrome"）；失败返回 None
unsafe fn read_source_app() -> Option<String> {
    let owner = GetClipboardOwner().ok()?;
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(owner, Some(&mut pid));
    if pid == 0 {
        return None;
    }
    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(
        process,
        PROCESS_NAME_WIN32,
        windows::core::PWSTR(buf.as_mut_ptr()),
        &mut len,
    )
    .is_ok();
    let _ = HANDLE(process.0); // 句柄泄漏可忽略？——不，关闭它
    windows::Win32::Foundation::CloseHandle(process).ok();
    if !ok || len == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    std::path::Path::new(&path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
}

/// 活动监听窗口句柄（消息循环线程创建后登记，stop_listener 取出并发停止消息）。
/// 存 HWND.0 的地址（usize）：HWND 包装裸指针、非 Send，不能进跨线程 Arc<Mutex<..>>。
pub struct WindowsClipboard {
    listener: Arc<Mutex<Option<usize>>>,
}

impl WindowsClipboard {
    pub fn new() -> Self {
        Self {
            listener: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for WindowsClipboard {
    fn default() -> Self {
        Self::new()
    }
}

/// DROPFILES 头：pFiles(4) + POINT(8) + fNC(4) + fWide(4) = 20 字节
const DROPFILES_SIZE: usize = 20;

impl ClipboardPort for WindowsClipboard {
    fn start_listener(
        &self,
        cb: Box<dyn Fn(ClipContent, Option<String>) + Send + Sync>,
    ) -> Result<(), AppError> {
        let cb: Cb = Arc::new(Mutex::new(Some(cb)));
        let err = |m: &str| {
            AppError::module(
                "CLIPBOARD_LISTENER_001",
                format!("剪贴板监听启动失败: {m}"),
                None,
            )
        };
        // 握手：消息循环线程建窗 + 注册监听成功后才返回，确保 self.listener 就绪、
        // 后续 stop_listener 随时可拆（S3 生命周期对称）
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), ()>>();
        let shared = self.listener.clone();
        // 专用 OS 线程：消息循环不能跑在 tokio worker 上
        std::thread::Builder::new()
            .name("clipboard-listener".into())
            .spawn(move || unsafe {
                CLASS_ONCE.call_once(|| {
                    let cname: Vec<u16> = "NexusForgeClipWnd\0".encode_utf16().collect();
                    let wc = WNDCLASSW {
                        lpfnWndProc: Some(wndproc),
                        lpszClassName: PCWSTR(cname.as_ptr()),
                        hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
                        ..Default::default()
                    };
                    RegisterClassW(&wc);
                });
                let name: Vec<u16> = "NexusForgeClipWnd\0".encode_utf16().collect();
                let class_name = PCWSTR(name.as_ptr());
                let hinst = GetModuleHandleW(None).unwrap_or_default();
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
                    hinst,
                    None,
                )
                .unwrap_or_default();
                if hwnd.is_invalid() {
                    let _ = ready_tx.send(Err(()));
                    return;
                }
                set_cb(hwnd, cb);
                if AddClipboardFormatListener(hwnd).is_err() {
                    release_cb(hwnd);
                    let _ = DestroyWindow(hwnd);
                    let _ = ready_tx.send(Err(()));
                    return;
                }
                if let Ok(mut g) = shared.lock() {
                    *g = Some(hwnd.0 as usize);
                }
                let _ = ready_tx.send(Ok(()));
                let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
                while GetMessageW(&mut msg, hwnd, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                // 收尾（S3）：注销监听、清句柄登记、回收回调（释放管线 Sender）、销毁窗口
                let _ = RemoveClipboardFormatListener(hwnd);
                if let Ok(mut g) = shared.lock() {
                    if *g == Some(hwnd.0 as usize) {
                        *g = None;
                    }
                }
                release_cb(hwnd);
                let _ = DestroyWindow(hwnd);
            })
            .map_err(|e| err(&e.to_string()))?;
        ready_rx
            .recv()
            .map_err(|_| err("监听线程提前退出"))?
            .map_err(|_| err("消息窗口创建失败"))?;
        Ok(())
    }

    fn stop_listener(&self) -> Result<(), AppError> {
        let raw = self.listener.lock().ok().and_then(|mut g| g.take());
        if let Some(raw) = raw {
            unsafe {
                // 异步投递：目标线程 wndproc 收到后 PostQuitMessage，消息循环退出并自行收尾
                let _ = PostMessageW(
                    HWND(raw as *mut core::ffi::c_void),
                    WM_STOP_LISTENER,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
        Ok(())
    }

    fn write(&self, content: &ClipContent) -> Result<(), AppError> {
        unsafe {
            OpenClipboard(HWND::default()).map_err(|e| {
                AppError::module(
                    "CLIPBOARD_WRITE_002",
                    format!("OpenClipboard 失败: {e}"),
                    None,
                )
            })?;
            let r = match content {
                ClipContent::Text { text, .. } => write_text(text),
                ClipContent::Files { paths } => write_files(paths),
                ClipContent::Image { format, bytes, .. } if format == "dib" => write_dib(bytes),
                ClipContent::Image {
                    format: _, bytes, ..
                } => {
                    // Port 契约：png 等编码由 win-integration 转为系统 DIB（CF_DIB 32bpp）
                    match png_to_dib(bytes) {
                        Ok(dib) => write_dib(&dib),
                        Err(e) => Err(e),
                    }
                }
            };
            let _ = CloseClipboard();
            r
        }
    }
}

/// PNG → CF_DIB（BITMAPINFOHEADER 40 字节 + 32bpp BGRA bottom-up，alpha 置 255）
fn png_to_dib(png: &[u8]) -> Result<Vec<u8>, AppError> {
    let img = image::load_from_memory(png)
        .map_err(|e| AppError::module("CLIPBOARD_WRITE_001", format!("PNG 解码失败: {e}"), None))?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let src = rgba.into_raw();
    let mut out = Vec::with_capacity(40 + w * h * 4);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(w as u32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(h as u32).to_le_bytes()); // biHeight 正数 = bottom-up
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&((w * h * 4) as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0u32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
                                                // bottom-up：从最后一行开始；BGRA 字节序 + alpha 强制 255（GDI DIB 无有效 alpha）
    for row in (0..h).rev() {
        for px in 0..w {
            let off = (row * w + px) * 4;
            out.push(src[off + 2]); // B
            out.push(src[off + 1]); // G
            out.push(src[off]); // R
            out.push(255);
        }
    }
    Ok(out)
}

unsafe fn write_text(text: &str) -> Result<(), AppError> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).map_err(|e| {
        AppError::module(
            "CLIPBOARD_WRITE_004",
            format!("GlobalAlloc 失败: {e}"),
            None,
        )
    })?;
    let ptr = GlobalLock(h) as *mut u16;
    if ptr.is_null() {
        return Err(AppError::module(
            "CLIPBOARD_WRITE_005",
            "GlobalLock 失败",
            None,
        ));
    }
    std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
    let _ = GlobalUnlock(h);
    EmptyClipboard().map_err(|e| {
        AppError::module(
            "CLIPBOARD_WRITE_003",
            format!("EmptyClipboard 失败: {e}"),
            None,
        )
    })?;
    SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(h.0)).map_err(|e| {
        AppError::module(
            "CLIPBOARD_WRITE_006",
            format!("SetClipboardData 失败: {e}"),
            None,
        )
    })?;
    Ok(())
}

unsafe fn write_files(paths: &[std::path::PathBuf]) -> Result<(), AppError> {
    let mut wide: Vec<u16> = Vec::new();
    for p in paths {
        wide.extend(p.as_os_str().to_string_lossy().encode_utf16());
        wide.push(0);
    }
    wide.push(0); // 双 null 结尾
    let total = DROPFILES_SIZE + wide.len() * 2;
    let h = GlobalAlloc(GMEM_MOVEABLE, total).map_err(|e| {
        AppError::module(
            "CLIPBOARD_WRITE_004",
            format!("GlobalAlloc 失败: {e}"),
            None,
        )
    })?;
    let ptr = GlobalLock(h) as *mut u8;
    if ptr.is_null() {
        return Err(AppError::module(
            "CLIPBOARD_WRITE_005",
            "GlobalLock 失败",
            None,
        ));
    }
    let slice = std::slice::from_raw_parts_mut(ptr, total);
    std::ptr::write_bytes(ptr, 0, total);
    // pFiles = 20（数据区偏移）；fWide = 1（宽字符）
    slice[0..4].copy_from_slice(&(DROPFILES_SIZE as u32).to_le_bytes());
    slice[16..20].copy_from_slice(&1u32.to_le_bytes());
    for (i, w) in wide.iter().enumerate() {
        std::ptr::write_unaligned(
            slice.as_mut_ptr().add(DROPFILES_SIZE + i * 2) as *mut u16,
            *w,
        );
    }
    let _ = GlobalUnlock(h);
    EmptyClipboard().map_err(|e| {
        AppError::module(
            "CLIPBOARD_WRITE_003",
            format!("EmptyClipboard 失败: {e}"),
            None,
        )
    })?;
    SetClipboardData(CF_HDROP.0 as u32, HANDLE(h.0)).map_err(|e| {
        AppError::module(
            "CLIPBOARD_WRITE_006",
            format!("SetClipboardData 失败: {e}"),
            None,
        )
    })?;
    Ok(())
}

unsafe fn write_dib(bytes: &[u8]) -> Result<(), AppError> {
    let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|e| {
        AppError::module(
            "CLIPBOARD_WRITE_004",
            format!("GlobalAlloc 失败: {e}"),
            None,
        )
    })?;
    let ptr = GlobalLock(h) as *mut u8;
    if ptr.is_null() {
        return Err(AppError::module(
            "CLIPBOARD_WRITE_005",
            "GlobalLock 失败",
            None,
        ));
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
    let _ = GlobalUnlock(h);
    EmptyClipboard().map_err(|e| {
        AppError::module(
            "CLIPBOARD_WRITE_003",
            format!("EmptyClipboard 失败: {e}"),
            None,
        )
    })?;
    SetClipboardData(CF_DIB.0 as u32, HANDLE(h.0)).map_err(|e| {
        AppError::module(
            "CLIPBOARD_WRITE_006",
            format!("SetClipboardData 失败: {e}"),
            None,
        )
    })?;
    Ok(())
}

/// 注册剪贴板自定义格式（预留：origin 标记用）
pub fn register_custom_format(name: &str) -> Option<u32> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) }.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_to_dib_header_and_pixel_layout() {
        use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};
        // 2x1 的 PNG：单行两像素
        let mut buf = std::io::Cursor::new(Vec::new());
        PngEncoder::new(&mut buf)
            .write_image(
                &[1, 2, 3, 255, 4, 5, 6, 255],
                2,
                1,
                ExtendedColorType::Rgba8,
            )
            .unwrap();
        let dib = png_to_dib(&buf.into_inner()).unwrap();
        assert_eq!(
            &dib[0..16],
            &[
                40, 0, 0, 0, // biSize=40
                2, 0, 0, 0, // width=2
                1, 0, 0, 0, // height=1
                1, 0, // planes
                32, 0, // bpp
            ]
        );
        // 唯一一行像素：BGR + alpha 255（GDI 无有效 alpha）
        assert_eq!(&dib[40..44], &[3, 2, 1, 255]);
        assert_eq!(&dib[44..48], &[6, 5, 4, 255]);
    }
}
