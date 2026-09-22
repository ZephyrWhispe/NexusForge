//! 像素与编码工具（docs/impl/03 P2）
//!
//! 关键规约：GDI BitBlt 得到的 32bpp 帧 alpha 通道无意义（常为 0），
//! 转 RGBA 时一律置 255，否则保存的 PNG 整图透明。

use host_core::error::AppError;
use host_core::ports::{Frame, Rect};

use host_core::util::app_err as err;

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

/// RGBA → PNG（Base64）：`encode_rgba` 之上的薄封装（既有用点零 churn）
pub fn encode_png_b64(width: u32, height: u32, rgba: &[u8]) -> Result<String, AppError> {
    let (bytes, _) = encode_rgba(EncodeFormat::Png, 80, width, height, rgba)?;
    Ok(host_core::util::b64_encode(&bytes))
}

/// 写侧导出格式（D-29 B4 T-B4-7）：全仓磁盘编码只认这三枚
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeFormat {
    Png,
    Jpeg,
    WebP,
}

/// 可用集合的唯一文案真源（拒绝消息与 schema enum 同源，两处各写必漂）
pub const ENCODE_FORMATS: [&str; 3] = ["png", "jpeg", "webp"];

/// JPEG 压平用的白底常量（调用点恒传此值；写成参数而不是藏进函数，
/// 是为了让"半透明像素会被合成成什么"在读取处就能看到）
pub const JPEG_BG_RGB: [u8; 3] = [255, 255, 255];

impl EncodeFormat {
    /// 大小写不敏感解析；未知值点名收到的值与可用集合（不静默回落 png：
    /// "配置写错了"与"配置没写"必须是两种可观察的结果）
    pub fn from_str_honest(s: &str) -> Result<Self, AppError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "png" => Ok(Self::Png),
            "jpeg" => Ok(Self::Jpeg),
            "webp" => Ok(Self::WebP),
            other => Err(err(
                "SCREENSHOT_FORMAT_001",
                format!(
                    "未知导出格式 {other:?}，可用：{}",
                    ENCODE_FORMATS.join(" | ")
                ),
            )),
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebP => "webp",
        }
    }

    /// MIME 名（与 `ext` 分野：扩展名 "jpg" 对 MIME "image/jpeg"，两者不是同一个词表，
    /// 混用会写出 `shot.jpeg` 或 `data:image/jpg` 这类看起来对、实际错的东西）
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::WebP => "image/webp",
        }
    }

    /// 本格式是否消费 quality（PNG 无损、WebP 在本代只有 VP8L 无损档）
    pub fn honours_quality(self) -> bool {
        matches!(self, Self::Jpeg)
    }
}

/// 按魔数嗅探编码 MIME（`ShotItem.file` 后缀自 T-B4-7 起不再恒 `.png`，读侧只能信内容）。
/// 与 `EncodeFormat::content_type` 同词表；未知一律 "unknown" 而不是猜一个 png。
pub fn sniff_content_type(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else if bytes.starts_with(b"\xFF\xD8\xFF") {
        "image/jpeg"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "unknown"
    }
}

