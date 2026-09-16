//! 类型与配置（docs/impl/03 P1 / P8 IPC DTO）

use serde::{Deserialize, Serialize};

/// 截图模块配置（设置中心 schema 渲染）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotConfig {
    /// 保存目录；空字符串 = {appData}/screenshots
    #[serde(default)]
    pub save_dir: String,
    /// 文件名模板；{ts} 占位符替换为 yyyy-MM-dd_HHmmss
    #[serde(default = "default_filename_template")]
    pub filename_template: String,
    /// 完成后自动复制到剪贴板
    #[serde(default = "default_true")]
    pub auto_copy: bool,
    /// 完成后自动保存文件
    #[serde(default = "default_true")]
    pub auto_save: bool,
    /// 完成后进入贴图（与 auto_save 共存，保存仍执行）
    #[serde(default)]
    pub auto_pin: bool,
}

fn default_filename_template() -> String {
    "shot_{ts}".into()
}
fn default_true() -> bool {
    true
}

impl Default for ScreenshotConfig {
    fn default() -> Self {
        Self {
            save_dir: String::new(),
            filename_template: default_filename_template(),
            auto_copy: true,
            auto_save: true,
            auto_pin: false,
        }
    }
}

/// 标注（前端 canvas 坐标归一化 0..1；最终合成图由前端导出，Rust 仅存档）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    /// pen | rect | ellipse | arrow | text | mosaic | number
    pub kind: String,
    /// "#RRGGBB"
    #[serde(default)]
    pub color: String,
    /// 线宽（相对导出图像像素）
    #[serde(default)]
    pub width: f32,
    #[serde(default)]
    pub points: Vec<(f32, f32)>,
    #[serde(default)]
    pub text: Option<String>,
    /// number 工具的序号
    #[serde(default)]
    pub seq: Option<u32>,
}

// ---------------- IPC DTO（docs/impl/03 P8）----------------

#[derive(Debug, Serialize)]
pub struct TaskStartDto {
    pub task_id: String,
    /// 虚拟桌面原点与尺寸（物理像素），覆盖层窗口定位用
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Serialize)]
pub struct TaskInfoDto {
    pub task_id: String,
    /// shot | ocr
    pub mode: String,
    pub width: u32,
    pub height: u32,
    /// 全屏帧 PNG（Base64），覆盖层背景
    pub png_b64: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ConfirmRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Serialize)]
pub struct CropDto {
    pub png_b64: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Deserialize)]
pub struct FinishRequest {
    /// 前端 canvas 合成后的最终图（PNG Base64）——预览即导出，保证一致性
    pub image_b64: String,
    /// save | copy | pin
    #[serde(default)]
    pub actions: Vec<String>,
    /// Pin 初始位置（屏幕物理像素）
    #[serde(default)]
    pub pin_x: Option<i32>,
    #[serde(default)]
    pub pin_y: Option<i32>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Serialize)]
pub struct FinishDto {
    pub file: Option<String>,
    pub pin_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PinDto {
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub zoom: f32,
    pub opacity: f32,
}

#[derive(Debug, Serialize)]
pub struct PinDataDto {
    /// 贴图 id（前端关闭/更新须回传；缺失会导致关闭时删不掉持久化记录）
    pub id: String,
    pub png_b64: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub zoom: f32,
    pub opacity: f32,
}

/// 历史条目
#[derive(Debug, Clone, Serialize)]
pub struct ShotItem {
    pub id: String,
    pub created_ms: i64,
    pub width: u32,
    pub height: u32,
    pub file: Option<String>,
    pub ocr_text: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u32,
    pub page: u32,
    pub size: u32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct HistoryQuery {
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_size")]
    pub size: u32,
}

fn default_page() -> u32 {
    1
}
fn default_size() -> u32 {
    30
}
