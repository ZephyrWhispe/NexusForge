//! C1 类型定义（docs/impl/02 C1）

use serde::{Deserialize, Serialize};

/// 列表条目（IPC 返回给前端的形态）
#[derive(Clone, Debug, Serialize)]
pub struct ClipEntry {
    pub id: String,
    pub content_type: String, // "text" | "files" | "image"
    /// 列表预览：文本前 200 字符 / 敏感遮蔽
    pub preview: String,
    /// >64KB 内容走 blob 时的引用路径
    pub blob_path: Option<String>,
    /// local | remote（防同步循环，docs/impl/02 C3 ③）
    pub origin: &'static str,
    pub source_app: Option<String>,
    pub pinned: bool,
    /// 用户分组名（T-B3-4：组名来自用户/分类器，可为任意 Unicode，不再是固定枚举）
    pub group: Option<String>,
    /// 命中敏感规则（内容加密存储）
    pub secret: bool,
    pub created_at: i64,
    pub usage_count: u32,
}

/// 智能分组建议（01§5-1：建议制非自动改，采纳/忽略都由用户点）
#[derive(Debug, Serialize)]
pub struct SuggestionDto {
    pub entry_id: String,
    pub preview: String,
    pub suggested_group: String,
    pub confidence: f32,
}

/// 统计卡（SettingsSection）：全部为库内聚合，无估算
#[derive(Debug, Serialize)]
pub struct StatsDto {
    pub total: u32,
    pub by_content_type: serde_json::Value,
    pub by_group: serde_json::Value,
    pub top_source_apps: Vec<(String, u32)>,
    /// 内联 content 字节 + blob 文件实际字节
    pub bytes_blob: u64,
}

/// 敏感桶类目标签（T-B3-5）：v1 只到"敏感内容"这一档，**不谎报具体密钥类型**——
/// `SecretKind` 精类目标签需 clip_entries 新列，已 [收窄] 登记归 B7（09 §8.2 T-B3-10 ④）。
pub const SECRET_CATEGORY_LABEL: &str = "敏感内容";

/// 搜索查询（C6）
#[derive(Debug, Deserialize, Default)]
pub struct SearchQuery {
    pub text: Option<String>,
    pub group: Option<String>,
    pub page: Option<u32>,
    pub size: Option<u32>,
    /// 内容类型显式筛选面（前端芯片用）。与 `text` 里的 `type:` 语法同源同效，
    /// 两者同时在场且不一致时**语法优先**——用户当场敲进搜索框的那句才是本次意图
    /// （见 store.rs search 的 `syntax.content_type.or(q.content_type)`）。
    #[serde(default)]
    pub content_type: Option<String>,
}

/// 分页结果
#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub has_more: bool,
    pub total: Option<u32>,
}

/// 模块配置（与 config_schema 一一对应）
///
/// `serde(default)` 是配置真源接线（09 §8.1-①）的前提：schema 无 `required`，
/// 盘上允许只写了部分键的合法文件，缺键须回落默认值而不是整份配置反序列化失败。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardConfig {
    pub max_entries: u32,
    pub retention_days: u32,
    pub capture_images: bool,
    pub capture_files: bool,
    pub sensitive_filter: bool,
    pub auto_group: bool,
    pub excluded_apps: Vec<String>,
    /// 暂停捕获：新复制不入库，已有历史与设置不受影响（§8-④）
    pub capture_paused: bool,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            max_entries: 5000,
            retention_days: 30,
            capture_images: true,
            capture_files: true,
            sensitive_filter: true,
            auto_group: true,
            excluded_apps: vec![],
            capture_paused: false,
        }
    }
}

/// 超过该大小的内容转 blob 存储（DESIGN O4）
pub const BLOB_THRESHOLD: usize = 64 * 1024;

pub use host_core::util::now_ms;
