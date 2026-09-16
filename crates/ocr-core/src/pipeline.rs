//! 识别管线（docs/impl/04 O4）：预处理 → 引擎识别 → 行排序重建 → 文本合并
//!
//! 引擎抽象说明：v1 仅接入 win-ocr（OcrPort，win-integration 实现）。
//! Paddle Sidecar（O3）与翻译引擎（O6）为独立里程碑，届时在 pipeline 前挂
//! EngineRegistry（O1）即可，本文件的排序/合并不变。

use host_core::error::AppError;
use host_core::ports::{Frame, OcrLine, OcrPort};

use crate::types::OcrResultDto;

fn err(code: &str, m: impl std::fmt::Display) -> AppError {
    AppError::module(code, m.to_string(), None)
}

pub struct OcrPipeline {
    port: std::sync::Arc<dyn OcrPort>,
}

impl OcrPipeline {
    pub fn new(port: std::sync::Arc<dyn OcrPort>) -> Self {
        Self { port }
    }

    /// 完整识别（阻塞；命令层负责 spawn_blocking + 超时）
    pub fn run(&self, frame: &Frame, langs: &[String]) -> Result<OcrResultDto, AppError> {
        // ① 预处理：OcrPort 输入为像素帧；> 4096px 的图引擎可能失败，
        //    缩放放命令层（decode 时已知尺寸），此处仅校验
        if frame.width == 0 || frame.height == 0 {
            return Err(err("OCR_INPUT_001", "图像尺寸无效"));
        }

        // ② 选语言：偏好列表中第一个引擎支持的；否则空串（引擎用用户配置语言）
        let available = self.port.available_languages()?;
        let lang = langs
            .iter()
            .find(|l| available.iter().any(|a| a.eq_ignore_ascii_case(l)))
            .cloned()
            .unwrap_or_default();

        // ③ 引擎识别
        let raw_lines = self.port.recognize(frame, &lang)?;

        // ④ 行排序重建（段聚类）
        let lines = segment_lines(raw_lines);

        // ⑤ 文本合并
        let text = merge_text(&lines);

        Ok(OcrResultDto {
            lines: lines
                .into_iter()
                .map(|l| crate::types::OcrLineDto {
                    text: l.text,
                    rect: crate::types::RectF {
                        x: l.rect.x,
                        y: l.rect.y,
                        w: l.rect.w,
                        h: l.rect.h,
                    },
                    confidence: l.confidence,
                })
                .collect(),
            text,
            lang,
            engine: "win-ocr".into(),
        })
    }
}

/// 行排序重建（docs/impl/04 O4 ③）：
/// 按行高中位数 × 0.6 的垂直阈值聚类成"段"；段内按 x 排序，段间按 y 排序。
/// 纯列表（行高接近、间距大）时每行自成一段，退化为按 y 排序——规约中的保底行为。
pub fn segment_lines(mut lines: Vec<OcrLine>) -> Vec<OcrLine> {
    if lines.len() <= 1 {
        return lines;
    }
    lines.sort_by(|a, b| {
        a.rect.y.partial_cmp(&b.rect.y).unwrap_or(std::cmp::Ordering::Equal)
    });
    // 行高中位数
    let mut heights: Vec<f32> = lines.iter().map(|l| l.rect.h).collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_h = heights[heights.len() / 2];
    let threshold = (median_h * 0.6).max(0.001);

    // 相邻行 y 差 ≤ 阈值 → 同段（段内保持 x 排序）
    let mut bands: Vec<Vec<OcrLine>> = vec![vec![lines.remove(0)]];
    for line in lines {
        let last_band = bands.last_mut().expect("至少一段");
        let last_y = last_band.last().expect("段非空").rect.y;
        if (line.rect.y - last_y).abs() <= threshold {
            last_band.push(line);
        } else {
            bands.push(vec![line]);
        }
    }
    let mut out = Vec::new();
    for band in &mut bands {
        band.sort_by(|a, b| {
            a.rect.x.partial_cmp(&b.rect.x).unwrap_or(std::cmp::Ordering::Equal)
        });
        out.extend(band.iter().cloned());
    }
    out
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF   // CJK 统一表意
        | 0x3400..=0x4DBF // 扩展 A
        | 0x3000..=0x303F // CJK 标点
        | 0xFF00..=0xFFEF // 全角
    )
}

