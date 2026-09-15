//! C1 类型定义（docs/impl/02 C1）

use serde::{Deserialize, Serialize};

/// 列表条目（IPC 返回给前端的形态）
#[derive(Clone, Debug, Serialize)]
pub struct ClipEntry {
    pub id: String,
    pub content_type: &'static str, // "text" | "files"
    /// 列表预览：文本前 200 字符 / 敏感遮蔽
    pub preview: String,
    /// >64KB 内容走 blob 时的引用路径
    pub blob_path: Option<String>,
    /// local | remote（防同步循环，docs/impl/02 C3 ③）
    pub origin: &'static str,
    pub source_app: Option<String>,
    pub pinned: bool,
    pub group: Option<&'static str>,
    /// 命中敏感规则（内容加密存储）
    pub secret: bool,
    pub created_at: i64,
    pub usage_count: u32,
}

/// 搜索查询（C6）
#[derive(Debug, Deserialize, Default)]
pub struct SearchQuery {
    pub text: Option<String>,
    pub group: Option<String>,
    pub page: Option<u32>,
    pub size: Option<u32>,
}

/// 分页结果
#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub has_more: bool,
    pub total: Option<u32>,
}

/// 模块配置（与 config_schema 一一对应）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardConfig {
    pub max_entries: u32,
    pub retention_days: u32,
    pub capture_images: bool,
    pub capture_files: bool,
    pub sensitive_filter: bool,
    pub auto_group: bool,
    pub excluded_apps: Vec<String>,
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
        }
    }
}

/// 超过该大小的内容转 blob 存储（DESIGN O4）
pub const BLOB_THRESHOLD: usize = 64 * 1024;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
