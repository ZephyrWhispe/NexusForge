//! F4 预览服务（docs/impl/05 F4）：文本片段 / 图片缩略图 / 视频等系统缩略图。
//!
//! - 图片：image crate 解码 → 等比缩到 thumb_px → PNG data URL（前端 <img> 直用）
//! - 视频/文档等：走 [`ThumbPort`]（win-integration IShellItemImageFactory 系统缩略图）
//! - 文本：前 max_text 字节，含 NUL 视为二进制 → 尝试系统缩略图，失败归 Unsupported

use std::path::Path;

use base64::Engine;
use serde::{Deserialize, Serialize};

use host_core::ports::ThumbPort;

use crate::browse::to_long_path;
use crate::error::FileError;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Preview {
    /// 文本片段（truncated 截断提示）
    Text { content: String, truncated: bool },
    /// 图片缩略图（PNG data URL）
    Image { data_url: String, width: u32, height: u32 },
    /// 系统 Shell 缩略图（视频/PDF 等）
    Shell { data_url: String, width: u32, height: u32 },
    /// 无法预览（原因展示）
    Unsupported { reason: String },
}

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff"];
/// 系统缩略图渠道优先的扩展名（视频/办公文档/PDF）
const SHELL_EXTS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "wmv", "flv", "m4v",
    "mp3", "wav", "flac", "ogg", "m4a",
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
];

pub fn preview_file(
    path: &Path,
    thumb: Option<&dyn ThumbPort>,
    thumb_px: u32,
    max_text: usize,
) -> Result<Preview, FileError> {
    let long = to_long_path(path);
    if !long.exists() {
        return Err(FileError::NotFound(
            crate::browse::display_path(path).to_string_lossy().into_owned(),
        ));
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if IMAGE_EXTS.contains(&ext.as_str()) {
        return image_preview(&long, thumb_px);
    }
    if SHELL_EXTS.contains(&ext.as_str()) {
        if let Some(port) = thumb {
            return shell_preview(port, path);
        }
        return Ok(Preview::Unsupported { reason: "系统缩略图服务未就绪".into() });
    }
    // 其余按文本尝试（含无扩展名；目录已由上层过滤）
    text_preview(&long, max_text)
}

fn image_preview(long: &Path, thumb_px: u32) -> Result<Preview, FileError> {
    let img = image::open(long).map_err(|e| FileError::Preview(e.to_string()))?;
    let (w, h) = (img.width(), img.height());
    let thumb = if w.max(h) > thumb_px { img.thumbnail(thumb_px, thumb_px) } else { img };
    let mut png = std::io::Cursor::new(Vec::new());
    thumb
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| FileError::Preview(e.to_string()))?;
    Ok(Preview::Image {
        data_url: to_data_url(png.get_ref()),
        width: w,
        height: h,
    })
}

fn shell_preview(port: &dyn ThumbPort, path: &Path) -> Result<Preview, FileError> {
    let (w, h, png) = port
        .thumbnail(&crate::browse::display_path(path), 256)
        .map_err(|e| FileError::Preview(e.to_string()))?;
    Ok(Preview::Shell { data_url: to_data_url(&png), width: w, height: h })
}

fn text_preview(long: &Path, max_text: usize) -> Result<Preview, FileError> {
    let size = std::fs::metadata(long).map_err(FileError::from)?.len();
    let take = (max_text as u64).min(size) as usize;
    let mut f = std::fs::File::open(long)?;
    use std::io::Read;
    let mut buf = vec![0u8; take];
    f.read_exact(&mut buf).map_err(|e| FileError::Preview(e.to_string()))?;
    if buf.contains(&0) {
        return Ok(Preview::Unsupported { reason: "二进制文件不支持文本预览".into() });
    }
    let content = String::from_utf8_lossy(&buf).to_string();
    let truncated = size > buf.len() as u64;
    // UTF-8 尾部截断修复：from_utf8_lossy 把不完整字符替换为 U+FFFD，丢掉即可
    let mut content = content;
    if truncated {
        while content.ends_with('\u{FFFD}') {
            content.pop();
        }
    }
    Ok(Preview::Text { content, truncated })
}

fn to_data_url(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nf_file_preview_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn text_preview_reads_prefix_and_flags_truncation() {
        let d = tmpdir("text");
        let long_text = "甲乙丙丁".repeat(1000);
        std::fs::write(d.join("t.txt"), &long_text).unwrap();
        let p = preview_file(&d.join("t.txt"), None, 256, 64).unwrap();
        match p {
            Preview::Text { content, truncated } => {
                assert!(truncated);
                assert!(content.chars().count() <= 64);
            }
            other => panic!("期望 Text，实际 {other:?}"),
        }
        // 小文件不截断
        std::fs::write(d.join("s.txt"), "hello").unwrap();
        match preview_file(&d.join("s.txt"), None, 256, 64).unwrap() {
            Preview::Text { content, truncated } => {
                assert_eq!(content, "hello");
                assert!(!truncated);
            }
            other => panic!("期望 Text，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn binary_falls_back_to_unsupported() {
        let d = tmpdir("bin");
        std::fs::write(d.join("b.dat"), vec![0u8, 1, 2, 3]).unwrap();
        match preview_file(&d.join("b.dat"), None, 256, 64).unwrap() {
            Preview::Unsupported { .. } => {}
            other => panic!("期望 Unsupported，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn image_preview_downscales_to_thumb() {
        let d = tmpdir("img");
        let img = image::DynamicImage::new_rgb8(1024, 512);
        img.save(d.join("pic.png")).unwrap();
        match preview_file(&d.join("pic.png"), None, 256, 64).unwrap() {
            Preview::Image { data_url, width, height } => {
                assert_eq!(width, 1024);
                assert_eq!(height, 512);
                assert!(data_url.starts_with("data:image/png;base64,"));
            }
            other => panic!("期望 Image，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn shell_ext_without_port_reports_unsupported() {
        let d = tmpdir("shell");
        std::fs::write(d.join("v.mp4"), b"\x00\x00").unwrap();
        match preview_file(&d.join("v.mp4"), None, 256, 64).unwrap() {
            Preview::Unsupported { reason } => assert!(reason.contains("缩略图")),
            other => panic!("期望 Unsupported，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_file_errors() {
        let d = tmpdir("miss");
        assert!(matches!(
            preview_file(&d.join("nope.txt"), None, 256, 64),
            Err(FileError::NotFound(_))
        ));
        let _ = std::fs::remove_dir_all(&d);
    }
}
