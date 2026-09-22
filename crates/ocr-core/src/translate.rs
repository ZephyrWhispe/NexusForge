//! 翻译槽（docs/impl/04 O6 / D-29 B4 T-B4-13）：OCR 结果的**可注入**翻译位。
//!
//! v1 不接任何在线服务（D-08 三欠账：体积 / 下载通道 / 崩溃恢复，未清之前不落地），
//! 但接口位与消费通路先建好：`TranslateRegistry` 持 provider 列表与"启用哪个"，
//! 注册一个真 provider 即得翻译，不改 `OcrModule`、不改命令签名。
//!
//! 两条纪律：
//! - **不吞错**：槽未启用 → `Ok(None)`（一次 provider 调用都不发）；已启用而 provider 报错
//!   → `Err` 原样上抛，绝不安抚性地折成 `None`。
//! - **零网络**：本文件不出现任何 HTTP/网络依赖，`NullTranslateProvider` 只回错误；
//!   这是 D-08"维持不接在线服务"的结构证明（判据见 09 §9.2 该行完成判据）。

use host_core::error::AppError;
use parking_lot::RwLock;
use std::sync::Arc;

/// 翻译 provider：一个实现 = 一种翻译后端（本地词典 / sidecar / 在线服务）
///
/// 签名口径与同 crate [`crate::engine::OcrEngine`] 一致：trait 保持**同步**，
/// 阻塞与超时由调用侧（命令层 spawn_blocking）负责——见 engine.rs 顶部"签名口径"段。
pub trait TranslateProvider: Send + Sync {
    /// 稳定 id（随诊断信息回传，供"启用了未注册的 provider"这类错误点名）
    fn id(&self) -> &'static str;
    /// `to` = 目标语言（BCP-47，来自设置键 `translate_target_lang`）
    fn translate(&self, text: &str, to: &str) -> Result<String, AppError>;
}

/// 占位 provider（v1 唯一实现）：任何调用都如实失败，不静默返回原文
pub struct NullTranslateProvider;

/// 占位 provider 的 id（`TranslateRegistry::default_provider` 在只有它一项时返回此值）
pub const NULL_PROVIDER_ID: &str = "none";

impl TranslateProvider for NullTranslateProvider {
    fn id(&self) -> &'static str {
        NULL_PROVIDER_ID
    }
    fn translate(&self, _text: &str, _to: &str) -> Result<String, AppError> {
        Err(AppError::module(
            "OCR_TRANSLATE_001",
            "v1 未接入翻译服务（D-08）",
            Some("接口位已在，Provider 注册即得"),
        ))
    }
}

/// 翻译 provider 注册表：引擎集不可变 + 一个可切换的启用项（与 `EngineRegistry` 同形）
pub struct TranslateRegistry {
    providers: Vec<Arc<dyn TranslateProvider>>,
    /// 启用的 provider id；`None` = 翻译关（[`TranslateRegistry::translate`] 零调用直接 `Ok(None)`）
    enabled: RwLock<Option<String>>,
}

impl TranslateRegistry {
    pub fn new(providers: Vec<Arc<dyn TranslateProvider>>) -> Self {
        Self {
            providers,
            enabled: RwLock::new(None),
        }
    }

    /// 首个注册项的 id（设置启用时"用哪个 provider"的缺省答案）
    pub fn default_provider(&self) -> Option<String> {
        self.providers.first().map(|p| p.id().to_owned())
    }

    pub fn set_enabled(&self, id: Option<String>) {
        *self.enabled.write() = id;
    }

    pub fn enabled(&self) -> Option<String> {
        self.enabled.read().clone()
    }

    /// 翻一次（`None` = 未启用，`Err` = provider 如实失败）
    pub fn translate(&self, text: &str, to: &str) -> Result<Option<String>, AppError> {
        let Some(id) = self.enabled() else {
            return Ok(None);
        };
        let Some(provider) = self.providers.iter().find(|p| p.id() == id) else {
            // 启用指向一个不存在的 provider 属内部状态不一致，点名而不是当没这回事
            return Err(AppError::module(
                "OCR_TRANSLATE_002",
                format!("翻译已启用，但 provider「{id}」未注册"),
                Some("在 TranslateRegistry 注册该 provider 或在设置里清空目标语言"),
            ));
        };
        provider.translate(text, to).map(Some)
    }
}

impl Default for TranslateRegistry {
    fn default() -> Self {
        Self::new(vec![Arc::new(NullTranslateProvider)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingNull {
        calls: Arc<AtomicUsize>,
    }
    impl TranslateProvider for CountingNull {
        fn id(&self) -> &'static str {
            "counting-null"
        }
        fn translate(&self, _text: &str, _to: &str) -> Result<String, AppError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(AppError::module(
                "OCR_TRANSLATE_001",
                "v1 未接入翻译服务（D-08）",
                None,
            ))
        }
    }

    struct FakeProvider;
    impl TranslateProvider for FakeProvider {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn translate(&self, text: &str, to: &str) -> Result<String, AppError> {
            Ok(format!("[{to}]{text}"))
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-13）字面测试名优先于 rustc 命名惯例
    fn translateSlot_absent_returnsNoneNotError() {
        let calls = Arc::new(AtomicUsize::new(0));
        let reg = TranslateRegistry::new(vec![Arc::new(CountingNull {
            calls: calls.clone(),
        })]);
        // 未启用（默认态）：None 且零调用——设置里没填目标语言就不该碰 provider
        assert_eq!(reg.translate("你好", "en-US").unwrap(), None);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-13）字面测试名优先于 rustc 命名惯例
    fn translateSlot_providerErr_propagatesNotSwallowed() {
        // 红线：启用后 provider 的错误必须原样上抛，不折成 None（吞掉的错误等于假装没事）
        let reg = TranslateRegistry::new(vec![Arc::new(NullTranslateProvider)]);
        reg.set_enabled(Some(NULL_PROVIDER_ID.into()));
        let err = reg.translate("你好", "en-US").unwrap_err();
        assert_eq!(err.code(), "OCR_TRANSLATE_001");
        assert!(err.to_string().contains("D-08"), "{err}");

        // 正对照：槽不是死码——注册一个真 provider 即出译值，消费通路无需改动
        let working = TranslateRegistry::new(vec![
            Arc::new(FakeProvider),
            Arc::new(NullTranslateProvider),
        ]);
        working.set_enabled(working.default_provider());
        assert_eq!(
            working.translate("你好", "en-US").unwrap().as_deref(),
            Some("[en-US]你好")
        );

        // 启用指向不存在的 id：同样如实报错，不静默
        let ghost = TranslateRegistry::new(vec![Arc::new(FakeProvider)]);
        ghost.set_enabled(Some("nope".into()));
        assert_eq!(
            ghost.translate("你好", "en-US").unwrap_err().code(),
            "OCR_TRANSLATE_002"
        );
    }
}
