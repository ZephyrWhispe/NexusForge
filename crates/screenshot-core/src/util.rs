//! 像素与编码工具（docs/impl/03 P2）
//!
//! 关键规约：GDI BitBlt 得到的 32bpp 帧 alpha 通道无意义（常为 0），
//! 转 RGBA 时一律置 255，否则保存的 PNG 整图透明。

use base64::Engine;
use host_core::error::AppError;
use host_core::ports::{Frame, Rect};

fn err(code: &str, m: impl std::fmt::Display) -> AppError {
    AppError::module(code, m.to_string(), None)
}

/// BGRA 帧 → RGBA 字节（alpha 强制 255）
pub fn bgra_to_rgba(frame: &Frame) -> Vec<u8> {
    let pixels = frame.width as usize * frame.height as usize;
    let src = frame.bgra.as_ref();
    let mut out = vec![255u8; pixels * 4];
    for i in 0..pixels {
        out[i * 4] = src[i * 4 + 2];
        out[i * 4 + 1] = src[i * 4 + 1];
        out[i * 4 + 2] = src[i * 4];
    }
    out
}

/// 从帧内裁剪区域（帧相对坐标，物理像素）→ RGBA 字节
pub fn crop_bgra(frame: &Frame, rect: Rect<i32>) -> Result<(u32, u32, Vec<u8>), AppError> {
    let fw = frame.width as i32;
    let fh = frame.height as i32;
    // 与帧边界求交，避免越界 panic
    let x0 = rect.x.clamp(0, fw);
    let y0 = rect.y.clamp(0, fh);
    let x1 = (rect.x + rect.w).clamp(0, fw);
    let y1 = (rect.y + rect.h).clamp(0, fh);
    if x1 - x0 <= 0 || y1 - y0 <= 0 {
        return Err(err("SCREENSHOT_CONFIRM_001", "裁剪区域为空"));
    }
    let w = (x1 - x0) as usize;
    let h = (y1 - y0) as usize;
    let src = frame.bgra.as_ref();
    let mut out = vec![255u8; w * h * 4];
    for row in 0..h {
        let src_off = (((y0 as usize) + row) * fw as usize + x0 as usize) * 4;
        let dst_off = row * w * 4;
        for px in 0..w {
            let s = src_off + px * 4;
            let d = dst_off + px * 4;
            out[d] = src[s + 2];
            out[d + 1] = src[s + 1];
            out[d + 2] = src[s];
        }
    }
    Ok((w as u32, h as u32, out))
}

/// RGBA → PNG（Base64）
pub fn encode_png_b64(width: u32, height: u32, rgba: &[u8]) -> Result<String, AppError> {
    use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};
    let mut buf = std::io::Cursor::new(Vec::new());
    PngEncoder::new(&mut buf)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .map_err(|e| err("SCREENSHOT_ENCODE_001", format!("PNG 编码失败: {e}")))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(buf.into_inner()))
}

/// PNG（Base64）→ RGBA 字节
pub fn decode_png_b64(b64: &str) -> Result<(u32, u32, Vec<u8>), AppError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| err("SCREENSHOT_DECODE_001", format!("Base64 解码失败: {e}")))?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| err("SCREENSHOT_DECODE_002", format!("PNG 解码失败: {e}")))?;
    let rgba = img.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// 黑帧检测（docs/impl/03 P2 潜在问题 3）：采样 16 点全 0 → 受保护窗口
pub fn is_black_frame(frame: &Frame) -> bool {
    let w = frame.width as usize;
    let h = frame.height as usize;
    if w == 0 || h == 0 {
        return true;
    }
    let src = frame.bgra.as_ref();
    for i in 0..16usize {
        let fx = (i % 4 + 1) * w / 5;
        let fy = (i / 4 + 1) * h / 5;
        let off = (fy.min(h - 1) * w + fx.min(w - 1)) * 4;
        if src[off] != 0 || src[off + 1] != 0 || src[off + 2] != 0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn frame(w: u32, h: u32) -> Frame {
        Frame {
            width: w,
            height: h,
            bgra: Arc::from(vec![0u8; (w * h * 4) as usize].into_boxed_slice()),
            dpi_scale: 1.0,
            monitor_id: 0,
        }
    }

    /// BGRA 帧内 (x,y) 像素涂色（rgb 输入，内部转 BGRA 存储）
    fn painted(w: u32, h: u32, x: u32, y: u32, rgb: [u8; 3]) -> Frame {
        let mut f = frame(w, h);
        let mut data = f.bgra.as_ref().to_vec();
        let off = (y as usize * w as usize + x as usize) * 4;
        data[off] = rgb[2];
        data[off + 1] = rgb[1];
        data[off + 2] = rgb[0];
        f.bgra = Arc::from(data.into_boxed_slice());
        f
    }

    #[test]
    fn bgra_to_rgba_swizzles_and_forces_alpha() {
        let f = painted(2, 1, 0, 0, [1, 2, 3]);
        let rgba = bgra_to_rgba(&f);
        assert_eq!(rgba[0], 1);
        assert_eq!(rgba[1], 2);
        assert_eq!(rgba[2], 3);
        assert_eq!(rgba[3], 255); // GDI alpha=0 → 强制 255
    }

    #[test]
    fn crop_clamps_and_swizzles() {
        let f = painted(4, 4, 1, 1, [10, 20, 30]);
        let (w, h, rgba) = crop_bgra(&f, Rect { x: 1, y: 1, w: 2, h: 2 }).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(&rgba[0..3], &[10, 20, 30]);
        assert_eq!(rgba[3], 255);
        // 越界裁剪：与帧边界求交
        let (w, h, _) = crop_bgra(&f, Rect { x: 3, y: 3, w: 10, h: 10 }).unwrap();
        assert_eq!((w, h), (1, 1));
        // 空区域报错
        assert!(crop_bgra(&f, Rect { x: 0, y: 0, w: 0, h: 5 }).is_err());
    }

    #[test]
    fn png_roundtrip() {
        let rgba = vec![128u8; 3 * 2 * 4];
        let b64 = encode_png_b64(3, 2, &rgba).unwrap();
        let (w, h, out) = decode_png_b64(&b64).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(out, rgba);
    }

    #[test]
    fn black_frame_detection() {
        assert!(is_black_frame(&frame(16, 16)));
        // 采样点：fx,fy ∈ {3,6,9,12}（w=h=16），涂色到 (9,9) 确保被采样
        let f = painted(16, 16, 9, 9, [1, 1, 1]);
        assert!(!is_black_frame(&f));
    }
}