/// RGBA → 指定格式字节，返回 (bytes, 扩展名)。**全仓唯一写侧编码入口**。
///
/// quality 只在 JPEG 上有效（钳位 1..=100 后传给编码器）；PNG/WebP 走无损，
/// 传什么都产出同一字节流——这不是遗漏，是 `image` 0.25 WebP 编码器只有
/// VP8L 的事实（`EncodeFormat::honours_quality` 把这个事实编码成可查询的一面）。
pub fn encode_rgba(
    fmt: EncodeFormat,
    quality: u8,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(Vec<u8>, &'static str), AppError> {
    use image::{
        codecs::{jpeg::JpegEncoder, png::PngEncoder, webp::WebPEncoder},
        ExtendedColorType, ImageEncoder,
    };
    let ext = fmt.ext();
    let mut buf = std::io::Cursor::new(Vec::new());
    let encoded = match fmt {
        EncodeFormat::Png => {
            PngEncoder::new(&mut buf).write_image(rgba, width, height, ExtendedColorType::Rgba8)
        }
        EncodeFormat::Jpeg => {
            let q = quality.clamp(1, 100);
            let rgb = rgba_to_rgb_over(rgba, JPEG_BG_RGB);
            JpegEncoder::new_with_quality(&mut buf, q).write_image(
                &rgb,
                width,
                height,
                ExtendedColorType::Rgb8,
            )
        }
        EncodeFormat::WebP => WebPEncoder::new_lossless(&mut buf).write_image(
            rgba,
            width,
            height,
            ExtendedColorType::Rgba8,
        ),
    };
    encoded.map_err(|e| err("SCREENSHOT_ENCODE_001", format!("{ext} 编码失败: {e}")))?;
    Ok((buf.into_inner(), ext))
}

/// RGBA → RGB（alpha 合成到指定底色）：JPEG 无 alpha 通道，"丢弃"与"合成"
/// 是两个语义，这里显式选后者（半透明像素按 a/255 加权，a=0 时完全取底色）
pub fn rgba_to_rgb_over(rgba: &[u8], bg: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() / 4 * 3);
    for px in rgba.as_chunks::<4>().0 {
        let a = px[3] as u32;
        for c in 0..3 {
            let blended = (px[c] as u32 * a + bg[c] as u32 * (255 - a)) / 255;
            out.push(blended as u8);
        }
    }
    out
}

