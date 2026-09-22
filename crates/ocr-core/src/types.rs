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

/// 模块配置（写侧唯一入口仍是 `host_config_set`；本结构是 schema 两键的强类型投影）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrConfig {
    /// 偏好语言（BCP-47，按序）；空 = 引擎按系统语言自选
    #[serde(default)]
    pub langs: Vec<String>,
    #[serde(default = "default_preferred")]
    pub preferred_engine: String,
}

fn default_preferred() -> String {
    "win-ocr".into()
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            langs: vec![],
            preferred_engine: default_preferred(),
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
