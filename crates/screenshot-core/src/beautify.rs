//! 截图美化（docs/impl/09 §9.2 T-B4-6）：像素进、像素出的纯函数层
//!
//! 静态判据（该行完成判据）：本文件对 `image` crate 的引用数为零——美化只改像素，不碰编码，
//! 磁盘与剪贴板的出口全仓仍只有 `util::encode_rgba` 一个（T-B4-7 收口纪律的延伸）。
//! 顺序即语义：外扩画布 → 线性渐变底 → 源图居中 → 圆角掩码 → 阴影。

use host_core::error::AppError;
use serde::{Deserialize, Serialize};

use host_core::util::app_err as err;

/// 美化后单边上限：超过即点名拒，**绝不静默缩图**（静默缩图=用户以为导的是原分辨率）
pub const BEAUTIFY_MAX_EDGE: u32 = 8192;
/// 阴影纵向扩散带（px）：card 底边之下另起这么多行放阴影（阴影带在 card 之外，
/// 因此"底边下方首行"的 alpha 才是阴影本身而不是不透明白底）
pub const BEAUTIFY_SHADOW_SPREAD: u32 = 24;
/// 阴影首行强度（255 制）。小于 255 才有"渐隐"可言——首行就是满强度的话
/// 这条带子是一块黑砖，不是投影
const SHADOW_TOP_STRENGTH: u32 = 160;

/// 美化参数（请求级，不进配置文件：美化是一次性装饰而非全局偏好）
///
/// serde 缺省值是**装饰预设**（深色渐变底），而 `Default` 是**恒等预设**（全 0 + 两 bg
/// 同黑）——两者不同是有意的：JSON 缺色值时用户要的是"给个好看的底"，而代码里
/// `BeautifySpec::default()` 必须等于"什么都没干"（`beautify_allZeroSpec_isByteIdentical` 钉）。
/// padding=0 时两枚底色永不参与输出（源图区域原样搬运），因此 `{}` 反序列化出来
/// 的图与恒等预设逐字节相等，分野只在肉眼看不见的字段值上。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeautifySpec {
    /// 圆角半径（px，0 = 直角）
    #[serde(default)]
    pub radius: u32,
    /// 四周外扩内边距（px，0 = 不外扩）
    #[serde(default)]
    pub padding: u32,
    /// 是否投影
    #[serde(default)]
    pub shadow: bool,
    /// 渐变起点色（`#RRGGBB`，card 顶行）
    #[serde(default = "default_bg_from")]
    pub bg_from: String,
    /// 渐变终点色（`#RRGGBB`，card 底行）
    #[serde(default = "default_bg_to")]
    pub bg_to: String,
}

fn default_bg_from() -> String {
    "#1f2937".into()
}
fn default_bg_to() -> String {
    "#0b1220".into()
}

/// 恒等预设：四值全 0 + 两 bg 同黑。任何"美化默认改了图"在这份定义下即测试红
impl Default for BeautifySpec {
    fn default() -> Self {
        Self {
            radius: 0,
            padding: 0,
            shadow: false,
            bg_from: "#000000".into(),
            bg_to: "#000000".into(),
        }
    }
}

/// `#RRGGBB`（大小写不敏感、去首尾空白）→ RGB；其余形状一律 None，由调用侧点名拒
pub fn parse_hex_color(s: &str) -> Option<[u8; 3]> {
    let t = s.trim();
    let hex = t.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok();
    Some([byte(0)?, byte(1)?, byte(2)?])
}

fn oversize(w: u32, h: u32, cw: u32, ch: u32, spec: &BeautifySpec) -> AppError {
    err(
        "SCREENSHOT_BEAUTIFY_001",
        format!(
            "美化后尺寸 {cw}×{ch} 超上限 {BEAUTIFY_MAX_EDGE}（源图 {w}×{h}、padding={}、阴影={}）；\
             本函数不静默缩图，请减小内边距或先缩放源图",
            spec.padding, spec.shadow
        ),
    )
}

