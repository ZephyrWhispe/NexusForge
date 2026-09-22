//! 类型与配置（docs/impl/03 P1 / P8 IPC DTO）

use serde::{Deserialize, Serialize};

/// 截图模块配置（设置中心 schema 渲染）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotConfig {
    /// 保存目录；空字符串 = {appData}/screenshots
    #[serde(default)]
    pub save_dir: String,
    /// 文件名模板；{ts} 占位符替换为 yyyy-MM-dd_HHmmss，{fmt} 替换为实际扩展名
    #[serde(default = "default_filename_template")]
    pub filename_template: String,
    /// 导出格式：png | jpeg | webp（写侧唯一入口 `util::encode_rgba`，未知值点名拒）
    #[serde(default = "default_format")]
    pub format: String,
    /// 编码质量 1..=100。**本代只有 JPEG 消费它**：`image` 0.25 的 WebP 编码器只有
    /// VP8L 无损档，PNG 本身无质量概念——schema 的 description 与本键的 doc 都说实话，
    /// 而不是摆一个只对三分之一格式生效却宣称通用的滑块
    #[serde(default = "default_quality")]
    pub quality: u8,
    /// 完成后自动复制到剪贴板
    #[serde(default = "default_true")]
    pub auto_copy: bool,
    /// 完成后自动保存文件
    #[serde(default = "default_true")]
    pub auto_save: bool,
    /// 完成后进入贴图（与 auto_save 共存，保存仍执行）
    #[serde(default)]
    pub auto_pin: bool,
    /// 完成后动作链（D-29 B4 T-B4-8）：`auto_*` 三 bool 的**替代真源**——非空时三 bool
    /// 全被忽略（它们只在新键缺席时参与派生，见 `effective_actions`）。
    /// 元素须在 [`POST_ACTION_WHITELIST`] 内：写侧 `apply_config` 点名拒，
    /// 运行期（旧快照里残留的坏值）`effective_actions` warn 后丢。
    #[serde(default)]
    pub post_actions: Vec<String>,
}

/// `post_actions` 的合法元素集：写侧拒与运行期 warn 读同一份词表（两处各写一份，
/// 加一个动作就会变成"设置里存得进、跑起来默默丢"）
pub const POST_ACTION_WHITELIST: &[&str] = &["save", "copy", "pin", "ocr", "beautify"];

/// 三 bool 的派生序：save→copy→pin，与 T-B4-8 之前 `finish()` 里那段兜底的字面顺序一致，
/// 旧快照因此在升级前后拿到同一份动作链——这就是"零迁移"的全部内容
fn actions_from_bools(cfg: &ScreenshotConfig) -> Vec<String> {
    let mut out = Vec::with_capacity(3);
    if cfg.auto_save {
        out.push("save".into());
    }
    if cfg.auto_copy {
        out.push("copy".into());
    }
    if cfg.auto_pin {
        out.push("pin".into());
    }
    out
}

/// 完成后动作链的**唯一决策点**（纯函数：只读 cfg 与 req，不改任何东西、也不落盘）
///
/// 三级优先，次序即语义：
/// 1. `req` 非空 → 原样交出。用户在覆盖层当面点的那颗钮大于任何全局偏好。
/// 2. `cfg.post_actions` 非空 → 白名单过滤后的它。**过滤后为空也是空**，
///    不回落三 bool：回落等于同时承认两个真源，"设置里选了不保存、却仍在写盘"
///    就是这么漂出来的。
/// 3. 否则由三 bool 派生（旧快照零迁移臂）。
pub fn effective_actions(cfg: &ScreenshotConfig, req: &[String]) -> Vec<String> {
    if !req.is_empty() {
        return req.to_vec();
    }
    if !cfg.post_actions.is_empty() {
        return cfg
            .post_actions
            .iter()
            .filter(|a| {
                let known = POST_ACTION_WHITELIST.contains(&a.as_str());
                if !known {
                    // 运行期兜底：写侧已拒，坏值只可能来自手改 JSON 或升级前残留
                    tracing::warn!(
                        action = %a,
                        whitelist = POST_ACTION_WHITELIST.join("、"),
                        "post_actions 含未知动作，本次忽略（不影响同批其余动作）"
                    );
                }
                known
            })
            .cloned()
            .collect();
    }
    actions_from_bools(cfg)
}

fn default_filename_template() -> String {
    "shot_{ts}".into()
}
fn default_format() -> String {
    "png".into()
}
fn default_quality() -> u8 {
    80
}
fn default_true() -> bool {
    true
}

