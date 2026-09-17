//! notes-core 数据模型（docs/impl/06 N1–N5）

use serde::{Deserialize, Serialize};

/// 笔记元数据（N1 索引行；path 为 `/` 分隔的库内相对路径，不含 .md 的一律存全名含扩展名）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NoteMeta {
    pub path: String,
    pub title: String,
    pub tags: Vec<String>,
    /// 毫秒
    pub mtime_ms: i64,
    pub size: u64,
}

/// 双链记录（N2）：dst 为链接原文，dst_path 为解析结果（未解析为空串）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LinkRec {
    pub src: String,
    pub dst: String,
    pub dst_path: String,
}

/// 反链（N2）：指向 note 的来源 + 上下文片段
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Backlink {
    pub src: String,
    pub title: String,
    /// 命中链接所在行（trim）
    pub snippet: String,
}

/// 复习卡片（N4 SM-2 简化版）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    /// 关联笔记（可空）
    pub note_path: Option<String>,
    pub front: String,
    pub back: String,
    /// easiness factor，下限 1.3
    pub ef: f64,
    /// 当前间隔天数
    pub interval_days: i64,
    /// 连续答对次数（q<3 归零）
    pub reps: i64,
    /// 下次到期毫秒
    pub due_ms: i64,
}

/// 画布节点（N3）：kind=note 引用笔记 / sticky 便签 / image 图片
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanvasNode {
    pub id: String,
    pub kind: String,
    pub x: f64,
    pub y: f64,
    #[serde(default = "default_w")]
    pub w: f64,
    #[serde(default = "default_h")]
    pub h: f64,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub src: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

fn default_w() -> f64 {
    180.0
}
fn default_h() -> f64 {
    80.0
}

/// 画布有向边
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanvasEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// 画布文档（.nforge-canvas.json，与 md 同目录；损坏时按空画布处理仅丢画布不丢笔记）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanvasDoc {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub nodes: Vec<CanvasNode>,
    #[serde(default)]
    pub edges: Vec<CanvasEdge>,
}

fn default_version() -> u32 {
    1
}

impl Default for CanvasDoc {
    fn default() -> Self {
        Self { version: 1, nodes: vec![], edges: vec![] }
    }
}

/// sync/reindex 结果
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncResult {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub total: usize,
}