/// 文件名解析：`{ts}` 换时间戳、`{fmt}` 换实际扩展名；未知占位符原样留字面
/// （吞掉它等于把用户的模板写错伪装成写对——留字面才看得见）
pub fn resolve_filename(template: &str, ts: &str, fmt: EncodeFormat) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        match rest[i..].find('}') {
            Some(j) => {
                match &rest[i + 1..i + j] {
                    "ts" => out.push_str(ts),
                    "fmt" => out.push_str(fmt.ext()),
                    // 未知占位符连同花括号原样保留（other 未消费正是重点：吞掉它就丢了信息）
                    _ => out.push_str(&rest[i..i + j + 1]),
                }
                rest = &rest[i + j + 1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// PNG（Base64）→ RGBA 字节
pub fn decode_png_b64(b64: &str) -> Result<(u32, u32, Vec<u8>), AppError> {
    let bytes = host_core::util::b64_decode(b64.trim())
        .ok_or_else(|| err("SCREENSHOT_DECODE_001", "Base64 解码失败"))?;
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

    /// 4×1 半透明条纹：(0) 不透明红、(1) 半透明绿、(2) 全透明蓝、(3) 不透明白
    fn rgba_strip() -> Vec<u8> {
        vec![
            255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 255, 255, 255, 255,
        ]
    }

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
        let (w, h, rgba) = crop_bgra(
            &f,
            Rect {
                x: 1,
                y: 1,
                w: 2,
                h: 2,
            },
        )
        .unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(&rgba[0..3], &[10, 20, 30]);
        assert_eq!(rgba[3], 255);
        // 越界裁剪：与帧边界求交
        let (w, h, _) = crop_bgra(
            &f,
            Rect {
                x: 3,
                y: 3,
                w: 10,
                h: 10,
            },
        )
        .unwrap();
        assert_eq!((w, h), (1, 1));
        // 空区域报错
        assert!(crop_bgra(
            &f,
            Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 5
            }
        )
        .is_err());
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

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-7）字面测试名优先于 rustc 命名惯例
    fn encodeRgba_threeFormats_extensionsAndMagicBytes() {
        let rgba = rgba_strip();
        let cases = [
            (EncodeFormat::Png, "png", b"\x89PNG".as_slice()),
            (EncodeFormat::Jpeg, "jpg", b"\xFF\xD8\xFF".as_slice()),
            (EncodeFormat::WebP, "webp", b"RIFF".as_slice()),
        ];
        for (fmt, ext, magic) in cases {
            let (bytes, got_ext) = encode_rgba(fmt, 80, 4, 1, &rgba).unwrap();
            assert_eq!(got_ext, ext, "{fmt:?} 的扩展名出口");
            assert_eq!(fmt.ext(), ext);
            assert!(
                bytes.len() > 12,
                "{fmt:?} 不得编码出空洞字节：{}",
                bytes.len()
            );
            assert!(
                bytes.starts_with(magic),
                "{fmt:?} 文件头应为 {magic:?}，实际 {bytes:?}"
            );
        }
        // WebP 的 RIFF 是容器名不是编码名：第二枚魔数必须一起对，否则任何 RIFF 文件都算过
        let (webp, _) = encode_rgba(EncodeFormat::WebP, 80, 4, 1, &rgba).unwrap();
        assert_eq!(&webp[8..12], b"WEBP");
    }

    #[test]
    #[allow(non_snake_case)]
    fn encodeRgba_jpegFlattensAlphaOverWhite() {
        let rgba = rgba_strip();
        let (bytes, _) = encode_rgba(EncodeFormat::Jpeg, 95, 4, 1, &rgba).unwrap();
        let out = image::load_from_memory(&bytes)
            .unwrap()
            .to_rgba8()
            .into_raw();
        let px = |i: usize| [out[i * 4], out[i * 4 + 1], out[i * 4 + 2], out[i * 4 + 3]];
        // JPEG 有损，容差 8：断言的是"压平到白底"这个语义，不是逐位精确
        let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 8;
        let expect = |i: usize, want: [u8; 3]| {
            let got = px(i);
            assert!(
                want.iter().zip(got.iter()).all(|(w, g)| near(*w, *g)),
                "像素 {i} 期望 {want:?} 实得 {got:?}"
            );
        };
        expect(0, [255, 0, 0]);
        // (0,255,0) α=128 合成白底：R/B = 0·128/255 + 255·127/255 ≈ 127
        expect(1, [127, 255, 127]);
        // 全透明蓝 → 完全取底色（这一枚是"合成"而非"丢弃"的证据：丢弃会留蓝色）
        expect(2, [255, 255, 255]);
        expect(3, [255, 255, 255]);
        // 正对照：同一批像素走 png 时 alpha 通道原样保留
        let (png, _) = encode_rgba(EncodeFormat::Png, 95, 4, 1, &rgba).unwrap();
        let png_out = image::load_from_memory(&png).unwrap().to_rgba8().into_raw();
        let png_alpha = |i: usize| png_out[i * 4 + 3];
        assert_eq!(png_alpha(2), 0, "png 必须留住全透明");
        assert_eq!(png_alpha(1), 128, "png 半透明像素的 α 原样存档");
        // 合成算术本体另钉一次（编码器容差之外的确定性一侧）
        assert_eq!(
            rgba_to_rgb_over(&rgba, JPEG_BG_RGB),
            vec![255, 0, 0, 127, 255, 127, 255, 255, 255, 255, 255, 255]
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn encodeRgba_webpPreservesAlpha() {
        let rgba = rgba_strip();
        let (bytes, ext) = encode_rgba(EncodeFormat::WebP, 80, 4, 1, &rgba).unwrap();
        assert_eq!(ext, "webp");
        let img = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(
            img.into_raw(),
            rgba,
            "webp 无损档必须逐字节还原含 alpha 的输入"
        );
        // quality 对 WebP 无效的诚实声明：两个极端质量产出同一字节流（不是"参数没接上"，
        // 是本代 `image` 只有 VP8L；honours_quality 把这事实编码成可查询的一面）
        let (lo, _) = encode_rgba(EncodeFormat::WebP, 5, 4, 1, &rgba).unwrap();
        let (hi, _) = encode_rgba(EncodeFormat::WebP, 95, 4, 1, &rgba).unwrap();
        assert_eq!(lo, hi);
        assert!(!EncodeFormat::WebP.honours_quality());
        // 正对照：JPEG 认这个参数，且 low/high 明显不同尺寸（否则 quality 是假接线）
        assert!(EncodeFormat::Jpeg.honours_quality());
        let (q10, _) =
            encode_rgba(EncodeFormat::Jpeg, 10, 32, 32, &vec![9u8; 32 * 32 * 4]).unwrap();
        let (q95, _) =
            encode_rgba(EncodeFormat::Jpeg, 95, 32, 32, &vec![9u8; 32 * 32 * 4]).unwrap();
        assert!(
            q10.len() < q95.len(),
            "quality 未进 JPEG 编码器：q10={} q95={}",
            q10.len(),
            q95.len()
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn encodeRgba_unknownFormat_rejectsNamingValue() {
        let e = EncodeFormat::from_str_honest("tiff").unwrap_err();
        assert_eq!(e.code(), "SCREENSHOT_FORMAT_001");
        let msg = e.to_string();
        assert!(
            msg.contains("tiff"),
            "红线：消息必须点名收到的值，实际 {msg}"
        );
        assert!(
            msg.contains("png"),
            "红线：消息必须列出可用集合，实际 {msg}"
        );
        assert!(msg.contains("jpeg") && msg.contains("webp"));
        // 空串与纯空白同样拒（"没填"与"填了 png"是两件事，配置层用 None/默认表达前者）
        assert!(EncodeFormat::from_str_honest("").is_err());
        assert!(EncodeFormat::from_str_honest("   ").is_err());
        // 正对照：大小写与前后空白无害，三枚全解析成功
        for (s, want) in [
            ("PNG", EncodeFormat::Png),
            (" jpeg", EncodeFormat::Jpeg),
            ("WebP ", EncodeFormat::WebP),
        ] {
            assert_eq!(EncodeFormat::from_str_honest(s).unwrap(), want, "{s:?}");
        }
    }

    #[test]
    #[allow(non_snake_case)]
    fn resolveFilename_fmtAndTsPlaceholders() {
        let ts = "2026-09-22_070500";
        assert_eq!(
            resolve_filename("shot_{ts}", ts, EncodeFormat::Jpeg),
            format!("shot_{ts}")
        );
        // {fmt} 给的是扩展名词表（jpg 而非 jpeg），与磁盘名一致
        assert_eq!(
            resolve_filename("{ts}_export_{fmt}", ts, EncodeFormat::Jpeg),
            format!("{ts}_export_jpg")
        );
        assert_eq!(resolve_filename("a_{fmt}", ts, EncodeFormat::Png), "a_png");
        assert_eq!(
            resolve_filename("b_{fmt}", ts, EncodeFormat::WebP),
            "b_webp"
        );
        // 重复占位符全部替换（不是只换第一个：模板里写两次 {ts} 的用户不该收到半个时间戳）
        assert_eq!(
            resolve_filename("{ts}@{ts}", ts, EncodeFormat::Png),
            format!("{ts}@{ts}")
        );
        // 无占位符模板原样返回（正对照：函数不会凭空加后缀）
        assert_eq!(resolve_filename("fixed", ts, EncodeFormat::Jpeg), "fixed");
        // 未闭合的花括号原样留下（吞掉=把用户的笔误伪装成正常名）
        assert_eq!(resolve_filename("x{ts", ts, EncodeFormat::Png), "x{ts");
    }

    #[test]
    #[allow(non_snake_case)]
    fn resolveFilename_unknownPlaceholder_keptLiterally() {
        let ts = "2026-09-22_070500";
        // 红线：{nope} 连同花括号原样保留
        assert_eq!(
            resolve_filename("shot_{nope}_{ts}", ts, EncodeFormat::Png),
            format!("shot_{{nope}}_{ts}")
        );
        // {} 空占位符同样是"不认识的东西"，不吞
        assert_eq!(resolve_filename("a{}b", ts, EncodeFormat::Png), "a{}b");
        // 正对照：认识的键照常展开
        assert_eq!(
            resolve_filename("{ts}{fmt}", ts, EncodeFormat::Jpeg),
            format!("{ts}jpg")
        );
    }

    /// 单向门（后缀不再恒 .png）的读侧兜底：三形魔数嗅探与写侧 content_type 同词表
    #[test]
    fn sniff_content_type_matches_encode_side() {
        for fmt in [EncodeFormat::Png, EncodeFormat::Jpeg, EncodeFormat::WebP] {
            let (bytes, _) = encode_rgba(fmt, 60, 4, 1, &rgba_strip()).unwrap();
            assert_eq!(sniff_content_type(&bytes), fmt.content_type());
        }
        assert_eq!(sniff_content_type(b"not an image"), "unknown");
        assert_eq!(sniff_content_type(&[]), "unknown");
    }
}
