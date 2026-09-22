//! 识别管线（docs/impl/04 O4）：预处理 → 引擎识别（O1 注册表 pick）→ 行排序重建 → 文本合并
//!
//! 引擎选择与降级统一在 [`EngineRegistry::pick`]（D-09 第 1 步）；
//! Paddle Sidecar（O3）与翻译引擎（O6）为独立里程碑，届时向注册表追加引擎项即可，
//! 本文件的排序/合并不变。

use host_core::error::AppError;
use host_core::ports::{Frame, OcrLine};

use crate::engine::EngineRegistry;
use crate::types::OcrResultDto;

use host_core::util::app_err as err;

pub struct OcrPipeline<'a> {
    engines: &'a EngineRegistry,
}

impl<'a> OcrPipeline<'a> {
    pub fn new(engines: &'a EngineRegistry) -> Self {
        Self { engines }
    }

    /// 完整识别（阻塞；命令层负责 spawn_blocking + 超时）
    pub fn run(&self, frame: &Frame, langs: &[String]) -> Result<OcrResultDto, AppError> {
        // ① 预处理：OcrEngine 输入为像素帧；> 4096px 的图引擎可能失败，
        //    缩放放命令层（decode 时已知尺寸），此处仅校验
        if frame.width == 0 || frame.height == 0 {
            return Err(err("OCR_INPUT_001", "图像尺寸无效"));
        }

        // ② 选引擎与语言（O1）：preferred 优先、注册顺序兜底、全不可用 → OCR_ENGINE_001
        let (engine, lang) = self.engines.pick(langs)?;

        // ③ 引擎识别
        let raw_lines = engine.recognize(frame, &lang)?;

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
            engine: engine.id().into(),
            // 置信度诚实位（T-B4-13）：由本次实际所用引擎自己表态；**数值一律照抄**，
            // 不做 false→改 0.0 之类的粉饰，"未知"由这一位说清楚。
            engines_report_confidence: self.engines.reports_confidence(engine.id()),
            // 翻译槽由模块层填（它才持有设置与 provider 注册表）
            translate: None,
            translate_error: None,
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
        a.rect
            .y
            .partial_cmp(&b.rect.y)
            .unwrap_or(std::cmp::Ordering::Equal)
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
            a.rect
                .x
                .partial_cmp(&b.rect.x)
                .unwrap_or(std::cmp::Ordering::Equal)
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
    use std::sync::Arc;

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

    // ---- T-B4-13（09 §9.2）：结果诚实化的两处钉（1:1 透传 + false 时不粉饰数值）----

    /// 表态可控的三行引擎：rect/confidence 逐值不同，才能证明"透传"而非"重算"
    struct TransportEngine {
        conf: f32,
        reports: bool,
    }
    impl crate::engine::OcrEngine for TransportEngine {
        fn id(&self) -> &'static str {
            "transport"
        }
        fn display_name(&self) -> &'static str {
            "transport"
        }
        fn available(&self) -> Result<Vec<String>, AppError> {
            Ok(vec!["zh-CN".into()])
        }
        fn recognize(&self, _frame: &Frame, _lang: &str) -> Result<Vec<OcrLine>, AppError> {
            let row = |text: &str, x: f32, y: f32, w: f32, h: f32| OcrLine {
                text: text.into(),
                rect: Rect { x, y, w, h },
                confidence: self.conf,
            };
            Ok(vec![
                row("第一行", 0.10, 0.10, 0.30, 0.04),
                row("第二行", 0.15, 0.40, 0.20, 0.06),
                row("第三行", 0.20, 0.70, 0.10, 0.08),
            ])
        }
        fn reports_confidence(&self) -> bool {
            self.reports
        }
    }

    fn tiny_frame() -> Frame {
        Frame {
            width: 2,
            height: 2,
            bgra: Arc::from(vec![0u8; 16]),
            dpi_scale: 1.0,
            monitor_id: 0,
        }
    }

    fn pipeline_engine(reports: bool) -> EngineRegistry {
        EngineRegistry::new(vec![Arc::new(TransportEngine {
            conf: 0.42,
            reports,
        })])
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-13）字面测试名优先于 rustc 命名惯例
    fn ocrLineRectAndConfidence_alreadyTransported_1to1() {
        // §9.1-⑫(b) 的正面钉：DTO 侧 rect/confidence 早已 1:1 透传，本行不改这个形状
        let reg = pipeline_engine(true);
        let out = OcrPipeline::new(&reg).run(&tiny_frame(), &[]).unwrap();
        assert_eq!(out.lines.len(), 3, "三行进三行出：聚类/重建不得吞行");
        let want = [
            ("第一行", 0.10f32, 0.10f32, 0.30f32, 0.04f32),
            ("第二行", 0.15, 0.40, 0.20, 0.06),
            ("第三行", 0.20, 0.70, 0.10, 0.08),
        ];
        for (got, (text, x, y, w, h)) in out.lines.iter().zip(want.iter()) {
            assert_eq!(got.text, *text);
            assert_eq!(
                (got.rect.x, got.rect.y, got.rect.w, got.rect.h),
                (*x, *y, *w, *h)
            );
            assert_eq!(got.confidence, 0.42, "引擎给的数值原样交出");
        }
        assert!(out.engines_report_confidence, "表态 true 时结果位为 true");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-13）字面测试名优先于 rustc 命名惯例
    fn confidenceFalse_neverClaimedAsHundred() {
        // 引擎不报置信度时，管线交出的仍是引擎给的那个值 + false 一位：
        // 把占位值改写成 0.0 是第二层谎（"既然未知那就显个低的"），前端读的是那一位
        let reg = pipeline_engine(false);
        let out = OcrPipeline::new(&reg).run(&tiny_frame(), &[]).unwrap();
        assert!(!out.engines_report_confidence);
        assert!(
            out.lines.iter().all(|l| l.confidence == 0.42),
            "数值未被改写：{:?}",
            out.lines.iter().map(|l| l.confidence).collect::<Vec<_>>()
        );
        let json = serde_json::to_string(&out).unwrap();
        assert!(
            json.contains("\"engines_report_confidence\":false"),
            "{json}"
        );
    }
}
