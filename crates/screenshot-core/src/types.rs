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

/// 标注（前端 canvas 坐标为画布像素；最终合成图由前端导出，Rust 仅存档）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    /// pen | rect | ellipse | line | arrow | highlight | text | mosaic | number | blur
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
    /// z 序：小在下先绘制。缺省 0 让旧前端/旧历史条目零迁移即可读
    #[serde(default)]
    pub layer: u32,
    /// 锁定：仍绘制，但点选穿透（选择穿透 = hitTest 的 skipLocked 臂）
    #[serde(default)]
    pub locked: bool,
    /// rect/ellipse 是否实心填充（其余类型不消费此键，绘制侧按各自语义走）
    #[serde(default)]
    pub fill: bool,
    /// 不透明度 0..=1。**独立键而非 `#RRGGBBAA` 字符串扩展**（§9.1-⑤）：色值解析点在
    /// 前端多处，扩字符串要改全部解析点；缺省 1.0 让旧条目零迁移即可读
    #[serde(default = "default_alpha")]
    pub alpha: f32,
}

fn default_alpha() -> f32 {
    1.0
}

impl Annotation {
    /// 落库序：`layer` 升序的稳定排序（同层保持入参序）。
    ///
    /// 稳定性是这一排序的全部要点——`sort_unstable_by_key` 在同层条目上会给出不连续
    /// 的次序，撤销/重做与图层面板就会看到标注"自己乱动"。
    pub fn sort_by_layer(list: &[Annotation]) -> Vec<Annotation> {
        let mut out = list.to_vec();
        out.sort_by_key(|a| a.layer);
        out
    }
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
    /// save | copy | pin | ocr（ocr 只发布 screenshot.ocr_requested 事件，
    /// 识别结果经 ocr.completed 异步回流，见 D-09）
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

/// 单条截图历史的 PNG 字节出口（D-29 B0-2：主面板缩略图/再复制；历史表只存路径）
///
/// `annotations` 为当年入库的标注矢量（D-29 B4 T-B4-1"收即持久"）：旧行为 NULL，
/// 读回空表而非报错——空标注与无标注在列上同为 NULL，因此历史列不必承担语义分裂。
#[derive(Debug, Clone, Serialize)]
pub struct ShotDataDto {
    pub id: String,
    pub png_b64: String,
    pub annotations: Vec<Annotation>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-2）字面测试名优先于 rustc 命名惯例
    fn annotationFillAlpha_deserializeDefaultsForLegacyJson() {
        // 旧三键 JSON（T-B4-1 之前的形状）直进新结构体：四个新键全走 default
        let legacy: Annotation = serde_json::from_str(
            r##"{"kind":"rect","color":"#ff4d4f","width":4,"points":[[0,0],[1,1]]}"##,
        )
        .unwrap();
        assert_eq!(legacy.layer, 0);
        assert!(!legacy.locked);
        assert!(!legacy.fill, "缺 fill 必须是不填充，而不是把形状糊成实心");
        assert_eq!(legacy.alpha, 1.0, "缺 alpha 必须是全不透明");
        // 正对照：新键真能读进来（否则上面的 default 断言可以是"任何输入都走 default"）
        let fresh: Annotation = serde_json::from_str(
            r##"{"kind":"rect","color":"#ff4d4f","width":4,"points":[],"fill":true,"alpha":0.35,"layer":3,"locked":true}"##,
        )
        .unwrap();
        assert!(fresh.fill);
        assert_eq!(fresh.alpha, 0.35);
        assert_eq!((fresh.layer, fresh.locked), (3, true));
        // 出口形状稳定：前端按同一批键读
        let json = serde_json::to_string(&fresh).unwrap();
        for key in [
            "\"fill\":true",
            "\"alpha\":0.35",
            "\"layer\":3",
            "\"locked\":true",
        ] {
            assert!(json.contains(key), "序列化缺 {key}：{json}");
        }
    }
}
