//! O1 引擎抽象（docs/impl/04 O1，DESIGN §4.3 / D-09 第 1 步）：
//! `OcrEngine` trait + `EngineRegistry` 注册表。v1 仅注册 win-ocr（OcrPort 适配器）；
//! PaddleOCR sidecar（D-09 第 2 步）届时作为新引擎项 `push` 进注册表即可，
//! 管线与模块层不改。
//!
//! 签名口径：trait 保持同步（与 OcrPort/管线一致，阻塞调用的
//! spawn_blocking/超时由命令层与事件消费层负责），规约草图的 async 仅为形态，
//! 扩展点语义（多引擎、可配置优先级、统一降级错误）在本文件全部落地。

use host_core::error::AppError;
use host_core::ports::{Frame, OcrLine, OcrPort};
use parking_lot::RwLock;
use std::sync::Arc;

use crate::types::EngineInfo;

/// OCR 引擎抽象：一个引擎 = 一种识别后端（系统 WinRT / sidecar / 插件）
pub trait OcrEngine: Send + Sync {
    /// 稳定 id（"win-ocr" | "paddle" | …），随结果 DTO 回传
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    /// 支持的语言（BCP-47）；Err = 当前不可用（语言包缺失 / sidecar 未就绪）
    fn available(&self) -> Result<Vec<String>, AppError>;
    /// 阻塞识别；lang 空串 = 引擎按用户配置语言自选
    fn recognize(&self, frame: &Frame, lang: &str) -> Result<Vec<OcrLine>, AppError>;
}

/// v1 内置引擎：win-integration [`OcrPort`]（Windows.Media.Ocr）适配器
pub struct WinOcrEngine {
    port: Arc<dyn OcrPort>,
}

impl WinOcrEngine {
    pub fn new(port: Arc<dyn OcrPort>) -> Self {
        Self { port }
    }
}

impl OcrEngine for WinOcrEngine {
    fn id(&self) -> &'static str {
        "win-ocr"
    }
    fn display_name(&self) -> &'static str {
        "Windows.Media.Ocr"
    }
    fn available(&self) -> Result<Vec<String>, AppError> {
        self.port.available_languages()
    }
    fn recognize(&self, frame: &Frame, lang: &str) -> Result<Vec<OcrLine>, AppError> {
        self.port.recognize(frame, lang)
    }
}

pub struct EngineRegistry {
    engines: Vec<Arc<dyn OcrEngine>>,
    /// 用户可配置优先级（DESIGN §4.3）：经模块 config `preferred_engine` 写入
    preferred: RwLock<String>,
    /// 用户配置的偏好语言：与 `preferred` 同处，语言选择与引擎选择由同一个对象说了算
    config_langs: RwLock<Vec<String>>,
}

/// 语言择优的单一决策点：请求内联非空 = 本次显式覆盖，否则取用户配置，
/// 两者皆空 = 空表（引擎按系统语言自选，即现网语义）。
pub fn resolve_langs(req: &[String], cfg: &[String]) -> Vec<String> {
    if req.is_empty() {
        cfg.to_vec()
    } else {
        req.to_vec()
    }
}

impl EngineRegistry {
    pub fn new(engines: Vec<Arc<dyn OcrEngine>>) -> Self {
        let preferred = engines
            .first()
            .map(|e| e.id().to_owned())
            .unwrap_or_default();
        Self {
            engines,
            preferred: RwLock::new(preferred),
            config_langs: RwLock::new(Vec::new()),
        }
    }

    pub fn set_config_langs(&self, langs: &[String]) {
        *self.config_langs.write() = langs.to_vec();
    }

    pub fn config_langs(&self) -> Vec<String> {
        self.config_langs.read().clone()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.engines.iter().any(|e| e.id() == id)
    }

    pub fn set_preferred(&self, id: &str) -> Result<(), AppError> {
        if !self.contains(id) {
            return Err(AppError::module(
                "OCR_ENGINE_003",
                format!("未知引擎 {id}"),
                Some("可选值见 ocr_engine_status 返回的 engines 列表"),
            ));
        }
        *self.preferred.write() = id.to_owned();
        Ok(())
    }

    pub fn preferred(&self) -> String {
        self.preferred.read().clone()
    }

