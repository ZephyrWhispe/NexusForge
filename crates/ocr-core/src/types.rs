//! OCR 类型与 DTO（docs/impl/04 O1/O7）

use serde::{Deserialize, Serialize};

/// 识别请求（overlay 直接 IPC；PNG Base64）
#[derive(Debug, Clone, Deserialize)]
pub struct OcrRequest {
    pub image_b64: String,
    /// 逐次覆盖的语言（BCP-47，如 zh-CN / en-US）；空 = 取用户配置语言，
    /// 两者皆空才落到引擎自选（优先级单一决策点在 engine::resolve_langs）
    #[serde(default)]
    pub langs: Vec<String>,
    /// 关联截图任务 id（历史回填用，可空）
    #[serde(default)]
    pub source_task_id: Option<String>,
}

/// 模块配置（写侧唯一入口仍是 `host_config_set`；本结构是 schema 各键的强类型投影）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrConfig {
    /// 偏好语言（BCP-47，按序）；空 = 引擎按系统语言自选
    #[serde(default)]
    pub langs: Vec<String>,
    #[serde(default = "default_preferred")]
    pub preferred_engine: String,
    /// 以下四键 = Tesseract CLI 第二引擎（T-B4-11，默认关；扁平命名而非 object 子键，
    /// 因为设置面 SchemaForm 只渲染 bool/int/string/array 四形，渲染不出的设置等于没有设置）
    #[serde(default)]
    pub tesseract_enabled: bool,
    #[serde(default)]
    pub tesseract_exe: String,
    #[serde(default)]
    pub tesseract_data_dir: Option<String>,
    #[serde(default = "default_tess_timeout")]
    pub tesseract_timeout_ms: u64,
    /// 译文目标语言（BCP-47）；空 = 不译（T-B4-13 槽位，v1 无在线 provider）
    #[serde(default)]
    pub translate_target_lang: String,
}

fn default_preferred() -> String {
    "win-ocr".into()
}

fn default_tess_timeout() -> u64 {
    20_000
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            langs: vec![],
            preferred_engine: default_preferred(),
            tesseract_enabled: false,
            tesseract_exe: String::new(),
            tesseract_data_dir: None,
            tesseract_timeout_ms: default_tess_timeout(),
            translate_target_lang: String::new(),
        }
    }
}

/// 配置补丁（写侧的"缺键 = 不动"显式形：整份反序列化会把"未提供"与"提供默认值"混为一谈）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcrConfigPatch {
    pub langs: Option<Vec<String>>,
    pub preferred_engine: Option<String>,
    pub tesseract_enabled: Option<bool>,
    pub tesseract_exe: Option<String>,
    pub tesseract_data_dir: Option<String>,
    pub tesseract_timeout_ms: Option<u64>,
    pub translate_target_lang: Option<String>,
}

impl OcrConfig {
    /// 应用补丁得到生效配置（None 键保留现值）
    pub fn merged(&self, patch: &OcrConfigPatch) -> OcrConfig {
        OcrConfig {
            langs: patch.langs.clone().unwrap_or_else(|| self.langs.clone()),
            preferred_engine: patch
                .preferred_engine
                .clone()
                .unwrap_or_else(|| self.preferred_engine.clone()),
            tesseract_enabled: patch.tesseract_enabled.unwrap_or(self.tesseract_enabled),
            tesseract_exe: patch
                .tesseract_exe
                .clone()
                .unwrap_or_else(|| self.tesseract_exe.clone()),
            tesseract_data_dir: patch
                .tesseract_data_dir
                .clone()
                .or_else(|| self.tesseract_data_dir.clone()),
            tesseract_timeout_ms: patch
                .tesseract_timeout_ms
                .unwrap_or(self.tesseract_timeout_ms),
            translate_target_lang: patch
                .translate_target_lang
                .clone()
                .unwrap_or_else(|| self.translate_target_lang.clone()),
        }
    }

    /// Tesseract 四键的引擎侧投影（键名去掉前缀：住在本模块命名空间里前缀是噪音）
    pub fn tesseract(&self) -> crate::tesseract::TesseractSettings {
        crate::tesseract::TesseractSettings {
            enabled: self.tesseract_enabled,
            exe: self.tesseract_exe.clone(),
            data_dir: self.tesseract_data_dir.clone(),
            timeout_ms: self.tesseract_timeout_ms,
        }
    }
}

/// 运行态配置快照（`ocr_config_get` 只读，不为同一语义开第二可写口）
pub type OcrConfigDto = OcrConfig;

/// 识别结果 DTO
#[derive(Debug, Clone, Serialize)]
pub struct OcrResultDto {
    pub lines: Vec<OcrLineDto>,
    /// 段落重建后的全文（UI "复制全部"用）
    pub text: String,
    /// 实际使用的语言（引擎解析结果）
    pub lang: String,
    /// 实际使用的引擎 id
    pub engine: String,
    /// 本次实际所用引擎是否真的输出置信度（**结果级**而非行级：一次识别只走一个引擎，
    /// 行级布尔只是 N 份相同字节）。false 时 `lines[].confidence` 是"未知"的占位值，
    /// 前端据此显示"未提供"而不是把 1.0 渲染成 100.0%（T-B4-13 / §9.1-⑫(b)）
    pub engines_report_confidence: bool,
    /// 译文（T-B4-13 槽位：设置里未设目标语言或槽未启用 ⇒ 字段整个不出现在 JSON 里）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translate: Option<String>,
    /// 翻译失败原因（与 `translate` 互斥的诚实面：错误必须看得见，但不至于让识别失败）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translate_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OcrLineDto {
    pub text: String,
    /// 归一化 0..1
    pub rect: RectF,
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// 引擎状态（docs/impl/04 O8）
#[derive(Debug, Clone, Serialize)]
pub struct EngineStatusDto {
    pub engines: Vec<EngineInfo>,
    /// win-ocr 可识别语言列表（BCP-47）
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineInfo {
    pub id: String,
    pub name: String,
    pub available: bool,
}
