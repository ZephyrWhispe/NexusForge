//! Shell 缩略图（ThumbPort）与回收站删除（RecycleBinPort）。
//!
//! - 缩略图：IShellItemImageFactory::GetImage（视频/PDF/办公文档系统缩略图）
//! - 回收站：SHFileOperationW + FOF_ALLOWUNDO（docs/impl/05 F 风险标注）

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use host_core::error::AppError;
use host_core::ports::{RecycleBinPort, ThumbPort};
use image::RgbaImage;
use windows::core::Interface;
use windows::core::PCWSTR;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP,
    BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SHFileOperationW, FOF_ALLOWUNDO,
    FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SIIGBF_RESIZETOFIT,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 系统 Shell 缩略图（win-integration 唯一 windows 依赖层的本文件实现）
pub struct ShellThumb;

impl ThumbPort for ShellThumb {
    fn thumbnail(&self, path: &Path, px: u32) -> Result<(u32, u32, Vec<u8>), AppError> {
        let wide_path = wide(&path.to_string_lossy());
        unsafe {
            let item: windows::Win32::UI::Shell::IShellItem =
                SHCreateItemFromParsingName(PCWSTR(wide_path.as_ptr()), None)
            .map_err(|e| AppError::module("FILE_PREVIEW_002", e.to_string(), None))?;
            let factory: IShellItemImageFactory = item
                .cast()
                .map_err(|e| AppError::module("FILE_PREVIEW_002", e.to_string(), None))?;
            let hbmp: HBITMAP = factory
                .GetImage(SIZE { cx: px as i32, cy: px as i32 }, SIIGBF_RESIZETOFIT)
                .map_err(|e| AppError::module("FILE_PREVIEW_002", e.to_string(), None))?;

            let png = hbitmap_to_png(hbmp);
            let _ = DeleteObject(hbmp);
            let (w, h, bytes) = png?;
            Ok((w, h, bytes))
        }
    }
}

/// HBITMAP 32bpp BGRA → (w, h, PNG 字节)
fn hbitmap_to_png(hbmp: HBITMAP) -> Result<(u32, u32, Vec<u8>), AppError> {
    unsafe {
        let mut bm = BITMAP::default();
        let n = GetObjectW(
            hbmp,
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut _),
        );
        if n == 0 {
            return Err(AppError::module("FILE_PREVIEW_003", "GetObjectW 失败", None));
        }
        let (w, h) = (bm.bmWidth.max(0) as i32, bm.bmHeight.max(0) as i32);

        let mut bi = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // 负高度 = top-down
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            ..Default::default()
        };
        let buf_size = (w as usize) * (h as usize) * 4;
        let mut buf = vec![0u8; buf_size];
        let hdc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            hdc,
            hbmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bi as *mut BITMAPINFOHEADER as *mut BITMAPINFO,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(hdc);
        if lines == 0 {
            return Err(AppError::module("FILE_PREVIEW_003", "GetDIBits 失败", None));
        }
        // BGRA → RGBA（image crate RgbaImage 期望 RGBA）
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let img = RgbaImage::from_raw(w as u32, h as u32, buf)
            .ok_or_else(|| AppError::module("FILE_PREVIEW_003", "像素缓冲不完整", None))?;
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png)
            .map_err(|e| AppError::module("FILE_PREVIEW_003", e.to_string(), None))?;
        Ok((w as u32, h as u32, png.into_inner()))
    }
}

/// 回收站删除（SHFileOperationW FOF_ALLOWUNDO）
pub struct RecycleBin;

impl RecycleBinPort for RecycleBin {
    fn delete(&self, paths: &[PathBuf]) -> Result<u32, AppError> {
        // pFrom 要求双 NUL 结尾的多串列表
        let mut list: Vec<u16> = Vec::new();
        for p in paths {
            list.extend(p.as_os_str().encode_wide());
            list.push(0);
        }
        list.push(0);
        let mut op = SHFILEOPSTRUCTW {
            hwnd: windows::Win32::Foundation::HWND::default(),
            wFunc: 3, // FO_DELETE
            pFrom: PCWSTR(list.as_ptr()),
            pTo: PCWSTR::null(),
            fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_NOERRORUI.0 | FOF_SILENT.0) as u16,
            fAnyOperationsAborted: false.into(),
            hNameMappings: std::ptr::null_mut(),
            lpszProgressTitle: PCWSTR::null(),
        };
        let hr = unsafe { SHFileOperationW(&mut op) };
        if hr != 0 {
            return Err(AppError::module(
                "FILE_OPS_005",
                format!("回收站删除失败（SHFileOperationW={hr}）"),
                Some("文件可能被占用或受系统保护，可改用永久删除"),
            ));
        }
        if op.fAnyOperationsAborted.as_bool() {
            return Err(AppError::module("FILE_OPS_006", "回收站删除被中止", None));
        }
        Ok(paths.len() as u32)
    }
}

/// Shell 启动（ShellExecuteW，desktop-core D1 启动器经 ShellPort 使用）
pub struct ShellOps;

impl host_core::ports::ShellPort for ShellOps {
    fn shell_execute(&self, path: &str) -> Result<(), AppError> {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{w, HSTRING};
        let file = HSTRING::from(path);
        let h = unsafe { ShellExecuteW(None, w!("open"), &file, None, None, SW_SHOWNORMAL) };
        // SE_ERR 约定：返回值 > 32 成功
        let code = h.0 as isize;
        if code <= 32 {
            return Err(AppError::module(
                "DESKTOP_LAUNCH_001",
                format!("ShellExecuteW 失败（SE_ERR={code}）: {path}"),
                Some("目标可能不存在、被占用或需要管理员权限"),
            ));
        }
        Ok(())
    }
}