fn bad_color(raw: &str) -> AppError {
    err(
        "SCREENSHOT_BEAUTIFY_002",
        format!("底色 \"{raw}\" 不是 #RRGGBB 形状（宁可按名拒，也不把它悄悄当黑色画出去）"),
    )
}

/// 目标尺寸（含阴影带）：溢出与上限在此一处判定，`beautify` 只信任它
pub fn beautified_size(w: u32, h: u32, spec: &BeautifySpec) -> Result<(u32, u32), AppError> {
    if w == 0 || h == 0 {
        return Err(err(
            "SCREENSHOT_BEAUTIFY_001",
            format!("源图尺寸 {w}×{h} 含 0，美化无从下手"),
        ));
    }
    let over = || err("SCREENSHOT_BEAUTIFY_001", "padding 相加溢出");
    let pad2 = spec.padding.checked_mul(2).ok_or_else(over)?;
    let cw = w.checked_add(pad2).ok_or_else(over)?;
    let extra = if spec.shadow {
        BEAUTIFY_SHADOW_SPREAD
    } else {
        0
    };
    let ch = h
        .checked_add(pad2)
        .ok_or_else(over)?
        .checked_add(extra)
        .ok_or_else(over)?;
    if cw > BEAUTIFY_MAX_EDGE || ch > BEAUTIFY_MAX_EDGE {
        return Err(oversize(w, h, cw, ch, spec));
    }
    Ok((cw, ch))
}

/// 顶到底的线性插值（t 为 0..=255 的行位置，负差走 i32 而不是先减后钳）
fn lerp3(from: [u8; 3], to: [u8; 3], t: u32) -> [u8; 3] {
    let mut out = [0u8; 3];
    for c in 0..3 {
        let a = from[c] as i32;
        let b = to[c] as i32;
        out[c] = (a + (b - a) * t as i32 / 255).clamp(0, 255) as u8;
    }
    out
}

/// 圆角掩码的 alpha 系数（0..=255）：矩形圆角 SDF 之外的像素按**二次**衰减，
/// 衰减带宽 r/4 —— 硬切会在斜边上留下阶梯，"圆角"就变成"切角"；
/// 带宽取 r/2 时正角上还剩 ~40 的残 alpha（切得不够干净），取 r/4 才归零
fn corner_factor(x: u32, y: u32, cw: u32, ch: u32, radius: u32) -> u32 {
    if radius == 0 {
        return 255;
    }
    // 半径大于半个短边时钳到短边一半：否则圆角互相吞掉，掩码中心反而成空洞
    let r = radius.min(cw / 2).min(ch / 2) as f64;
    if r <= 0.0 {
        return 255;
    }
    // 标准圆角矩形 SDF：q = |p| - 半宽高 + r；d = |max(q,0)| + min(max(q),0) - r
    let qx = (x as f64 + 0.5 - cw as f64 / 2.0).abs() - cw as f64 / 2.0 + r;
    let qy = (y as f64 + 0.5 - ch as f64 / 2.0).abs() - ch as f64 / 2.0 + r;
    let outside = (qx.max(0.0) * qx.max(0.0) + qy.max(0.0) * qy.max(0.0)).sqrt();
    let d = outside + qx.max(qy).min(0.0) - r;
    if d <= 0.0 {
        return 255;
    }
    let band = (r / 4.0).max(1.0);
    let t = (1.0 - d / band).clamp(0.0, 1.0);
    (t * t * 255.0) as u32
}