impl Default for ScreenshotConfig {
    fn default() -> Self {
        Self {
            save_dir: String::new(),
            filename_template: default_filename_template(),
            format: default_format(),
            quality: default_quality(),
            auto_copy: true,
            auto_save: true,
            auto_pin: false,
            post_actions: Vec::new(),
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
    /// 本次任务的定位矩形（物理像素），覆盖层窗口定位用：
    /// 全屏轨 = 虚拟桌面原点与整幅；窗口轨 = 目标窗的 `GetWindowRect`
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// 配置真源直达覆盖层（D-29 B4 T-B4-8）：= `effective_actions(cfg, &[])`，
    /// 覆盖层"完成"钮按它传动作。**不给 overlay 开读设置的口子**（§9.1-⑪）——
    /// 偏好要出现在覆盖层上，就得经过这里，否则覆盖层要么自己 invoke 一次 config_get
    /// （多一条 ACL 面），要么写死一套动作（和设置漂移）。
    pub default_actions: Vec<String>,
}

/// 可选窗口（`screenshot_windows` 出口，D-29 B4 T-B4-4）。
///
/// 与 `host_core::ports::WindowTarget` 同名同型是刻意的：这一层只是把端口结构换成
/// "模块自己的 DTO"，好让命令面签名不泄漏 host-core 类型；字段一旦开始分叉，
/// 就再没人看得出下拉里那行标题和实际窗口是不是同一件事。
#[derive(Debug, Serialize)]
pub struct WindowTargetDto {
    pub hwnd: i64,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub minimized: bool,
}

impl From<host_core::ports::WindowTarget> for WindowTargetDto {
    fn from(w: host_core::ports::WindowTarget) -> Self {
        Self {
            hwnd: w.hwnd,
            title: w.title,
            x: w.x,
            y: w.y,
            width: w.width,
            height: w.height,
            minimized: w.minimized,
        }
    }
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
    /// 窗口轨句柄（D-29 B4 T-B4-4）；`None` = 全屏轨。
    /// 覆盖层据此跳过拖框、把选区初值铺成整窗。它是这条事实的**唯一**携带者：
    /// 装载覆盖层的两条路（预热事件带 `TaskStartDto`、URL 回退带 `?task=`）都必然取一次帧，
    /// 挂在这里就等于"两条路同一个判据"，不必再给回退路径加一个 URL 参数。
    pub hwnd: Option<i64>,
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
    /// save | copy | pin | ocr | beautify（ocr 只发布 screenshot.ocr_requested 事件，
    /// 识别结果经 ocr.completed 异步回流，见 D-09；beautify 不产副作用，像素在
    /// 动作循环之前就已作用于最终图，见 T-B4-8）
    #[serde(default)]
    pub actions: Vec<String>,
    /// Pin 初始位置（屏幕物理像素）
    #[serde(default)]
    pub pin_x: Option<i32>,
    #[serde(default)]
    pub pin_y: Option<i32>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    /// 本次保存的导出格式（"png" | "jpeg" | "webp"，大小写不敏感）；
    /// None = 用配置里的 `format`。未知值不回落默认，点名拒（`SCREENSHOT_FORMAT_001`）
    #[serde(default)]
    pub format: Option<String>,
    /// 美化参数（D-29 B4 T-B4-6）：None = 不美化。在场即在动作循环**之前**作用于
    /// 合成图，因此 copy/save/pin/ocr 四面看到的是同一张美化后的图——分叉正是
    /// "预览是原图、保存是美化图"这类报告的根因
    #[serde(default)]
    pub beautify: Option<crate::beautify::BeautifySpec>,
}

#[derive(Debug, Serialize)]
pub struct FinishDto {
    pub file: Option<String>,
    pub pin_id: Option<String>,
    /// 预览字节（PNG Base64）：**只有** `screenshot_beautify_apply` 的空 actions 分支填它，
    /// 正常完成动作一律 None（`skip_serializing_if` 让旧前端 DTO 形状零变化）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_b64: Option<String>,
}

/// 单条截图历史的原始字节出口（D-29 B0-2：主面板缩略图/再复制；历史表只存路径）
///
/// `annotations` 为当年入库的标注矢量（D-29 B4 T-B4-1"收即持久"）：旧行为 NULL，
/// 读回空表而非报错——空标注与无标注在列上同为 NULL，因此历史列不必承担语义分裂。
///
/// `png_b64` 是历史命名，实为**文件原始字节**的 Base64：T-B4-7 之后编码由
/// `format` 键声明（png/jpeg/webp），读侧一律按内容嗅探而不是信后缀。
#[derive(Debug, Clone, Serialize)]
pub struct ShotDataDto {
    pub id: String,
    pub png_b64: String,
    /// 文件字节的 MIME（"image/png" | "image/jpeg" | "image/webp" | "unknown"），
    /// 由读侧按魔数嗅探得出——前端据此拼 data URL，不靠后缀猜
    pub format: String,
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

    fn cfg_with(post_actions: &[&str], save: bool, copy: bool, pin: bool) -> ScreenshotConfig {
        ScreenshotConfig {
            post_actions: post_actions.iter().map(|s| (*s).to_owned()).collect(),
            auto_save: save,
            auto_copy: copy,
            auto_pin: pin,
            ..Default::default()
        }
    }

    fn strs(v: &[String]) -> Vec<&str> {
        v.iter().map(|s| s.as_str()).collect()
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-8）字面测试名优先于 rustc 命名惯例
    fn effectiveActions_explicitRequest_winsOverConfig() {
        // 用户在覆盖层当面点的钮 > 全局偏好：配置写"保存"而这次点"复制"，就得只复制
        let cfg = cfg_with(&["save"], true, true, true);
        let got = effective_actions(&cfg, &["copy".to_owned()]);
        assert_eq!(strs(&got), ["copy"]);
        // 正对照：空请求才轮到配置（否则本测试可以是"req 永远原样返回"的空洞实现）
        assert_eq!(strs(&effective_actions(&cfg, &[])), ["save"]);
        // 第二级胜过第三级：请求为空时 post_actions 顶掉三 bool
        let cfg2 = cfg_with(&["ocr"], true, true, true);
        assert_eq!(strs(&effective_actions(&cfg2, &[])), ["ocr"]);
    }

    #[test]
    #[allow(non_snake_case)]
    fn effectiveActions_legacyBools_deriveInStableOrder() {
        // 旧快照零迁移臂：只有三 bool 的配置（post_actions 缺席）→ 升级前的字面顺序
        let cfg = cfg_with(&[], true, true, true);
        assert_eq!(strs(&effective_actions(&cfg, &[])), ["save", "copy", "pin"]);
        // 顺序判据不是集合判据：save→copy→pin 与 T-B4-8 之前 finish() 里那三段 push 逐字对齐
        let cfg = cfg_with(&[], false, true, true);
        assert_eq!(strs(&effective_actions(&cfg, &[])), ["copy", "pin"]);
        // 全负：三开关全关就是真的没事做（不是回落某个"默认动作"）
        let cfg = cfg_with(&[], false, false, false);
        assert!(effective_actions(&cfg, &[]).is_empty());
        // 默认配置（首次运行、没写过设置）：auto_save+auto_copy 开、auto_pin 关
        assert_eq!(
            strs(&effective_actions(&ScreenshotConfig::default(), &[])),
            ["save", "copy"]
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn effectiveActions_postActionsEmptyWithBools_prefersConfigKey() {
        // 单一真源：post_actions 在场（非空）时三 bool 全被忽略——两处都可写就会漂出
        // "设置里选了不保存、却仍在写盘"。auto_pin=true 而新键里没有 pin，pin 就不该出现。
        let cfg = cfg_with(&["copy", "ocr"], true, false, true);
        assert_eq!(strs(&effective_actions(&cfg, &[])), ["copy", "ocr"]);
        // 新键**为空**时三 bool 重新生效（本名的另一臂：空名单 = 未配置，不是"关掉一切"）
        let cfg = cfg_with(&[], true, false, true);
        assert_eq!(strs(&effective_actions(&cfg, &[])), ["save", "pin"]);
        // 边界如实登记：新键非空但全是不认识的名称时**不**回落三 bool（见下一个测试）
        let cfg = cfg_with(&["teleport"], true, true, true);
        assert!(effective_actions(&cfg, &[]).is_empty());
    }

    #[test]
    #[allow(non_snake_case)]
    fn effectiveActions_unknownName_droppedWithWarn_notStored() {
        // 运行期兜底臂（写侧 apply_config 已拒，坏值只能来自手改 JSON）：坏值丢、好值留
        let cfg = cfg_with(&["copy", "teleport", "save"], true, true, true);
        let got = effective_actions(&cfg, &[]);
        assert_eq!(
            strs(&got),
            ["copy", "save"],
            "越界项丢掉后其余动作照常按原序执行"
        );
        assert!(!got.iter().any(|a| a == "teleport"));
        // "notStored"：纯函数不改配置——过滤发生在读出侧，而不是顺手把用户配置改写了。
        // 真把坏值写回需要 &mut，而那条路会让设置页每次打开都静默吃掉一格用户的输入
        assert_eq!(strs(&cfg.post_actions), ["copy", "teleport", "save"]);
        // 词表自身：五枚动作名一个不多一个不少（新增动作要连白名单一起改，这里先钉住形状）
        assert_eq!(
            POST_ACTION_WHITELIST,
            ["save", "copy", "pin", "ocr", "beautify"]
        );
    }
}