    /// O1 选引擎算法：preferred 优先，其后按注册顺序，取首个 available 的引擎；
    /// 语言 = [`resolve_langs`]（请求覆盖 → 用户配置 → 空）中该引擎支持的首个，
    /// 无匹配（或无偏好）→ 空串（引擎按系统语言自选）。
    /// 全部不可用 → `OCR_ENGINE_001` + 可操作提示（各引擎错误并入 message）。
    pub fn pick(&self, langs: &[String]) -> Result<(Arc<dyn OcrEngine>, String), AppError> {
        let wanted = resolve_langs(langs, &self.config_langs.read());
        let preferred = self.preferred();
        let mut ordered: Vec<&Arc<dyn OcrEngine>> = Vec::with_capacity(self.engines.len());
        let mut rest = Vec::with_capacity(self.engines.len());
        for e in &self.engines {
            if e.id() == preferred {
                ordered.push(e);
            } else {
                rest.push(e);
            }
        }
        ordered.extend(rest);
        let mut errors: Vec<String> = Vec::new();
        for engine in ordered {
            let available = match engine.available() {
                Ok(a) => a,
                Err(e) => {
                    errors.push(format!("{}: {e}", engine.id()));
                    continue;
                }
            };
            let lang = wanted
                .iter()
                .find(|want| available.iter().any(|a| a.eq_ignore_ascii_case(want)))
                .cloned()
                .unwrap_or_default();
            return Ok((engine.clone(), lang));
        }
        Err(AppError::module(
            "OCR_ENGINE_001",
            format!(
                "OCR 引擎不可用：{}",
                if errors.is_empty() {
                    "没有已注册的引擎".to_owned()
                } else {
                    errors.join("；")
                }
            ),
            Some("在 Windows 设置中安装对应语言包，或下载 PaddleOCR 引擎"),
        ))
    }

    /// 逐引擎可用性探测（O8；ocr_engine_status IPC 消费）
    pub fn status(&self) -> Vec<EngineInfo> {
        self.engines
            .iter()
            .map(|e| EngineInfo {
                id: e.id().into(),
                name: e.display_name().into(),
                available: e.available().is_ok(),
            })
            .collect()
    }