/// 美化：`(rgba, w, h, spec)` → `(新宽, 新高, 新 RGBA 字节)`
///
/// 源图区域**原样搬运**而不是先铺底再合成——这一条是"恒等预设"的结构保证：
/// padding=0 && radius=0 && !shadow 时输出与输入逐字节相等，靠的是"没有任何一步
/// 碰到源图像素"，而不是函数开头的一句特判（特判会随字段增加而失真）。
/// 代价如实登记：半透明源图在 padding=0 时不会得到底色垫背（截图帧的 alpha 恒 255，
/// 本代无从触发；若将来接带透明的来源，这里要显式合成而不是维持搬运）。
///
/// 阴影取**源图底行的 alpha 水平翻转**作为断面、纵向二次渐隐：截图底行恒不透明，
/// 因此阴影是矩形带。这是已知边界而非疏漏——"带圆角的整卡投影"需要改用③之后的
/// card alpha，归后续迭代。
pub fn beautify(
    rgba: &[u8],
    w: u32,
    h: u32,
    spec: &BeautifySpec,
) -> Result<(u32, u32, Vec<u8>), AppError> {
    let (ow, oh) = beautified_size(w, h, spec)?;
    let need = w as usize * h as usize * 4;
    if rgba.len() < need {
        return Err(err(
            "SCREENSHOT_BEAUTIFY_003",
            format!(
                "像素缓冲与声明尺寸不符：{w}×{h} 需 {need} 字节，实得 {}",
                rgba.len()
            ),
        ));
    }
    let from = parse_hex_color(&spec.bg_from).ok_or_else(|| bad_color(&spec.bg_from))?;
    let to = parse_hex_color(&spec.bg_to).ok_or_else(|| bad_color(&spec.bg_to))?;

    let pitch = ow as usize;
    let p = spec.padding as usize;
    let shadow_rows = if spec.shadow {
        BEAUTIFY_SHADOW_SPREAD as usize
    } else {
        0
    };
    let card_h = oh as usize - shadow_rows;
    let mut out = vec![0u8; pitch * oh as usize * 4];

    // ① 线性渐变底（左上→右下按 y 插值；只铺 card，阴影带留给④写）
    for y in 0..card_h {
        let t = if card_h > 1 {
            (y * 255) / (card_h - 1)
        } else {
            0
        };
        let rgb = lerp3(from, to, t as u32);
        let row = y * pitch * 4;
        for x in 0..pitch {
            let d = row + x * 4;
            out[d] = rgb[0];
            out[d + 1] = rgb[1];
            out[d + 2] = rgb[2];
            out[d + 3] = 255;
        }
    }

    // ② 源图居中
    for y in 0..h as usize {
        for x in 0..w as usize {
            let s = (y * w as usize + x) * 4;
            let d = ((y + p) * pitch + x + p) * 4;
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }

    // ③ 圆角掩码（整张 card 的 alpha，含底与图：圆角外是"什么都不透出"，不是黑边）
    if spec.radius > 0 {
        for y in 0..card_h {
            for x in 0..pitch {
                let f = corner_factor(x as u32, y as u32, ow, card_h as u32, spec.radius);
                if f == 255 {
                    continue;
                }
                let d = (y * pitch + x) * 4;
                out[d + 3] = (out[d + 3] as u32 * f / 255) as u8;
            }
        }
    }

    // ④ 阴影：card 底边之下的独立带（黑色 + 二次渐隐），因此"底边下方首行"
    //    的 alpha 就是阴影强度本身，而不是被不透明白底盖住
    for dy in 0..shadow_rows {
        let t = (shadow_rows - dy) as f64 / shadow_rows as f64;
        let fade = (t * t * 255.0) as u32;
        let row = (card_h + dy) * pitch * 4;
        for x in 0..w as usize {
            let mirrored = w as usize - 1 - x;
            let src_a = rgba[((h as usize - 1) * w as usize + mirrored) * 4 + 3] as u32;
            let a = src_a * SHADOW_TOP_STRENGTH * fade / (255 * 255);
            let d = row + (x + p) * 4;
            out[d] = 0;
            out[d + 1] = 0;
            out[d + 2] = 0;
            out[d + 3] = a.min(255) as u8;
        }
    }

    Ok((ow, oh, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 纯色不透明源图（每像素 [g,g,g,255]，便于"源图被搬运而非被改写"的逐字节断言）
    fn solid(w: u32, h: u32, v: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            out.extend_from_slice(&[v, v, v, 255]);
        }
        out
    }

    fn px(buf: &[u8], pitch: usize, x: usize, y: usize) -> [u8; 4] {
        let d = (y * pitch + x) * 4;
        [buf[d], buf[d + 1], buf[d + 2], buf[d + 3]]
    }

    #[test]
    fn hex_color_parse_accepts_case_and_trim_rejects_shapes() {
        assert_eq!(parse_hex_color("#1F2937"), Some([0x1F, 0x29, 0x37]));
        assert_eq!(parse_hex_color("  #0b1220 "), Some([0x0B, 0x12, 0x20]));
        // 负例三形：缺 # / 三位缩写 / 非十六进制——一律 None（调用侧点名拒），
        // 悄悄当黑色画出去才是这里最坏的失败方式
        assert_eq!(parse_hex_color("1f2937"), None);
        assert_eq!(parse_hex_color("#fff"), None);
        assert_eq!(parse_hex_color("#gggggg"), None);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-6）字面测试名优先于 rustc 命名惯例
    fn beautify_allZeroSpec_isByteIdentical() {
        let src = solid(7, 5, 0x40);
        let (w, h, out) = beautify(&src, 7, 5, &BeautifySpec::default()).unwrap();
        assert_eq!((w, h), (7, 5));
        assert_eq!(out, src, "恒等预设必须逐字节不动图");
        // 同一件事的 JSON 形：{} 里连 bg 都没有，serde 给的是装饰渐变底色，
        // 但 padding=0 时底色永不参与输出——"缺省值不同"不等于"图不同"
        let from_empty: BeautifySpec = serde_json::from_str("{}").unwrap();
        assert_ne!(from_empty, BeautifySpec::default());
        assert_eq!(beautify(&src, 7, 5, &from_empty).unwrap().2, src);
    }

    #[test]
    #[allow(non_snake_case)]
    fn beautify_paddingCentersSourceOnGradient() {
        let src = solid(4, 4, 0x80);
        let spec = BeautifySpec {
            radius: 0,
            padding: 6,
            shadow: false,
            bg_from: "#000000".into(),
            bg_to: "#0000ff".into(),
        };
        let (ow, oh, out) = beautify(&src, 4, 4, &spec).unwrap();
        assert_eq!((ow, oh), (16, 16));
        // 源图左上角落在 ((W-w)/2, (H-h)/2)，且四像素搬运未改
        let left = ((ow - 4) / 2) as usize;
        let top = ((oh - 4) / 2) as usize;
        assert_eq!((left, top), (6, 6));
        for y in 0..4usize {
            for x in 0..4usize {
                assert_eq!(
                    px(&out, ow as usize, left + x, top + y),
                    [0x80, 0x80, 0x80, 255]
                );
            }
        }
        let pitch = ow as usize;
        assert_eq!(px(&out, pitch, 0, 0), [0, 0, 0, 255], "顶行=渐变起点");
        assert_eq!(px(&out, pitch, 0, 15), [0, 0, 255, 255], "底行=渐变终点");
        // 中间行确实是插值而不是端点二选一（y=8 → t=128/255 → B≈128）
        let mid = px(&out, pitch, 0, 8)[2];
        assert!((120..=136).contains(&mid), "渐变中段实得 B={mid}");
    }

    #[test]
    #[allow(non_snake_case)]
    fn beautify_roundCorners_areTransparentOutsideMask() {
        let src = solid(20, 20, 0x40);
        let spec = BeautifySpec {
            radius: 6,
            padding: 2,
            shadow: false,
            bg_from: "#ffffff".into(),
            bg_to: "#ffffff".into(),
        };
        let (ow, oh, out) = beautify(&src, 20, 20, &spec).unwrap();
        assert_eq!((ow, oh), (24, 24));
        let pitch = ow as usize;
        assert_eq!(px(&out, pitch, 0, 0)[3], 0, "角上什么都不该透出");
        assert_eq!(px(&out, pitch, pitch - 1, oh as usize - 1)[3], 0);
        // 距角 2r 处（沿对角线）已完全在圆角内
        let d = (2.0 * 6.0 / 2.0f64.sqrt()).round() as usize; // 2r 沿对角线的整坐标步长
        assert_eq!(px(&out, pitch, d, d)[3], 255, "距角 2r 处必不透明");
        // 二次衰减的中间态：斜边上存在 0<a<255 的过渡像素（硬切的话这一档会找不到）
        let has_soft = (0..pitch)
            .map(|x| px(&out, pitch, x, 0)[3])
            .any(|a| a > 0 && a < 255);
        assert!(has_soft, "圆角须有渐隐边，不能是切角");
    }

    #[test]
    #[allow(non_snake_case)]
    fn beautify_shadowAddsAlphaBelowBottomEdge() {
        let src = solid(10, 10, 0x40);
        let spec = BeautifySpec {
            radius: 0,
            padding: 4,
            shadow: true,
            bg_from: "#ffffff".into(),
            bg_to: "#ffffff".into(),
        };
        let (ow, oh, out) = beautify(&src, 10, 10, &spec).unwrap();
        // card 18×18 + 阴影带 24 行
        assert_eq!((ow, oh), (18, 42));
        let card_bottom = oh as usize - BEAUTIFY_SHADOW_SPREAD as usize;
        let x = (4 + 5) as usize; // 源图水平跨度内（翻转后仍在带内）
        let first = px(&out, ow as usize, x, card_bottom)[3];
        assert!(
            first > 0 && first < 255,
            "底边下方首行 alpha={first} 须落在 (0,255)"
        );
        let mut prev = first;
        for dy in 1..BEAUTIFY_SHADOW_SPREAD as usize {
            let a = px(&out, ow as usize, x, card_bottom + dy)[3];
            assert!(a <= prev, "阴影随 y 单调递减：{a} > {prev}");
            prev = a;
        }
        assert_eq!(prev, 0, "扩散带末端收干净");
        // 源图内像素不被阴影步（正对照：阴影只住 card 之外）
        assert_eq!(px(&out, ow as usize, x, 9)[3], 255);
        // 水平翻转的断面：带外（源图水平跨度之外）零阴影
        assert_eq!(px(&out, ow as usize, 0, card_bottom)[3], 0);
    }

    #[test]
    #[allow(non_snake_case)]
    fn beautify_oversizeTarget_rejects001NamingSizes() {
        let spec = BeautifySpec {
            radius: 0,
            padding: 4000,
            shadow: false,
            bg_from: "#000000".into(),
            bg_to: "#000000".into(),
        };
        let e = beautified_size(3000, 2000, &spec).unwrap_err();
        assert_eq!(e.code(), "SCREENSHOT_BEAUTIFY_001");
        let msg = e.to_string();
        // 红线：请求尺寸与上限都要点名（只说"太大了"用户无从下手）
        assert!(msg.contains("11000"), "{msg}");
        assert!(msg.contains("8192"), "{msg}");
        assert!(msg.contains("3000×2000"), "{msg}");
        // 阴影带的额外高度同样算进上限（正对照：恰好卡线的放行，多一行即拒）
        let edge = BeautifySpec {
            radius: 0,
            padding: 0,
            shadow: true,
            bg_from: "#000000".into(),
            bg_to: "#000000".into(),
        };
        assert_eq!(
            beautified_size(8192, 8192, &edge).unwrap_err().code(),
            "SCREENSHOT_BEAUTIFY_001"
        );
        assert_eq!(
            beautified_size(8192, 8192 - 24, &edge).unwrap(),
            (8192, 8192)
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-6）字面测试名优先于 rustc 命名惯例
    fn beautify_badColor_rejectsNamingValue_andBufferShortfallRejects() {
        let src = solid(2, 2, 0x10);
        let spec = BeautifySpec {
            bg_from: "red".into(),
            ..Default::default()
        };
        let e = beautify(&src, 2, 2, &spec).unwrap_err();
        assert_eq!(e.code(), "SCREENSHOT_BEAUTIFY_002");
        assert!(e.to_string().contains("red"), "{e}");
        // 字节数不足：报错而不是越界 panic（调用方是前端传来的尺寸）
        let short = BeautifySpec {
            padding: 2,
            ..Default::default()
        };
        let e = beautify(&src, 9, 9, &short).unwrap_err();
        assert_eq!(e.code(), "SCREENSHOT_BEAUTIFY_003");
    }
}
