//! Windows.Media.Ocr 系统 OCR 引擎（docs/impl/04 O2）
//!
//! 流程：BGRA bytes → IBuffer → SoftwareBitmap(Bgra8) → OcrEngine → RecognizeAsync。
//! `recognize` 为阻塞实现（`.get()`）：OcrPort 契约即同步，模块层经 spawn_blocking
//! 在 MTA 线程池调用，无 STA 死锁风险（docs/impl/04 O2 潜在问题 2 的规避方式）。

use windows::Foundation::Rect as WinRect;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;

use host_core::error::AppError;
use host_core::ports::{Frame, OcrLine, OcrPort, Rect};

fn err(code: &str, m: impl std::fmt::Display) -> AppError {
    AppError::module(code, m.to_string(), None)
}

pub struct WinOcr;

impl WinOcr {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WinOcr {
    fn default() -> Self {
        Self::new()
    }
}

impl OcrPort for WinOcr {
    fn available_languages(&self) -> Result<Vec<String>, AppError> {
        let langs = OcrEngine::AvailableRecognizerLanguages()
            .map_err(|e| err("OCR_ENGINE_001", format!("枚举语言失败: {e}")))?;
        Ok(langs
            .into_iter()
            .filter_map(|l| l.LanguageTag().ok().map(|t| t.to_string()))
            .collect())
    }

    fn recognize(&self, image: &Frame, lang: &str) -> Result<Vec<OcrLine>, AppError> {
        // ① BGRA → SoftwareBitmap（Bgra8 直拷，免 PNG 编解码）
        let buffer = CryptographicBuffer::CreateFromByteArray(image.bgra.as_ref())
            .map_err(|e| err("OCR_INPUT_001", format!("构建像素缓冲失败: {e}")))?;
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            image.width as i32,
            image.height as i32,
        )
        .map_err(|e| err("OCR_INPUT_002", format!("构建位图失败: {e}")))?;

        // ② 选引擎：lang 为空 → 用户配置语言；语言包缺失（null/Err）→ 可操作错误
        let engine_result = if lang.is_empty() {
            OcrEngine::TryCreateFromUserProfileLanguages()
        } else {
            let language = Language::CreateLanguage(&windows::core::HSTRING::from(lang))
                .map_err(|e| err("OCR_ENGINE_002", format!("语言标签无效: {e}")))?;
            OcrEngine::TryCreateFromLanguage(&language)
        };
        // Try* 静态在语言包缺失时返回 null 接口，windows crate 投影为 Err(E_FAIL/E_POINTER)
        let engine = engine_result.map_err(|_| {
            AppError::module(
                "OCR_ENGINE_001",
                "OCR 引擎不可用：未安装对应语言包",
                Some("在 Windows 设置 → 时间和语言 → 语言中添加语言包，或下载 PaddleOCR 引擎"),
            )
        })?;

        // ③ 识别（MTA 线程池上阻塞等待安全）
        let result = engine
            .RecognizeAsync(&bitmap)
            .map_err(|e| err("OCR_RUN_001", format!("启动识别失败: {e}")))?
            .get()
            .map_err(|e| err("OCR_RUN_002", format!("识别失败: {e}")))?;

        // ④ Word 坐标聚合为行框，归一化 0..1
        let lines = result
            .Lines()
            .map_err(|e| err("OCR_RUN_003", format!("读取结果失败: {e}")))?;
        let (iw, ih) = (image.width as f32, image.height as f32);
        let mut out: Vec<OcrLine> = Vec::new();
        for line in lines.into_iter() {
            let text = line
                .Text()
                .map_err(|e| err("OCR_RUN_003", format!("读取行文本失败: {e}")))?;
            let mut min_x = f32::MAX;
            let mut min_y = f32::MAX;
            let mut max_x = f32::MIN;
            let mut max_y = f32::MIN;
            if let Ok(words) = line.Words() {
                for word in words.into_iter() {
                    if let Ok(r) = word.BoundingRect() {
                        let WinRect { X, Y, Width, Height } = r;
                        min_x = min_x.min(X as f32);
                        min_y = min_y.min(Y as f32);
                        max_x = max_x.max((X + Width) as f32);
                        max_y = max_y.max((Y + Height) as f32);
                    }
                }
            }
            let rect = if min_x <= max_x && min_y <= max_y {
                Rect {
                    x: (min_x / iw).clamp(0.0, 1.0),
                    y: (min_y / ih).clamp(0.0, 1.0),
                    w: ((max_x - min_x) / iw).clamp(0.0, 1.0),
                    h: ((max_y - min_y) / ih).clamp(0.0, 1.0),
                }
            } else {
                Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }
            };
            out.push(OcrLine {
                text: text.to_string(),
                rect,
                // 该引擎无置信度输出：统一 1.0（docs/impl/04 O2 潜在问题 4）
                confidence: 1.0,
            });
        }
        Ok(out)
    }
}