    /// 全部引擎支持语言的并集（保序去重）
    pub fn languages(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in &self.engines {
            if let Ok(langs) = e.available() {
                for l in langs {
                    if !out.iter().any(|o| o.eq_ignore_ascii_case(&l)) {
                        out.push(l);
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::ports::Rect;

    struct MockEngine {
        id: &'static str,
        langs: Result<Vec<String>, ()>,
    }

    impl OcrEngine for MockEngine {
        fn id(&self) -> &'static str {
            self.id
        }
        fn display_name(&self) -> &'static str {
            self.id
        }
        fn available(&self) -> Result<Vec<String>, AppError> {
            match &self.langs {
                Ok(l) => Ok(l.clone()),
                Err(_) => Err(AppError::module(
                    "MOCK_001",
                    format!("{} 不可用", self.id),
                    None,
                )),
            }
        }
        fn recognize(&self, _frame: &Frame, _lang: &str) -> Result<Vec<OcrLine>, AppError> {
            Ok(vec![OcrLine {
                text: self.id.into(),
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 0.1,
                },
                confidence: 1.0,
            }])
        }
    }

    fn engine(id: &'static str, langs: Result<Vec<String>, ()>) -> Arc<dyn OcrEngine> {
        Arc::new(MockEngine { id, langs })
    }

    fn frame() -> Frame {
        Frame {
            width: 2,
            height: 2,
            bgra: Arc::from(vec![0u8; 16]),
            dpi_scale: 1.0,
            monitor_id: 0,
        }
    }

    #[test]
    fn pick_defaults_to_first_registered_engine() {
        let reg = EngineRegistry::new(vec![
            engine("a", Ok(vec!["zh-CN".into()])),
            engine("b", Ok(vec![])),
        ]);
        let (e, lang) = reg.pick(&[]).unwrap();
        assert_eq!(e.id(), "a");
        assert_eq!(lang, "");
    }

    #[test]
    fn pick_honours_preferred_order() {
        let reg = EngineRegistry::new(vec![
            engine("a", Ok(vec!["zh-CN".into()])),
            engine("b", Ok(vec!["en-US".into()])),
        ]);
        reg.set_preferred("b").unwrap();
        let (e, lang) = reg.pick(&["zh-CN".into(), "en-US".into()]).unwrap();
        assert_eq!(e.id(), "b");
        assert_eq!(lang, "en-US", "preferred 引擎支持的偏好语言");
    }

    #[test]
    fn pick_falls_back_when_preferred_unavailable() {
        let reg = EngineRegistry::new(vec![
            engine("a", Ok(vec!["zh-CN".into()])),
            engine("b", Err(())),
        ]);
        reg.set_preferred("b").unwrap();
        let (e, _) = reg.pick(&["zh-CN".into()]).unwrap();
        assert_eq!(e.id(), "a");
    }

    #[test]
    fn pick_unsupported_lang_falls_back_to_empty() {
        // 与旧管线口径一致：偏好语言无一被支持时空串（引擎按用户语言自选），不报错
        let reg = EngineRegistry::new(vec![engine("a", Ok(vec!["zh-CN".into()]))]);
        let (e, lang) = reg.pick(&["fr-FR".into()]).unwrap();
        assert_eq!(e.id(), "a");
        assert_eq!(lang, "");
    }

    #[test]
    fn pick_all_unavailable_returns_engine_001_with_hint() {
        let reg = EngineRegistry::new(vec![engine("a", Err(())), engine("b", Err(()))]);
        let err = match reg.pick(&[]) {
            Ok(_) => panic!("全部引擎不可用时应报错"),
            Err(e) => e,
        };
        assert_eq!(err.code(), "OCR_ENGINE_001");
        let msg = err.to_string();
        assert!(msg.contains("a 不可用"), "各引擎错误并入: {msg}");
        match err {
            AppError::Module { hint, .. } => {
                let hint = hint.unwrap_or_default();
                assert!(
                    hint.contains("语言包") && hint.contains("PaddleOCR"),
                    "{hint}"
                );
            }
            other => panic!("期望 Module 变体，实际: {other:?}"),
        }
    }

    #[test]
    fn set_preferred_rejects_unknown_id() {
        let reg = EngineRegistry::new(vec![engine("a", Ok(vec![]))]);
        let err = reg.set_preferred("paddle").unwrap_err();
        assert_eq!(err.code(), "OCR_ENGINE_003");
        assert_eq!(reg.preferred(), "a", "拒绝后保持原优先级");
    }

    #[test]
    fn status_and_languages_union_follow_registry() {
        let reg = EngineRegistry::new(vec![
            engine("a", Ok(vec!["zh-CN".into(), "en-US".into()])),
            engine("b", Err(())),
        ]);
        let st = reg.status();
        assert_eq!(st.len(), 2);
        assert!(st[0].available && !st[1].available);
        assert_eq!(
            reg.languages(),
            vec!["zh-CN".to_string(), "en-US".to_string()]
        );
    }

    // ---- T-B4-10（09 §9.2）：语言择优单一决策点 ----

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-10）字面测试名优先于 rustc 命名惯例
    fn resolveLangs_precedence_pinned() {
        let req = vec!["en-US".to_string()];
        let cfg = vec!["zh-CN".to_string()];
        assert_eq!(
            resolve_langs(&req, &cfg),
            req,
            "请求内联非空 = 本次显式覆盖"
        );
        assert_eq!(resolve_langs(&[], &cfg), cfg, "无覆盖时取用户配置");
        assert!(
            resolve_langs(&[], &[]).is_empty(),
            "两者皆空 = 引擎自选（现语义不变）"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-10）字面测试名优先于 rustc 命名惯例
    fn ocrConfig_langs_usedWhenRequestOmits() {
        let reg = EngineRegistry::new(vec![engine("a", Ok(vec!["zh-CN".into(), "en-US".into()]))]);
        reg.set_config_langs(&["zh-CN".into()]);
        let (_, lang) = reg.pick(&[]).unwrap();
        assert_eq!(lang, "zh-CN", "请求未选语言时引擎收到配置里的偏好语言");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-10）字面测试名优先于 rustc 命名惯例
    fn ocrConfig_requestLangsOverrideConfig() {
        let reg = EngineRegistry::new(vec![engine("a", Ok(vec!["zh-CN".into(), "en-US".into()]))]);
        reg.set_config_langs(&["zh-CN".into()]);
        let (_, lang) = reg.pick(&["en-US".into()]).unwrap();
        assert_eq!(
            lang, "en-US",
            "面板本次手选优先于持久配置（两臂齐备才算数）"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-10）字面测试名优先于 rustc 命名惯例
    fn ocrConfig_bothEmpty_engineAutoSelectEmptyString() {
        let reg = EngineRegistry::new(vec![engine("a", Ok(vec!["zh-CN".into()]))]);
        let (_, lang) = reg.pick(&[]).unwrap();
        assert_eq!(
            lang, "",
            "未配置且未手选 = 空串（引擎按系统语言自选），不得凭空造偏好"
        );
    }

    #[test]
    fn win_engine_adapts_port() {
        struct MockPort;
        impl OcrPort for MockPort {
            fn available_languages(&self) -> Result<Vec<String>, AppError> {
                Ok(vec!["zh-CN".into()])
            }
            fn recognize(&self, image: &Frame, lang: &str) -> Result<Vec<OcrLine>, AppError> {
                assert_eq!(image.width, 2);
                Ok(vec![OcrLine {
                    text: lang.into(),
                    rect: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 1.0,
                        h: 0.1,
                    },
                    confidence: 1.0,
                }])
            }
        }
        let e = WinOcrEngine::new(Arc::new(MockPort));
        let reg = EngineRegistry::new(vec![Arc::new(e)]);
        let (picked, _) = reg.pick(&["zh-CN".into()]).unwrap();
        assert_eq!(picked.id(), "win-ocr");
        assert_eq!(picked.display_name(), "Windows.Media.Ocr");
        assert_eq!(
            picked.recognize(&frame(), "zh-CN").unwrap()[0].text,
            "zh-CN",
            "识别调用透传到 OcrPort"
        );
    }
}