/// 文本合并（docs/impl/04 O4 ④）：
/// 同段行间——CJK 边界直接拼接，西文边界加空格；段间换行。
pub fn merge_text(lines: &[OcrLine]) -> String {
    // 重建段结构：与 segment_lines 相同的聚类规则
    if lines.is_empty() {
        return String::new();
    }
    let mut heights: Vec<f32> = lines.iter().map(|l| l.rect.h).collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let threshold = (heights[heights.len() / 2] * 0.6).max(0.001);

    let mut paragraphs: Vec<Vec<&OcrLine>> = vec![vec![&lines[0]]];
    for line in &lines[1..] {
        let last = paragraphs.last_mut().expect("至少一段");
        let last_y = last.last().expect("段非空").rect.y;
        if (line.rect.y - last_y).abs() <= threshold {
            last.push(line);
        } else {
            paragraphs.push(vec![line]);
        }
    }

    let mut out = String::new();
    for (pi, para) in paragraphs.iter().enumerate() {
        if pi > 0 {
            out.push('\n');
        }
        for (i, line) in para.iter().enumerate() {
            if i > 0 {
                // 边界拼接规则
                let prev_last = out.chars().last();
                let next_first = line.text.chars().next();
                let join_with_space = match (prev_last, next_first) {
                    (Some(p), Some(n)) => !is_cjk(p) && !is_cjk(n),
                    _ => false,
                };
                if join_with_space {
                    out.push(' ');
                }
            }
            out.push_str(&line.text);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::ports::Rect;

    fn line(text: &str, x: f32, y: f32, w: f32, h: f32) -> OcrLine {
        OcrLine {
            text: text.into(),
            rect: Rect { x, y, w, h },
            confidence: 1.0,
        }
    }

    #[test]
    fn segment_sorts_bands_and_columns() {
        // 引擎乱序返回：右侧列(band1) + 左侧列(band2)，每列内部 x 乱序
        let lines = vec![
            line("B右", 0.6, 0.25, 0.1, 0.05),
            line("A右", 0.3, 0.25, 0.1, 0.05),
            line("B左", 0.6, 0.05, 0.1, 0.05),
            line("A左", 0.3, 0.05, 0.1, 0.05),
        ];
        let out = segment_lines(lines);
        assert_eq!(out[0].text, "A左");
        assert_eq!(out[1].text, "B左");
        assert_eq!(out[2].text, "A右");
        assert_eq!(out[3].text, "B右");
    }

    #[test]
    fn segment_pure_list_falls_back_to_y_order() {
        // 行高一致、间距大 → 每行自成段，按 y 排序
        let lines = vec![
            line("第三行", 0.1, 0.6, 0.3, 0.05),
            line("第一行", 0.1, 0.1, 0.3, 0.05),
            line("第二行", 0.1, 0.35, 0.3, 0.05),
        ];
        let out = segment_lines(lines);
        let texts: Vec<&str> = out.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["第一行", "第二行", "第三行"]);
    }

    #[test]
    fn merge_cjk_joins_without_space() {
        let lines = vec![
            line("你好", 0.1, 0.1, 0.2, 0.05),
            line("世界", 0.31, 0.1, 0.2, 0.05), // 同段（y 差 < 阈值）
        ];
        assert_eq!(merge_text(&lines), "你好世界");
    }

    #[test]
    fn merge_latin_joins_with_space() {
        let lines = vec![
            line("hello", 0.1, 0.1, 0.2, 0.05),
            line("world", 0.31, 0.11, 0.2, 0.05),
        ];
        assert_eq!(merge_text(&lines), "hello world");
    }

    #[test]
    fn merge_bands_split_with_newline() {
        let lines = vec![
            line("第一段", 0.1, 0.1, 0.2, 0.05),
            line("second", 0.1, 0.5, 0.2, 0.05),
        ];
        assert_eq!(merge_text(&lines), "第一段\nsecond");
    }
}
