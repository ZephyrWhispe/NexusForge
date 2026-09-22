//! ScreenshotModule：截图任务状态机 + Pin 贴图 + 历史记录（docs/impl/03 P2/P5/P6/P8）
//!
//! 任务流（覆盖层前端驱动，Rust 持有帧数据）：
//! start_capture → 覆盖层 task_info → 选区 confirm → 标注 → finish(合成图+动作)
//!
//! v1 简化（相对 docs/impl/03）：
//! - 捕获走 GDI BitBlt（win-integration capture.rs），Windows.Graphics.Capture 后续迭代；
//! - 最终合成图由前端 canvas 导出（预览即导出），Rust 侧 skia/ab_glyph 渲染延后；
//! - 录屏（P7）独立里程碑交付。

use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::PathBuf;

use std::sync::Arc;

use host_core::capability::{
    HotkeyAction, HotkeyBinding, HotkeyProvider, TrayAction, TrayMenuItem, TrayProvider,
};
use host_core::error::{AppError, ModuleError};
use host_core::events::{Event, EventBus};
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};
use host_core::ports::{CapturePort, CaptureTarget, ClipboardPort};
use tokio::sync::watch;
use tokio::sync::Mutex as AsyncMutex;

use crate::store::ShotStore;
use crate::types::{
    effective_actions, Annotation, ConfirmRect, CropDto, FinishDto, FinishRequest, PinDataDto,
    PinDto, ScreenshotConfig, ShotDataDto, ShotItem, TaskInfoDto, TaskStartDto, WindowTargetDto,
    POST_ACTION_WHITELIST,
};
use crate::util;

use host_core::util::app_err as mod_err;

/// OCR 联动帧的临时目录（{appData}/frames）：finish(ocr) 写入 → ocr-core 消费后删除；
/// init 时清空上次进程遗留（帧是一次性交接物，崩溃残留无保留价值）
fn frames_dir(app_data: &std::path::Path) -> std::path::PathBuf {
    app_data.join("frames")
}

/// `run_actions` 的结果（OCR 只登记意向：交接帧要 task_id，语义归调用侧）
struct ActionOutcome {
    file: Option<String>,
    pin_id: Option<String>,
    request_ocr: bool,
}

/// 一次后处理动作的入参包：像素 + 尺寸 + 动作 + 两个可选覆盖项。
/// 收成结构体而不是七个位置参数——`finish` 与 `beautify_apply` 两侧都要填同一组，
/// 位置参数让"哪一维是 pin 坐标、哪一维是格式"只能靠调用点对齐。
struct ActionRun<'a> {
    actions: &'a [String],
    w: u32,
    h: u32,
    rgba: &'a [u8],
    fmt_override: Option<&'a str>,
    pin_x: Option<i32>,
    pin_y: Option<i32>,
}

/// 从 `ocr.completed` 事件解析历史回填载荷（纯函数便于测试）。
/// `ocr_copy_text` 也发布本主题（{action:"copied"}），故必须同时具备
/// source_task_id 与 text 才算回填事件。
fn parse_ocr_backfill(ev: &host_core::events::Event) -> Option<(String, String)> {
    if ev.source != "ocr" {
        return None;
    }
    let task_id = ev.payload.get("source_task_id").and_then(|v| v.as_str())?;
    let text = ev.payload.get("text").and_then(|v| v.as_str())?;
    if task_id.is_empty() {
        return None;
    }
    Some((task_id.to_owned(), text.to_owned()))
}

/// 进行中的截图任务（同一时刻通常只有一个；新任务替换旧任务）
struct PendingTask {
    frame: host_core::ports::Frame,
    mode: String,
    /// 全屏帧 PNG 缓存（4K 一次编码 ~200ms，缓存避免重复编码）
    info_b64: Option<String>,
    /// 窗口轨句柄（D-29 B4 T-B4-4）；`None` = 全屏轨。随帧一起存：
    /// 覆盖层经 `task_info` 拿它决定"跳过拖框"，两条装载路径因此共用一个判据。
    hwnd: Option<i64>,
}

/// Pin 贴图记录（持久化到 {appData}/pins.json）
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PinRecord {
    id: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    zoom: f32,
    opacity: f32,
    /// 相对 {appData} 的图片路径（pins/{id}.png）
    file: String,
}

pub struct ScreenshotModule {
    app_data_dir: RwLock<Option<PathBuf>>,
    store: RwLock<Option<Arc<ShotStore>>>,
    capture: RwLock<Option<Arc<dyn CapturePort>>>,
    clipboard: RwLock<Option<Arc<dyn ClipboardPort>>>,
    bus: RwLock<Option<Arc<EventBus>>>,
    pending: Mutex<HashMap<String, PendingTask>>,
    pins: Mutex<Vec<PinRecord>>,
    config: Arc<AsyncMutex<ScreenshotConfig>>,
    state: ModuleStateCell,
    /// ocr.completed 历史回填协程的停机信道（S4 协作停机，同 automation-core）
    ocr_shutdown: RwLock<Option<watch::Sender<bool>>>,
}

impl ScreenshotModule {
    pub fn new() -> Self {
        Self {
            app_data_dir: RwLock::new(None),
            store: RwLock::new(None),
            capture: RwLock::new(None),
            clipboard: RwLock::new(None),
            bus: RwLock::new(None),
            pending: Mutex::new(HashMap::new()),
            pins: Mutex::new(Vec::new()),
            config: Arc::new(AsyncMutex::new(ScreenshotConfig::default())),
            state: ModuleStateCell::new(),
            ocr_shutdown: RwLock::new(None),
        }
    }

    fn app_data(&self) -> Option<PathBuf> {
        self.app_data_dir.read().clone()
    }

    /// pins.json 原子写（临时文件 + rename，规约 5）
    fn persist_pins(&self, pins: &[PinRecord]) -> Result<(), ModuleError> {
        let Some(dir) = self.app_data() else {
            return Ok(());
        };
        let path = dir.join("pins.json");
        let json =
            serde_json::to_vec_pretty(pins).map_err(|e| ModuleError::Storage(e.to_string()))?;
        let tmp = dir.join("pins.json.tmp");
        std::fs::write(&tmp, json).map_err(|e| ModuleError::Storage(e.to_string()))?;
        std::fs::rename(&tmp, &path).map_err(|e| ModuleError::Storage(e.to_string()))?;
        Ok(())
    }

    /// init 时恢复 pins：文件已丢失的记录直接丢弃（docs/impl/03 P6）
    fn restore_pins(&self) {
        let Some(dir) = self.app_data() else { return };
        let Ok(bytes) = std::fs::read(dir.join("pins.json")) else {
            return;
        };
        let pins: Vec<PinRecord> = serde_json::from_slice(&bytes).unwrap_or_default();
        let alive: Vec<PinRecord> = pins
            .into_iter()
            .filter(|p| dir.join(&p.file).is_file())
            .collect();
        let dropped = {
            let mut g = self.pins.lock();
            *g = alive.clone();
            alive.len()
        };
        tracing::info!(count = dropped, "Pin 贴图恢复完成");
    }
}

impl Default for ScreenshotModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ScreenshotModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "screenshot",
            name: "截图贴图",
            version: "0.1.0",
            icon: Some("screenshot"),
            priority: priority_of("screenshot"),
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let store = Arc::new(
            ShotStore::open(&ctx.app_data_dir.join("db").join("screenshot.db"))
                .map_err(|e| ModuleError::Storage(e.to_string()))?,
        );
        let capture = ctx.ports.get::<dyn CapturePort>().ok_or_else(|| {
            ModuleError::Init("CapturePort 未注册（win-integration 缺失）".into())
        })?;
        let clipboard = ctx
            .ports
            .get::<dyn ClipboardPort>()
            .ok_or_else(|| ModuleError::Init("ClipboardPort 未注册".into()))?;

        *self.app_data_dir.write() = Some(ctx.app_data_dir.clone());
        *self.store.write() = Some(store);
        *self.capture.write() = Some(capture);
        *self.clipboard.write() = Some(clipboard);
        *self.bus.write() = Some(ctx.event_bus.clone());
        self.restore_pins();
        // 上次进程遗留的 OCR 联动帧一次性清空（交接物消费即删，残留皆孤儿）
        if let Ok(entries) = std::fs::read_dir(frames_dir(&ctx.app_data_dir)) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        // D-09 第 1 步：截图联动 OCR 的历史回填端——订阅 ocr.completed
        // （ocr-core 消费 screenshot.ocr_requested 后发出），经事件单向交互，
        // 不经函数调用（DESIGN O1）；每次 start 重建停机信道（S4）
        if let (Some(bus), Some(store)) = (self.bus.read().clone(), self.store.read().clone()) {
            match bus.subscribe("ocr.completed") {
                Ok(mut rx) => {
                    let (tx, mut shutdown) = watch::channel(false);
                    *self.ocr_shutdown.write() = Some(tx);
                    tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                biased;
                                ch = shutdown.changed() => {
                                    if ch.is_err() || *shutdown.borrow_and_update() {
                                        break;
                                    }
                                }
                                received = rx.recv() => match received {
                                    Ok(event) => {
                                        if let Some((task_id, text)) = parse_ocr_backfill(&event) {
                                            if let Err(e) = store.set_ocr_text(&task_id, &text) {
                                                tracing::debug!(task_id, error = %e, "OCR 历史回填未命中记录");
                                            }
                                        }
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                        continue;
                                    }
                                    Err(_) => break,
                                },
                            }
                        }
                    });
                }
                Err(e) => tracing::warn!(error = %e, "ocr.completed 订阅失败，历史回填不可用"),
            }
        }
        self.state.set(ModuleState::Running);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        if let Some(tx) = self.ocr_shutdown.write().take() {
            tx.send(true).ok();
        }
        self.pending.lock().clear();
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "save_dir": {
                    "type": "string", "title": "保存目录",
                    "description": "留空使用应用数据目录下的 screenshots",
                    "default": ""
                },
                "filename_template": {
                    "type": "string", "title": "文件名模板",
                    "description": "{ts} 替换为时间戳，{fmt} 替换为实际扩展名（png/jpg/webp）；未知占位符原样保留",
                    "default": "shot_{ts}"
                },
                "format": {
                    "type": "string", "title": "导出格式",
                    "enum": ["png", "jpeg", "webp"], "default": "png",
                    "description": "磁盘写侧唯一编码入口 util::encode_rgba；未知值直接报错不回落 png"
                },
                "quality": {
                    "type": "integer", "title": "编码质量",
                    "minimum": 1, "maximum": 100, "default": 80,
                    "description": "仅 JPEG 生效：本代 WebP 只有 VP8L 无损档、PNG 无质量概念，故对二者无效"
                },
                "auto_copy": {
                    "type": "boolean", "title": "完成后复制",
                    "description": "截图完成后自动复制到剪贴板", "default": true
                },
                "auto_save": {
                    "type": "boolean", "title": "完成后保存",
                    "description": "截图完成后自动保存文件", "default": true
                },
                "auto_pin": {
                    "type": "boolean", "title": "完成后贴图",
                    "description": "截图完成后自动创建贴图窗口", "default": false
                },
                "post_actions": {
                    "type": "array", "title": "完成后动作链",
                    "items": { "type": "string" },
                    "default": [],
                    "description": "save/copy/pin/ocr/beautify 的有序子集；非空时上面三个「完成后…」开关全部失效（它们只在本键为空时决定动作链）。留空 = 沿用三开关"
                }
            }
        })
    }

    fn apply_config(&self, values: serde_json::Value) -> Result<(), ModuleError> {
        // 导出格式在落配置前就校验（设置是用户输入边界）：收下一个拼错的值，
        // 等于让下一次截图保存才炸——那时用户已经不记得自己在设置里填过什么。
        if let Some(raw) = values.get("format").and_then(|v| v.as_str()) {
            util::EncodeFormat::from_str_honest(raw)
                .map_err(|e| ModuleError::Config(e.to_string()))?;
        }
        // 动作链同样在落配置前校验：收下一个拼错的 "cpo" 等于让下一次截图默默少一个动作。
        // 非字符串元素一并点名（serde 的类型错只说 "invalid type"，说不出是哪一格坏）
        if let Some(arr) = values.get("post_actions").and_then(|v| v.as_array()) {
            let bad: Vec<String> = arr
                .iter()
                .filter_map(|v| match v.as_str() {
                    Some(s) if POST_ACTION_WHITELIST.contains(&s) => None,
                    Some(s) => Some(s.to_owned()),
                    None => Some(v.to_string()),
                })
                .collect();
            if !bad.is_empty() {
                return Err(ModuleError::Config(format!(
                    "post_actions 含未知动作「{}」，白名单：{}",
                    bad.join("」「"),
                    POST_ACTION_WHITELIST.join("、")
                )));
            }
        }
        let cfg: ScreenshotConfig =
            serde_json::from_value(values).map_err(|e| ModuleError::Config(e.to_string()))?;
        if let Ok(mut g) = self.config.try_lock() {
            *g = cfg;
        }
        Ok(())
    }

    fn status(&self) -> ModuleState {
        self.state.get()
    }

    fn set_status(&self, state: ModuleState) {
        self.state.set(state);
    }
}

impl HotkeyProvider for ScreenshotModule {
    fn global_hotkeys(&self) -> Vec<HotkeyBinding> {
        vec![HotkeyBinding {
            id: "screenshot.region".into(),
            label: "截图选区".into(),
            // MOD_CONTROL(0x02) | MOD_SHIFT(0x04) | MOD_NOREPEAT(0x4000)
            modifiers: 0x02 | 0x04 | 0x4000,
            vk: 0x53, // 'S'
        }]
    }

    fn hotkey_actions(&self) -> Vec<HotkeyAction> {
        let bus = self.bus.read().clone();
        let Some(bus) = bus else { return vec![] };
        vec![HotkeyAction {
            binding_id: "screenshot.region".into(),
            action: Arc::new(move || {
                tracing::info!("截图快捷键动作触发：publish screenshot.overlay_requested");
                bus.publish(Event::new(
                    "screenshot.overlay_requested",
                    "screenshot",
                    serde_json::json!({ "mode": "shot" }),
                ))
                .ok();
            }),
        }]
    }
}

impl ScreenshotModule {
    // ---------------- 任务状态机（P8 IPC 后端）----------------

    /// 抓帧开任务（阻塞 GDI 调用，命令层负责 spawn_blocking）。
    ///
    /// `hwnd: None` = 全屏轨（虚拟桌面整幅，逐字沿用旧行为）；`Some(h)` = 窗口轨
    /// （D-29 B4 T-B4-4）：抓帧走 [`CaptureTarget::Window`]（PrintWindow，被遮挡也抓得全），
    /// 定位矩形取该窗的 `GetWindowRect` 口径。覆盖层窗口因此贴在目标窗之上、背景即该窗帧，
    /// 选区坐标仍是"帧内坐标"，`confirm` 的裁剪逻辑一字节都不用改。
    pub fn start_capture(&self, mode: &str, hwnd: Option<i64>) -> Result<TaskStartDto, AppError> {
        let capture = self
            .capture
            .read()
            .clone()
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_001", "模块未就绪"))?;
        // 窗口轨先查表再抓帧：拿不到矩形就没法定位覆盖层，而"抓到了却摆错地方"比"没抓"
        // 更糟——用户会拿着一张裁错的图，且没有任何线索指向句柄这条根因。
        let window = match hwnd {
            Some(h) => {
                let w = capture
                    .list_windows()
                    .into_iter()
                    .find(|w| w.hwnd == h)
                    .ok_or_else(|| {
                        AppError::module(
                            "SCREENSHOT_WINDOW_001",
                            format!(
                                "窗口句柄 {h} 不在当前窗口表中（可能已关闭，或属于更高权限进程）"
                            ),
                            Some("请重新打开窗口列表再选择"),
                        )
                    })?;
                if w.minimized {
                    return Err(AppError::module(
                        "SCREENSHOT_WINDOW_002",
                        format!("窗口「{}」处于最小化状态，无法截取", w.title),
                        Some("请先恢复该窗口：PrintWindow 对最小化窗只给得出空图"),
                    ));
                }
                Some(w)
            }
            None => None,
        };
        let target = match &window {
            Some(w) => CaptureTarget::Window {
                hwnd: w.hwnd as isize,
            },
            None => CaptureTarget::FullScreen { monitor: 0 },
        };
        let frame = capture.capture(target)?;
        if util::is_black_frame(&frame) {
            return Err(AppError::module(
                "SCREENSHOT_CAPTURE_002",
                "捕获到黑帧：目标可能受系统保护",
                Some("受 DRM 保护的窗口无法截取"),
            ));
        }
        // 虚拟桌面 bounds（覆盖层窗口定位；副屏负坐标场景见 docs/impl/03 P2）
        let (x, y, width, height) = match &window {
            Some(w) => (w.x, w.y, w.width as i32, w.height as i32),
            None => capture
                .enumerate_monitors()
                .ok()
                .and_then(|m| {
                    m.first()
                        .map(|i| (i.x, i.y, i.width as i32, i.height as i32))
                })
                .unwrap_or((0, 0, frame.width as i32, frame.height as i32)),
        };
        let task_id = uuid::Uuid::now_v7().to_string();
        let mut pending = self.pending.lock();
        pending.clear(); // 单任务模型：替换遗留任务，释放旧帧内存
        pending.insert(
            task_id.clone(),
            PendingTask {
                frame,
                mode: mode.to_owned(),
                info_b64: None,
                hwnd,
            },
        );
        Ok(TaskStartDto {
            task_id,
            x,
            y,
            width,
            height,
            // 配置真源直达覆盖层：这里是它读偏好的唯一途径（§9.1-⑪ 不给覆盖层开 config_get）
            default_actions: effective_actions(
                &self
                    .config
                    .try_lock()
                    .map(|g| g.clone())
                    .unwrap_or_default(),
                &[],
            ),
        })
    }

    /// 窗口表透传（D-29 B4 T-B4-4）：端口缺失或端口没有枚举能力 → **空表 + warn**，
    /// 不是错误。枚举不出窗口是端口的能力边界，把它上报成一次失败会让面板弹红色 toast，
    /// 而正确反应是显"当前环境没有可截取的窗口"引导文案。
    pub fn window_targets(&self) -> Vec<WindowTargetDto> {
        let Some(capture) = self.capture.read().clone() else {
            tracing::warn!("window_targets: CapturePort 未注册，按空表处理");
            return Vec::new();
        };
        let wins = capture.list_windows();
        if wins.is_empty() {
            tracing::warn!("window_targets: 端口未给出任何窗口（枚举能力缺失或全部被过滤）");
        }
        wins.into_iter().map(WindowTargetDto::from).collect()
    }

    /// 覆盖层取背景帧（PNG Base64，编码一次后缓存）
    pub fn task_info(&self, task_id: &str) -> Result<TaskInfoDto, AppError> {
        let mut pending = self.pending.lock();
        let task = pending
            .get_mut(task_id)
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_002", "任务不存在或已结束"))?;
        let b64 = match &task.info_b64 {
            Some(b) => b.clone(),
            None => {
                let rgba = util::bgra_to_rgba(&task.frame);
                let b64 = util::encode_png_b64(task.frame.width, task.frame.height, &rgba)?;
                task.info_b64 = Some(b64.clone());
                b64
            }
        };
        Ok(TaskInfoDto {
            task_id: task_id.to_owned(),
            mode: task.mode.clone(),
            width: task.frame.width,
            height: task.frame.height,
            png_b64: b64,
            hwnd: task.hwnd,
        })
    }

    /// 选区确认：裁剪 + 编码（阻塞，命令层 spawn_blocking）
    pub fn confirm(&self, task_id: &str, rect: ConfirmRect) -> Result<CropDto, AppError> {
        let pending = self.pending.lock();
        let task = pending
            .get(task_id)
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_002", "任务不存在或已结束"))?;
        let (w, h, rgba) = util::crop_bgra(
            &task.frame,
            host_core::ports::Rect {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: rect.h,
            },
        )?;
        let png_b64 = util::encode_png_b64(w, h, &rgba)?;
        Ok(CropDto {
            png_b64,
            width: w,
            height: h,
        })
    }

    pub fn discard(&self, task_id: &str) {
        self.pending.lock().remove(task_id);
    }

    /// 完成：解码前端合成图 → （可选）美化 → 执行动作（copy/save/pin/ocr）→ 入历史 → 发事件
    ///
    /// ocr 动作不阻塞识别：帧写 {appData}/frames 后只发 `screenshot.ocr_requested`
    /// 事件，结果经 `ocr.completed` 异步回流回填历史（D-09 第 1 步）
    ///
    /// 美化（D-29 B4 T-B4-6）放在动作循环**之前**而不是各臂里各做一次：分叉的下游
    /// 就是"预览是原图、保存是美化图"这类报告，一次决议四面同源是唯一不收口就说不清的位置
    ///
    /// 动作链（D-29 B4 T-B4-8）经 `effective_actions` 一处决议：请求里显式带的动作 >
    /// 配置的 `post_actions` > 三 bool 派生。链条要能在像素决议之前读到，
    /// 所以 `beautify` 这个"只改像素、不产副作用"的动作先扫一遍名单、把美化一次做完，
    /// 循环里的 `"beautify"` 臂随之退化为 documented no-op（见 `run_actions`）。
    pub fn finish(&self, task_id: &str, req: &FinishRequest) -> Result<FinishDto, AppError> {
        let cfg = self
            .config
            .try_lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let actions = effective_actions(&cfg, &req.actions);
        let (raw_w, raw_h, raw_rgba) = util::decode_png_b64(&req.image_b64)?;
        let spec = match req.beautify.as_ref() {
            Some(s) => Some(s.clone()),
            // 名单里有 beautify 而请求没带参数 = 恒等预设（`BeautifySpec::default()` 逐字节
            // 不动图，见 beautify.rs 的 `beautify_allZeroSpec_isByteIdentical`）。
            // 这一档不是"顺手补个默认"：它让 `post_actions: ["save","beautify"]` 这种
            // 纯配置动作链在没有覆盖层参与时也成立，语义是"这条链走美化通路但这次不装饰"。
            None if actions.iter().any(|a| a == "beautify") => {
                Some(crate::beautify::BeautifySpec::default())
            }
            None => None,
        };
        let (w, h, rgba) = match spec.as_ref() {
            Some(spec) => crate::beautify::beautify(&raw_rgba, raw_w, raw_h, spec)?,
            None => (raw_w, raw_h, raw_rgba),
        };

        let outcome = self.run_actions(ActionRun {
            actions: &actions,
            w,
            h,
            rgba: &rgba,
            fmt_override: req.format.as_deref(),
            pin_x: req.pin_x,
            pin_y: req.pin_y,
        })?;
        let file = outcome.file;
        let pin_id = outcome.pin_id;

        // 入历史
        let item = crate::types::ShotItem {
            id: task_id.to_owned(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            width: w,
            height: h,
            file: file.clone(),
            ocr_text: None,
        };
        // 标注"收即持久"（D-29 B4 T-B4-1）：按 layer 升序稳定排序后整表存 JSON。
        // 排序放在这一侧而不是信任前端序——图层面板与历史读回必须同一份次序。
        let ordered = Annotation::sort_by_layer(&req.annotations);
        let annotations_json = if ordered.is_empty() {
            None
        } else {
            match serde_json::to_string(&ordered) {
                Ok(s) => Some(s),
                Err(e) => {
                    // 标注是合成图的旁证而非产物：序列化失败不该让一次截图整体失败，
                    // 但也不能静默——真因进 warn 日志（D-16 错误可见面纪律）
                    tracing::warn!(error = %e, "标注序列化失败，本次历史不存标注");
                    None
                }
            }
        };
        if let Some(store) = self.store.read().clone() {
            store.insert(&item).ok();
            if let Err(e) = store.set_annotations(&item.id, annotations_json.as_deref()) {
                tracing::warn!(error = %e, "标注写库失败");
            }
        }

        if let Some(bus) = self.bus.read().clone() {
            bus.publish(Event::new(
                "screenshot.taken",
                "screenshot",
                serde_json::json!({ "task_id": task_id, "file": file }),
            ))
            .ok();
        }
        // D-09 联动触发：历史已入库（回填 UPDATE 可命中）后再交接帧。
        // 帧走文件、事件只带路径引用——forward_events 会把全量 payload 转发到
        // 每个窗口，MB 级 base64 会淹掉 IPC；识别在 ocr-core 侧异步完成。
        // best-effort：截图产物已落盘，OCR 请求失败只告警不判 finish 失败。
        if outcome.request_ocr {
            if let Err(e) = self.dispatch_ocr_request(task_id, w, h, &rgba) {
                tracing::warn!(error = %e, "截图联动 OCR 请求失败，本次识别跳过");
            }
        }
        self.discard(task_id);
        Ok(FinishDto {
            file,
            pin_id,
            preview_b64: None,
        })
    }

    /// 历史条目的美化出口（D-29 B4 T-B4-6）：读原字节 → 解码 → 美化 → 走**同一条**
    /// 动作通路（`run_actions`），因此不存在第二套导出实现。
    ///
    /// **`actions` 为空 = 只预览不落盘**（字面写在这里，前端预览钮传 `[]`）：
    /// 返回 `preview_b64`（PNG，展示面恒无损，同 T-B4-7 对显示路径的裁定），
    /// 零磁盘写、零剪贴板写、零历史新增。
    /// 非空时按请求执行 save/copy/pin（OCR 动作同样复用，帧走既有交接），
    /// 但**不新增历史行**——美化产物是既有条目的派生物，历史表语义不变（本行"无 DB 变更"）。
    pub fn beautify_apply(
        &self,
        id: &str,
        spec: &crate::beautify::BeautifySpec,
        actions: &[String],
    ) -> Result<FinishDto, AppError> {
        let (_item, bytes) = self.history_raw(id)?;
        // 解码按内容嗅探（历史文件可能是 png/jpeg/webp 三形之一，见 T-B4-7 单向门）
        let (w, h, rgba) = util::decode_rgba_bytes(&bytes)?;
        let (nw, nh, nrgba) = crate::beautify::beautify(&rgba, w, h, spec)?;
        if actions.is_empty() {
            return Ok(FinishDto {
                file: None,
                pin_id: None,
                preview_b64: Some(util::encode_png_b64(nw, nh, &nrgba)?),
            });
        }
        // 格式跟随配置（fmt_override=None）：美化面板不重复覆盖层那颗格式钮的语义
        let outcome = self.run_actions(ActionRun {
            actions,
            w: nw,
            h: nh,
            rgba: &nrgba,
            fmt_override: None,
            pin_x: None,
            pin_y: None,
        })?;
        if outcome.request_ocr {
            if let Err(e) = self.dispatch_ocr_request(id, nw, nh, &nrgba) {
                tracing::warn!(error = %e, "美化联动 OCR 请求失败，本次识别跳过");
            }
        }
        Ok(FinishDto {
            file: outcome.file,
            pin_id: outcome.pin_id,
            preview_b64: None,
        })
    }

    /// 后处理动作的唯一执行点（finish 与 beautify_apply 共用）
    ///
    /// `fmt_override` = 覆盖层那颗格式钮的请求值；None 时回到配置 `format`。
    /// `save` 臂之外的动作与编码格式无关（copy/pin 恒 PNG，见各自函数 doc）。
    fn run_actions(&self, run: ActionRun<'_>) -> Result<ActionOutcome, AppError> {
        let ActionRun {
            actions,
            w,
            h,
            rgba,
            fmt_override,
            pin_x,
            pin_y,
        } = run;
        let cfg = self
            .config
            .try_lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let mut file: Option<String> = None;
        let mut pin_id: Option<String> = None;
        let mut request_ocr = false;
        for action in actions {
            match action.as_str() {
                "copy" => self.action_copy(w, h, rgba)?,
                "save" => {
                    // 解析放在 save 臂里而不是函数开头：一个写坏的 format 只该让保存失败，
                    // 不该波及"只复制/只贴图"这两条与编码格式无关的动作。
                    let raw = fmt_override.unwrap_or(&cfg.format);
                    let fmt = util::EncodeFormat::from_str_honest(raw)?;
                    file = Some(self.action_save(w, h, rgba, fmt, cfg.quality)?);
                }
                "pin" => {
                    let id =
                        self.action_pin(w, h, rgba, pin_x.unwrap_or(100), pin_y.unwrap_or(100))?;
                    pin_id = Some(id);
                }
                // docs/impl/03 P5：Ocr 动作只转发事件，识别在 ocr-core 侧异步完成，
                // 结果经 ocr.completed 回流（历史回填 + 截图 UI），此处登记待触发
                "ocr" => request_ocr = true,
                // 美化在这里是**有意的空臂**：像素在两个调用侧都已于循环之前一次性作用于
                // 最终图（`finish` 的 spec 决议 / `beautify_apply` 的入参），循环再改一次就是
                // "复制的是原图、保存的是美化图"那类分叉的成因。列进白名单而不留空臂，
                // 用户点的动作会被下面的 warn 吞掉——名字看得见、效果看不见，比空臂更坏。
                "beautify" => {}
                other => tracing::warn!(action = other, "未知截图后处理动作，已忽略"),
            }
        }
        Ok(ActionOutcome {
            file,
            pin_id,
            request_ocr,
        })
    }

    /// 写联动帧到 {appData}/frames/{task_id}.png（tmp+rename，规约 5）并发布
    /// `screenshot.ocr_requested {task_id, frame_ref}`（DESIGN O1：截图与 OCR
    /// 只经事件交互，不经函数调用；帧文件由消费端读取后删除）
    ///
    /// **恒 PNG**：OCR 输入侧要的是无损像素，JPEG 的块效应会直接打在字形边缘上，
    /// 而识别精度是这条链路的产物质量上限——导出格式（T-B4-7）因此不适用于此。
    fn dispatch_ocr_request(
        &self,
        task_id: &str,
        w: u32,
        h: u32,
        rgba: &[u8],
    ) -> Result<(), AppError> {
        let Some(dir) = self.app_data() else {
            return Err(mod_err("SCREENSHOT_STATE_001", "模块未初始化"));
        };
        let frames = frames_dir(&dir);
        std::fs::create_dir_all(&frames)
            .map_err(|e| mod_err("SCREENSHOT_OCR_001", format!("创建帧目录失败: {e}")))?;
        let path = frames.join(format!("{task_id}.png"));
        let tmp = frames.join(format!(".{task_id}.png.tmp"));
        let (bytes, _) = util::encode_rgba(util::EncodeFormat::Png, 80, w, h, rgba)?;
        let written = std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_file(&path);
            return Err(mod_err("SCREENSHOT_OCR_001", format!("帧落盘失败: {e}")));
        }
        let bus = self
            .bus
            .read()
            .clone()
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_001", "模块未就绪"))?;
        if let Err(e) = bus.publish(Event::new(
            "screenshot.ocr_requested",
            "screenshot",
            serde_json::json!({ "task_id": task_id, "frame_ref": path.to_string_lossy() }),
        )) {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        Ok(())
    }

    /// 复制到剪贴板。**恒 PNG，不随配置 `format` 走**（D-29 B4 T-B4-7 的"这条不做"）：
    /// `ClipContent::Image` 经 win-integration 的 `png_to_dib` 转成 CF_DIB，落到系统
    /// 剪贴板时已经是位图，编码格式在到达目的地之前就被消费掉了——给剪贴板加 JPEG
    /// 只会多一次有损解码，用户看到的像素数不会变。
    fn action_copy(&self, w: u32, h: u32, rgba: &[u8]) -> Result<(), AppError> {
        let (bytes, _) = util::encode_rgba(util::EncodeFormat::Png, 80, w, h, rgba)?;
        let clipboard = self
            .clipboard
            .read()
            .clone()
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_001", "ClipboardPort 未就绪"))?;
        clipboard.write(&host_core::ports::ClipContent::Image {
            format: "png".into(),
            width: w,
            height: h,
            bytes: std::sync::Arc::from(bytes.into_boxed_slice()),
        })
    }

    fn resolve_save_dir(&self, cfg: &ScreenshotConfig) -> PathBuf {
        if cfg.save_dir.is_empty() {
            self.app_data()
                .map(|d| d.join("screenshots"))
                .unwrap_or_else(std::env::temp_dir)
        } else {
            PathBuf::from(&cfg.save_dir)
        }
    }

    /// 保存到磁盘：本模块**唯一**跟随配置 `format` 的出口（D-29 B4 T-B4-7）。
    ///
    /// `fmt`/`quality` 由调用方（`finish` 的 save 臂）决议后传入，而不是在这里再读一次
    /// 配置——决议点只有一处，请求级覆盖与配置默认值的优先关系才不会在两处各写一遍。
    fn action_save(
        &self,
        w: u32,
        h: u32,
        rgba: &[u8],
        fmt: util::EncodeFormat,
        quality: u8,
    ) -> Result<String, AppError> {
        let cfg = self
            .config
            .try_lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let dir = self.resolve_save_dir(&cfg);
        std::fs::create_dir_all(&dir)
            .map_err(|e| mod_err("SCREENSHOT_SAVE_001", format!("创建目录失败: {e}")))?;

        let ext = fmt.ext();
        let ts = chrono::Local::now().format("%Y-%m-%d_%H%M%S").to_string();
        let resolved = util::resolve_filename(&cfg.filename_template, &ts, fmt);
        // 模板已经写了 `.{fmt}` 的不再补一次后缀（否则 shot_x.jpg.jpg），没写的补上——
        // 后缀由格式决定这件事只有一个真源，模板里的 {fmt} 只是让用户能控制它出现的位置
        let dot_ext = format!(".{ext}");
        let stem = resolved
            .strip_suffix(dot_ext.as_str())
            .unwrap_or(resolved.as_str());
        // 文件名冲突：追加 _1 递增，且扩展名跟随本次格式（docs/impl/03 P5）——
        // 写死 .png 会产出 `shot_x_1.png.jpg` 这类两后缀名，或在 jpeg 档下反复叠加
        let mut path = dir.join(format!("{stem}{dot_ext}"));
        let mut n = 1;
        while path.exists() {
            path = dir.join(format!("{stem}_{n}{dot_ext}"));
            n += 1;
        }

        // 临时文件 + rename（规约 5）
        let tmp = dir.join(format!(
            ".{}.tmp",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        let (bytes, _) = util::encode_rgba(fmt, quality, w, h, rgba)?;
        let written = std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_file(&path);
            return Err(mod_err("SCREENSHOT_SAVE_004", format!("落盘失败: {e}")));
        }
        Ok(path.to_string_lossy().to_string())
    }

    /// 贴图落盘。**恒 PNG**：贴图窗口要把图直接贴在屏幕上，文字边缘的 JPEG 块效应
    /// 在放大与半透明底色下最显眼，而贴图从不进"另存为"那条格式协商路径。
    fn action_pin(&self, w: u32, h: u32, rgba: &[u8], x: i32, y: i32) -> Result<String, AppError> {
        let Some(dir) = self.app_data() else {
            return Err(mod_err("SCREENSHOT_PIN_001", "模块未初始化"));
        };
        let pin_dir = dir.join("pins");
        std::fs::create_dir_all(&pin_dir)
            .map_err(|e| mod_err("SCREENSHOT_PIN_002", format!("创建目录失败: {e}")))?;
        let id = uuid::Uuid::now_v7().to_string();
        let file_rel = format!("pins/{id}.png");
        let path = dir.join(&file_rel);
        let (bytes, _) = util::encode_rgba(util::EncodeFormat::Png, 80, w, h, rgba)?;
        std::fs::write(&path, &bytes)
            .map_err(|e| mod_err("SCREENSHOT_PIN_004", format!("写入失败: {e}")))?;
        let record = PinRecord {
            id: id.clone(),
            x,
            y,
            width: w,
            height: h,
            zoom: 1.0,
            opacity: 1.0,
            file: file_rel,
        };
        {
            let mut pins = self.pins.lock();
            pins.push(record);
            let snapshot = pins.clone();
            drop(pins);
            self.persist_pins(&snapshot)
                .map_err(|e| mod_err("SCREENSHOT_PIN_005", e.to_string()))?;
        }
        Ok(id)
    }

    // ---------------- Pin 贴图（P6）----------------

    pub fn pins(&self) -> Vec<PinDto> {
        self.pins
            .lock()
            .iter()
            .map(|p| PinDto {
                id: p.id.clone(),
                x: p.x,
                y: p.y,
                width: p.width,
                height: p.height,
                zoom: p.zoom,
                opacity: p.opacity,
            })
            .collect()
    }

    pub fn pin_get(&self, id: &str) -> Result<PinDataDto, AppError> {
        let pins = self.pins.lock();
        let record = pins
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| mod_err("SCREENSHOT_PIN_006", "贴图不存在"))?;
        let Some(dir) = self.app_data() else {
            return Err(mod_err("SCREENSHOT_PIN_001", "模块未初始化"));
        };
        let bytes = std::fs::read(dir.join(&record.file))
            .map_err(|e| mod_err("SCREENSHOT_PIN_007", format!("读取贴图失败: {e}")))?;
        Ok(PinDataDto {
            id: record.id.clone(),
            png_b64: host_core::util::b64_encode(&bytes),
            x: record.x,
            y: record.y,
            width: record.width,
            height: record.height,
            zoom: record.zoom,
            opacity: record.opacity,
        })
    }

    pub fn pin_update(&self, id: &str, zoom: f32, opacity: f32) -> Result<(), AppError> {
        let mut pins = self.pins.lock();
        let record = pins
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| mod_err("SCREENSHOT_PIN_006", "贴图不存在"))?;
        record.zoom = zoom.clamp(0.2, 5.0);
        record.opacity = opacity.clamp(0.2, 1.0);
        let snapshot = pins.clone();
        drop(pins);
        self.persist_pins(&snapshot)
            .map_err(|e| mod_err("SCREENSHOT_PIN_005", e.to_string()))?;
        Ok(())
    }

    pub fn pin_close(&self, id: &str) -> Result<(), AppError> {
        let removed = {
            let mut pins = self.pins.lock();
            let old = pins.len();
            pins.retain(|p| p.id != id);
            let snapshot = pins.clone();
            let removed = old - pins.len();
            drop(pins);
            self.persist_pins(&snapshot)
                .map_err(|e| mod_err("SCREENSHOT_PIN_005", e.to_string()))?;
            removed
        };
        if let Some(dir) = self.app_data() {
            let _ = std::fs::remove_file(dir.join(format!("pins/{id}.png")));
        }
        if removed == 0 {
            return Err(mod_err("SCREENSHOT_PIN_006", "贴图不存在"));
        }
        Ok(())
    }

    /// OCR 文本回填（识别完成后由 ocr 命令层调用；仅记录，失败静默）
    pub fn record_ocr_text(&self, task_id: &str, text: &str) {
        if let Some(store) = self.store.read().clone() {
            store.set_ocr_text(task_id, text).ok();
        }
    }

    /// 历史库句柄（screenshot_history_list IPC 用）
    pub fn history_store(&self) -> Option<Arc<ShotStore>> {
        self.store.read().clone()
    }

    // ---------------- 历史字节出口（D-29 B0-2：面板缩略图 / 再复制）----------------

    /// 取历史条目的磁盘字节（名字里的 `png` 已随 T-B4-7 退役：自本代起扩展名可以是
    /// png/jpg/webp，因此**读侧一律按内容嗅探**，任何按后缀过滤的写法都是错的）
    fn history_raw(&self, id: &str) -> Result<(ShotItem, Vec<u8>), AppError> {
        let store = self
            .store
            .read()
            .clone()
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_001", "模块未就绪"))?;
        let item = store
            .get(id)?
            .ok_or_else(|| mod_err("SCREENSHOT_HISTORY_404", "历史记录不存在"))?;
        let file = item
            .file
            .clone()
            .ok_or_else(|| mod_err("SCREENSHOT_HISTORY_404", "该记录未保存文件"))?;
        let bytes = std::fs::read(&file)
            .map_err(|e| mod_err("SCREENSHOT_HISTORY_410", format!("文件已丢失或不可读: {e}")))?;
        Ok((item, bytes))
    }

    pub fn history_get(&self, id: &str) -> Result<ShotDataDto, AppError> {
        let (item, bytes) = self.history_raw(id)?;
        // 标注与字节同读取口：旧行列为 NULL → 空表（T-B4-1 零迁移承诺的消费侧）
        let store = self
            .store
            .read()
            .clone()
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_001", "模块未就绪"))?;
        let annotations = store.annotations_of(&item.id)?;
        Ok(ShotDataDto {
            id: item.id,
            png_b64: host_core::util::b64_encode(&bytes),
            // 前端拼 data URL 需要 MIME，而它只有 Base64 与（可能骗人的）后缀：
            // 嗅探放在宿主这一侧，读侧就只剩一个真源
            format: util::sniff_content_type(&bytes).to_owned(),
            annotations,
        })
    }

    /// 再复制：文件原字节交 ClipboardPort，CF_DIB 转换由 win-integration 负责（module.rs action_copy 同通道）
    pub fn history_copy(&self, id: &str) -> Result<(), AppError> {
        let (item, bytes) = self.history_raw(id)?;
        let clipboard = self
            .clipboard
            .read()
            .clone()
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_001", "ClipboardPort 未就绪"))?;
        clipboard.write(&host_core::ports::ClipContent::Image {
            // 标签按内容嗅探而不是写死 "png"：win-integration 对非 "dib" 一律走
            // load_from_memory（内容嗅探），所以贴错标签不会让它选错解码器，
            // 但会让任何读这个字段的下游（日志、未来的格式判断）拿到假话。
            format: util::sniff_content_type(&bytes).to_owned(),
            width: item.width,
            height: item.height,
            bytes: std::sync::Arc::from(bytes.into_boxed_slice()),
        })
    }
}

impl TrayProvider for ScreenshotModule {
    /// D-26：托盘段「截图与贴图」——与全局热键同一事件通路（overlay_requested）
    fn tray_menu_items(&self) -> Vec<TrayMenuItem> {
        vec![TrayMenuItem {
            id: "region".into(),
            label: "截图选区".into(),
            enabled: true,
        }]
    }

    fn tray_actions(&self) -> Vec<TrayAction> {
        let bus = self.bus.read().clone();
        let Some(bus) = bus else {
            return vec![];
        };
        vec![TrayAction {
            item_id: "region".into(),
            action: Arc::new(move || {
                bus.publish(Event::new(
                    "screenshot.overlay_requested",
                    "screenshot",
                    serde_json::json!({ "mode": "shot" }),
                ))
                .ok();
            }),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ocr_event(source: &'static str, payload: serde_json::Value) -> Event {
        Event::new("ocr.completed", source, payload)
    }

    #[test]
    fn parse_ocr_backfill_accepts_text_completion() {
        let ev = ocr_event(
            "ocr",
            json!({ "source_task_id": "t1", "text": "你好", "engine": "mock" }),
        );
        assert_eq!(parse_ocr_backfill(&ev), Some(("t1".into(), "你好".into())));
    }

    #[test]
    fn parse_ocr_backfill_rejects_copied_and_malformed() {
        // ocr_copy_text 也发布本主题（{action:"copied"}），不得回填
        let copied = ocr_event("ocr", json!({ "action": "copied" }));
        assert_eq!(parse_ocr_backfill(&copied), None);
        // 空 task_id / 非 ocr 来源 / 缺 text 均忽略
        let empty = ocr_event("ocr", json!({ "source_task_id": "", "text": "x" }));
        assert_eq!(parse_ocr_backfill(&empty), None);
        let foreign = ocr_event("screenshot", json!({ "source_task_id": "t1", "text": "x" }));
        assert_eq!(parse_ocr_backfill(&foreign), None);
        let no_text = ocr_event("ocr", json!({ "source_task_id": "t1" }));
        assert_eq!(parse_ocr_backfill(&no_text), None);
    }

    fn ann(kind: &str, layer: u32) -> Annotation {
        Annotation {
            kind: kind.into(),
            color: "#ff4d4f".into(),
            width: 2.0,
            points: vec![(0.0, 0.0), (1.0, 1.0)],
            text: None,
            seq: None,
            layer,
            locked: false,
            fill: false,
            alpha: 1.0,
        }
    }

    /// 只入历史、不跑后处理动作的 finish（auto_* 全关：测试不碰剪贴板与磁盘保存）
    fn finish_only(m: &ScreenshotModule, task_id: &str, annotations: Vec<Annotation>) {
        let cfg = ScreenshotConfig {
            auto_save: false,
            auto_copy: false,
            ..Default::default()
        };
        *m.config.try_lock().unwrap() = cfg;
        let png = util::encode_png_b64(2, 2, &[0u8; 16]).unwrap();
        let req = FinishRequest {
            image_b64: png,
            actions: vec![],
            pin_x: None,
            pin_y: None,
            annotations,
            format: None,
            beautify: None,
        };
        m.finish(task_id, &req).unwrap();
    }

    fn store_of(dir: &std::path::Path) -> Arc<ShotStore> {
        Arc::new(ShotStore::open(&dir.join("shots.db")).unwrap())
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-1）字面测试名优先于 rustc 命名惯例
    fn finish_annotationsPersistedAsJson_layersOrdered() {
        let dir = tempfile::tempdir().unwrap();
        let m = ScreenshotModule::new();
        let store = store_of(dir.path());
        *m.store.write() = Some(store.clone());
        // 入参 layer 乱序（5/1/3）：读回必须是升序，且次序由宿主排而不是信前端序
        finish_only(
            &m,
            "t1",
            vec![ann("pen", 5), ann("rect", 1), ann("ellipse", 3)],
        );
        let back = store.annotations_of("t1").unwrap();
        assert_eq!(
            back.iter()
                .map(|a| (a.layer, a.kind.clone()))
                .collect::<Vec<_>>(),
            vec![(1, "rect".into()), (3, "ellipse".into()), (5, "pen".into())]
        );
        // 列内容确实是 JSON 文本（而不是逐条一行之类的自定义编码），首元素就是最低层
        let raw = rusqlite::Connection::open(dir.path().join("shots.db"))
            .unwrap()
            .query_row("SELECT annotations FROM shots WHERE id = 't1'", [], |r| {
                r.get::<_, Option<String>>(0)
            })
            .unwrap()
            .expect("t1 的标注列应已写入");
        assert!(raw.starts_with(r#"[{"kind":"rect""#), "实际落盘：{raw}");
        // 同层条目保持入参相对序（稳定排序，不是按内容重排）
        let m2 = ScreenshotModule::new();
        let store2 = store_of(dir.path());
        *m2.store.write() = Some(store2.clone());
        let mut tie = vec![ann("rect", 0), ann("ellipse", 0), ann("pen", 0)];
        tie[0].color = "#000000".into();
        finish_only(&m2, "t2", tie);
        assert_eq!(
            store2
                .annotations_of("t2")
                .unwrap()
                .iter()
                .map(|a| a.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["rect", "ellipse", "pen"]
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn finish_noAnnotations_storesNullNotEmptyJson() {
        let dir = tempfile::tempdir().unwrap();
        let m = ScreenshotModule::new();
        let store = store_of(dir.path());
        *m.store.write() = Some(store.clone());
        finish_only(&m, "empty", vec![]);
        let raw = rusqlite::Connection::open(dir.path().join("shots.db"))
            .unwrap()
            .query_row(
                "SELECT annotations FROM shots WHERE id = 'empty'",
                [],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap();
        assert_eq!(raw, None, "空数组必须落 NULL，不能落 \"[]\"");
        assert!(store.annotations_of("empty").unwrap().is_empty());
        // 正对照：同库一条带标注的落 Some(JSON)，两态可分辨
        finish_only(&m, "with", vec![ann("rect", 0)]);
        let raw2 = rusqlite::Connection::open(dir.path().join("shots.db"))
            .unwrap()
            .query_row("SELECT annotations FROM shots WHERE id = 'with'", [], |r| {
                r.get::<_, Option<String>>(0)
            })
            .unwrap();
        assert!(raw2.is_some(), "带标注的必须非 NULL");
        assert_eq!(store.annotations_of("with").unwrap().len(), 1);
    }

    #[test]
    #[allow(non_snake_case)]
    fn historyGet_annotationsAbsent_returnsEmptyNotError() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("shot.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n rest-bytes").unwrap();
        let m = ScreenshotModule::new();
        let store = store_of(dir.path());
        *m.store.write() = Some(store.clone());
        let shot = |id: &str| ShotItem {
            id: id.into(),
            created_ms: 1,
            width: 2,
            height: 2,
            file: Some(png.to_string_lossy().into_owned()),
            ocr_text: None,
        };
        store.insert(&shot("absent")).unwrap();
        store.insert(&shot("present")).unwrap();
        store
            .set_annotations(
                "present",
                Some(&serde_json::to_string(&[ann("rect", 0)]).unwrap()),
            )
            .unwrap();
        let a = m.history_get("absent").unwrap();
        assert!(a.annotations.is_empty(), "旧行 NULL 读回空表而非报错");
        assert!(!a.png_b64.is_empty(), "字节出口不受新列影响");
        // 正对照：同库带标注那条读回非空（否则上面的"空"可以是永远空的空洞）
        let p = m.history_get("present").unwrap();
        assert_eq!(p.annotations.len(), 1);
        assert_eq!(p.annotations[0].kind, "rect");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-7）字面测试名优先于 rustc 命名惯例
    fn actionSave_usesConfigFormat_endToEnd() {
        let dir = tempfile::tempdir().unwrap();
        let save_dir = dir.path().join("out");
        let m = ScreenshotModule::new();
        let cfg = ScreenshotConfig {
            save_dir: save_dir.to_string_lossy().into_owned(),
            filename_template: "shot_{ts}.{fmt}".into(),
            format: "jpeg".into(),
            quality: 40,
            ..Default::default()
        };
        *m.config.try_lock().unwrap() = cfg;
        let rgba = vec![10u8, 20, 30, 255, 1, 2, 3, 128, 5, 6, 7, 0, 9, 10, 11, 255];
        let path = m
            .action_save(2, 2, &rgba, util::EncodeFormat::Jpeg, 40)
            .unwrap();

        // 目录里必须恰有这一个文件（tmp 不留残，扩展名不写第二遍）
        let files = std::fs::read_dir(&save_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            files.len(),
            1,
            "保存目录里只该有落盘的那一个文件：{files:?}"
        );
        let name = &files[0];
        assert!(name.ends_with(".jpg"), "扩展名要跟格式走，实得 {name}");
        assert!(
            !name.contains(".jpg.jpg"),
            "模板里的 {{fmt}} 与补上的后缀重复了：{name}"
        );
        assert!(name.starts_with("shot_20"), "时间戳占位符没被替换：{name}");
        assert_eq!(
            PathBuf::from(&path),
            save_dir.join(name),
            "返回值就是落盘路径"
        );

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 12);
        assert!(bytes.starts_with(b"\xFF\xD8\xFF"), "JPEG 文件头对味");
        // content_type 与配置一致（读侧只信内容，不信后缀）
        assert_eq!(util::sniff_content_type(&bytes), "image/jpeg");
        assert_eq!(
            util::sniff_content_type(&bytes),
            util::EncodeFormat::Jpeg.content_type()
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn actionSave_nameCollision_appendsSuffixSameExt() {
        let dir = tempfile::tempdir().unwrap();
        let save_dir = dir.path().join("out2");
        let m = ScreenshotModule::new();
        // 固定模板（不含 {ts}）：两次保存必然撞名，这才测得到冲突分支
        *m.config.try_lock().unwrap() = ScreenshotConfig {
            save_dir: save_dir.to_string_lossy().into_owned(),
            filename_template: "dup".into(),
            format: "jpeg".into(),
            ..Default::default()
        };
        let rgba = vec![1u8, 2, 3, 255];
        let first = m
            .action_save(1, 1, &rgba, util::EncodeFormat::Jpeg, 80)
            .unwrap();
        let second = m
            .action_save(1, 1, &rgba, util::EncodeFormat::Jpeg, 80)
            .unwrap();
        assert!(first.ends_with("dup.jpg"), "{first}");
        // 红线：`_1` 加在主名后、扩展名只有一个（旧写法会产出 dup.png.jpg 这类两后缀）
        assert!(second.ends_with("dup_1.jpg"), "{second}");
        assert!(!second.contains(".png"), "冲突分支偷偷换了格式：{second}");
        assert_ne!(first, second);
        let files = std::fs::read_dir(&save_dir).unwrap().count();
        assert_eq!(files, 2, "两次保存留两个文件");
        // 正对照：同目录换 png 时主名相同也不撞车（后缀由格式决定，不是写死的）
        let third = m
            .action_save(1, 1, &rgba, util::EncodeFormat::Png, 80)
            .unwrap();
        assert!(third.ends_with("dup.png"), "{third}");
        assert_eq!(std::fs::read_dir(&save_dir).unwrap().count(), 3);
    }

    /// 单向门（`file` 后缀不再恒 `.png`）的读侧红线：格式声明来自内容嗅探，后缀骗人不管用
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-7）字面测试名优先于 rustc 命名惯例
    fn historyGet_formatSniffed_notFromSuffix() {
        let dir = tempfile::tempdir().unwrap();
        // 故意把 JPEG 字节存成 .png 后缀：任何"按后缀拼 data URL"的写法都会在这里露馅
        let lie = dir.path().join("lie.png");
        let (jpeg, _) =
            util::encode_rgba(util::EncodeFormat::Jpeg, 70, 1, 1, &[1, 2, 3, 255]).unwrap();
        std::fs::write(&lie, &jpeg).unwrap();
        let m = ScreenshotModule::new();
        let store = store_of(dir.path());
        *m.store.write() = Some(store.clone());
        store
            .insert(&ShotItem {
                id: "liar".into(),
                created_ms: 1,
                width: 1,
                height: 1,
                file: Some(lie.to_string_lossy().into_owned()),
                ocr_text: None,
            })
            .unwrap();
        let got = m.history_get("liar").unwrap();
        assert_eq!(got.format, "image/jpeg", "必须嗅探出真格式");
        assert!(
            !got.format.contains("png"),
            "后缀里的 png 不得泄漏成格式声明"
        );
        // 正对照：真 png 存成 .png 时同一函数报 image/png（否则可以是"永远报 jpeg"）
        std::fs::write(&lie, b"\x89PNG\r\n\x1a\n body").unwrap();
        assert_eq!(m.history_get("liar").unwrap().format, "image/png");
    }

    /// 捕获写入的位图字节（T-B4-6 三面同源判据的观察窗：剪贴板是唯一能被测试
    /// 直接看见的落点，磁盘与帧文件另有各自断言）
    #[derive(Default)]
    struct CaptureClipboard {
        images: std::sync::Mutex<Vec<(u32, u32, Vec<u8>)>>,
    }
    impl ClipboardPort for CaptureClipboard {
        fn start_listener(
            &self,
            _cb: Box<dyn Fn(host_core::ports::ClipContent, Option<String>) + Send + Sync>,
        ) -> Result<(), AppError> {
            Ok(())
        }
        fn write(&self, content: &host_core::ports::ClipContent) -> Result<(), AppError> {
            if let host_core::ports::ClipContent::Image {
                width,
                height,
                bytes,
                ..
            } = content
            {
                self.images
                    .lock()
                    .unwrap()
                    .push((*width, *height, bytes.to_vec()));
            }
            Ok(())
        }
    }

    /// 美化后的四面同源断言（T-B4-6 承重判据）：`finish` 里美化只发生一次，
    /// 因此剪贴板字节 / 落盘字节 / 联动帧字节三者逐字节相等，且等于"单跑一次
    /// beautify + 单跑一次 png 编码"的结果。任何"某一面偷偷用原图"的写法都会在这里红。
    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-6）字面测试名优先于 rustc 命名惯例
    async fn finish_beautifyAppliedOnceForAllThreeActions() {
        let dir = tempfile::tempdir().unwrap();
        let m = ScreenshotModule::new();
        let clip = Arc::new(CaptureClipboard::default());
        *m.clipboard.write() = Some(clip.clone());
        let store = store_of(dir.path());
        *m.store.write() = Some(store);
        let bus = Arc::new(EventBus::new());
        let mut rx = bus.subscribe("screenshot.ocr_requested").unwrap();
        *m.bus.write() = Some(bus);
        *m.app_data_dir.write() = Some(dir.path().to_path_buf());
        *m.config.try_lock().unwrap() = ScreenshotConfig {
            save_dir: dir.path().join("out").to_string_lossy().into_owned(),
            auto_copy: false,
            auto_save: false,
            auto_pin: false,
            ..Default::default()
        };

        // 源图：2×2 每像素不同值——搬运错位一眼可见
        let src: Vec<u8> = vec![
            10, 11, 12, 255, 20, 21, 22, 255, 30, 31, 32, 255, 40, 41, 42, 255,
        ];
        let spec = crate::beautify::BeautifySpec {
            radius: 0,
            padding: 2,
            shadow: false,
            bg_from: "#000000".into(),
            bg_to: "#000000".into(),
        };
        let (ew, eh, ebytes) = crate::beautify::beautify(&src, 2, 2, &spec).unwrap();
        assert_eq!((ew, eh), (6, 6), "2×2 + padding 2 → 6×6");
        let expected_png = util::encode_rgba(util::EncodeFormat::Png, 80, ew, eh, &ebytes)
            .unwrap()
            .0;

        let req = FinishRequest {
            image_b64: util::encode_png_b64(2, 2, &src).unwrap(),
            actions: vec!["copy".into(), "save".into(), "pin".into(), "ocr".into()],
            pin_x: Some(5),
            pin_y: Some(6),
            annotations: vec![],
            format: None,
            beautify: Some(spec.clone()),
        };
        let out = m.finish("t1", &req).unwrap();
        let file = out.file.clone().expect("save 动作应返回路径");
        let pin = out.pin_id.clone().expect("pin 动作应返回 id");
        assert!(out.preview_b64.is_none(), "正常完成动作不填预览");

        // ① 剪贴板面（块内读完即释放锁：下面有 await，同步锁跨 await 是 clippy 硬拦）
        {
            let images = clip.images.lock().unwrap();
            assert_eq!(images.len(), 1);
            assert_eq!(images[0].0, 6);
            assert_eq!(images[0].1, 6);
            assert_eq!(
                images[0].2, expected_png,
                "剪贴板字节须等于单跑 beautify 的编码"
            );
        }

        // ② 磁盘面
        let saved = std::fs::read(&file).unwrap();
        assert_eq!(saved, expected_png, "落盘字节须与剪贴板同源");

        // ③ 贴图面
        let pinned = std::fs::read(dir.path().join(format!("pins/{pin}.png"))).unwrap();
        assert_eq!(pinned, expected_png, "贴图字节须与另两面同源");

        // ④ OCR 交接帧面（行字面"ocr 臂另断帧文件同像素"）
        let frame = dir.path().join("frames").join("t1.png");
        let ev = rx.recv().await.unwrap();
        assert_eq!(
            ev.payload["frame_ref"],
            frame.to_string_lossy().as_ref(),
            "事件带的就是那枚帧路径"
        );
        assert_eq!(std::fs::read(&frame).unwrap(), expected_png);

        // 历史行记的是美化后的实际产物尺寸（不是原图尺寸），面板才不会显错缩略图比例
        let item = m.history_store().unwrap().get("t1").unwrap().unwrap();
        assert_eq!((item.width, item.height), (6, 6));
        // 正对照：不带 beautify 时原样（否则上面四条可以是"永远加 4 像素"的假绿）
        let plain = FinishRequest {
            beautify: None,
            actions: vec!["copy".into()],
            ..req
        };
        m.finish("t2", &plain).unwrap();
        let imgs = clip.images.lock().unwrap();
        assert_eq!((imgs[1].0, imgs[1].1), (2, 2), "无 beautify 时尺寸不变");
    }

    /// 写侧拒（D-29 B4 T-B4-8 红线）：拼错的动作名进不了配置。
    /// 运行期那臂（`effective_actions` warn 后丢）测的是手改 JSON 的残留，两者各测一次：
    /// 只留运行期 warn，用户永远看不到自己写坏了什么；只留写侧拒，坏值一旦来自
    /// 旧版本/手工编辑就会在截图时才炸——那时没人会想起去翻设置。
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-8）字面测试名优先于 rustc 命名惯例
    fn applyConfig_unknownPostAction_rejectsNamingValue() {
        let m = ScreenshotModule::new();
        let e = match m.apply_config(json!({ "post_actions": ["copy", "teleport"] })) {
            Err(e @ ModuleError::Config(_)) => e,
            other => panic!("越界动作必须走 Config 拒绝，实得 {other:?}"),
        };
        // 点名违规值本身（只说"配置错误"等于让用户自己一条条试）
        let msg = e.to_string();
        assert!(msg.contains("teleport"), "{msg}");
        // 白名单同样进消息：用户看得见有哪些合法值可填
        assert!(msg.contains("beautify"), "{msg}");
        // 拒后状态不变：坏 payload 一个键都不落地（不是"先收下再报错"）
        let cfg = m.config.try_lock().unwrap().clone();
        assert!(cfg.post_actions.is_empty());
        assert!(cfg.auto_save, "三 bool 保持默认，未被半写覆盖");
        // 正对照：同一形状的合法值真能写进去（否则上面三条对任何输入都绿）
        m.apply_config(json!({ "post_actions": ["copy", "ocr"], "auto_save": false }))
            .unwrap();
        let cfg = m.config.try_lock().unwrap().clone();
        assert_eq!(cfg.post_actions, ["copy", "ocr"]);
        assert!(!cfg.auto_save);
        // 非字符串元素也点名（serde 的类型错只会说 invalid type，指不出是哪一格）
        let e = m
            .apply_config(json!({ "post_actions": [42] }))
            .expect_err("数字动作必须拒");
        assert!(e.to_string().contains("42"), "{e}");
    }

    /// `beautify` 进动作链但不改像素（D-29 B4 T-B4-8）：请求没带 spec 时走恒等预设。
    /// 这一条钉的是"名单里有这个名字就足以让链路走美化通路，而通路本身是零副作用的"。
    #[test]
    #[allow(non_snake_case)]
    fn finish_beautifyDefaultSpec_isNoOp() {
        let dir = tempfile::tempdir().unwrap();
        let m = ScreenshotModule::new();
        let clip = Arc::new(CaptureClipboard::default());
        *m.clipboard.write() = Some(clip.clone());
        let store = store_of(dir.path());
        *m.store.write() = Some(store);
        *m.app_data_dir.write() = Some(dir.path().to_path_buf());
        *m.config.try_lock().unwrap() = ScreenshotConfig {
            save_dir: dir.path().join("out").to_string_lossy().into_owned(),
            ..Default::default()
        };
        let src: Vec<u8> = vec![
            10, 11, 12, 255, 20, 21, 22, 255, 30, 31, 32, 255, 40, 41, 42, 255,
        ];
        let png = util::encode_png_b64(2, 2, &src).unwrap();

        // ① 只 save：基线字节
        let base = m
            .finish(
                "t1",
                &FinishRequest {
                    image_b64: png.clone(),
                    actions: vec!["save".into()],
                    pin_x: None,
                    pin_y: None,
                    annotations: vec![],
                    format: None,
                    beautify: None,
                },
            )
            .unwrap()
            .file
            .unwrap();
        // ② save + beautify（无 spec）：字节与历史尺寸须与基线逐字节相同
        let with_action = m
            .finish(
                "t2",
                &FinishRequest {
                    image_b64: png,
                    actions: vec!["save".into(), "beautify".into()],
                    pin_x: None,
                    pin_y: None,
                    annotations: vec![],
                    format: None,
                    beautify: None,
                },
            )
            .unwrap();
        let f2 = with_action
            .file
            .clone()
            .expect("beautify 不得吞掉同批 save");
        assert_eq!(
            std::fs::read(&f2).unwrap(),
            std::fs::read(&base).unwrap(),
            "恒等预设必须逐字节不动图"
        );
        let item = m.history_store().unwrap().get("t2").unwrap().unwrap();
        assert_eq!((item.width, item.height), (2, 2), "历史行记的仍是原尺寸");
        // ③ 动作链不被空臂打断：beautify 之后的 copy 照常执行（原样进剪贴板）
        m.finish(
            "t3",
            &FinishRequest {
                image_b64: util::encode_png_b64(2, 2, &src).unwrap(),
                actions: vec!["beautify".into(), "copy".into()],
                pin_x: None,
                pin_y: None,
                annotations: vec![],
                format: None,
                beautify: None,
            },
        )
        .unwrap();
        {
            let images = clip.images.lock().unwrap();
            assert_eq!(images.len(), 1, "beautify 空臂不能让后续动作漏掉");
            assert_eq!((images[0].0, images[0].1), (2, 2));
        }
        // ④ 正对照（防空洞）：同一枚动作名配上真 spec 时像素确实变了——
        // 说明 ② 的"不变"来自恒等预设而不是 beautify 被整个忽略
        let spec = crate::beautify::BeautifySpec {
            padding: 2,
            ..Default::default()
        };
        let big = m
            .finish(
                "t4",
                &FinishRequest {
                    image_b64: util::encode_png_b64(2, 2, &src).unwrap(),
                    actions: vec!["save".into(), "beautify".into()],
                    pin_x: None,
                    pin_y: None,
                    annotations: vec![],
                    format: None,
                    beautify: Some(spec),
                },
            )
            .unwrap()
            .file
            .unwrap();
        let (w, h, _) = util::decode_rgba_bytes(&std::fs::read(&big).unwrap()).unwrap();
        assert_eq!((w, h), (6, 6), "带 spec 时 padding=2 应外扩成 6×6");
    }

    /// 预览臂（行字面 `actions: []` = 只预览不落盘）：零磁盘写、零剪贴板写、零历史新增
    #[test]
    #[allow(non_snake_case)]
    fn beautifyApply_emptyActions_previewsOnly() {
        let dir = tempfile::tempdir().unwrap();
        let shot = dir.path().join("shot.png");
        let src: Vec<u8> = vec![7, 7, 7, 255, 9, 9, 9, 255];
        std::fs::write(
            &shot,
            util::encode_rgba(util::EncodeFormat::Png, 80, 2, 1, &src)
                .unwrap()
                .0,
        )
        .unwrap();
        let m = ScreenshotModule::new();
        let clip = Arc::new(CaptureClipboard::default());
        *m.clipboard.write() = Some(clip.clone());
        let store = store_of(dir.path());
        store
            .insert(&ShotItem {
                id: "s1".into(),
                created_ms: 1,
                width: 2,
                height: 1,
                file: Some(shot.to_string_lossy().into_owned()),
                ocr_text: None,
            })
            .unwrap();
        *m.store.write() = Some(store);
        let save_dir = dir.path().join("out");
        *m.config.try_lock().unwrap() = ScreenshotConfig {
            save_dir: save_dir.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let spec = crate::beautify::BeautifySpec {
            padding: 3,
            ..Default::default()
        };
        let out = m.beautify_apply("s1", &spec, &[]).unwrap();
        let b64 = out.preview_b64.expect("空 actions 必须回预览字节");
        assert!(out.file.is_none() && out.pin_id.is_none());
        let (w, h, _) = util::decode_png_b64(&b64).unwrap();
        assert_eq!((w, h), (8, 7), "预览须是美化后的 2×1+padding3 而非原图");
        assert_eq!(clip.images.lock().unwrap().len(), 0, "预览不碰剪贴板");
        assert!(!save_dir.exists(), "预览不落盘（连保存目录都不该建出来）");
        assert_eq!(
            m.history_store()
                .unwrap()
                .list(&crate::types::HistoryQuery { page: 1, size: 10 })
                .unwrap()
                .total,
            1,
            "预览不新增历史"
        );
    }

    /// 出盘面复用同一条 `run_actions`：美化另存真的落盘，且**不新增历史行**
    #[test]
    #[allow(non_snake_case)]
    fn beautifyApply_saveAction_writesFileAndKeepsHistoryRow() {
        let dir = tempfile::tempdir().unwrap();
        let shot = dir.path().join("shot.png");
        let src: Vec<u8> = vec![7u8; 8];
        std::fs::write(
            &shot,
            util::encode_rgba(util::EncodeFormat::Png, 80, 2, 1, &src)
                .unwrap()
                .0,
        )
        .unwrap();
        let m = ScreenshotModule::new();
        let store = store_of(dir.path());
        store
            .insert(&ShotItem {
                id: "s1".into(),
                created_ms: 1,
                width: 2,
                height: 1,
                file: Some(shot.to_string_lossy().into_owned()),
                ocr_text: None,
            })
            .unwrap();
        let n_before = store
            .list(&crate::types::HistoryQuery { page: 1, size: 10 })
            .unwrap()
            .total;
        *m.store.write() = Some(store);
        let save_dir = dir.path().join("out");
        *m.config.try_lock().unwrap() = ScreenshotConfig {
            save_dir: save_dir.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let spec = crate::beautify::BeautifySpec {
            padding: 1,
            shadow: true,
            ..Default::default()
        };
        let actions = vec!["save".to_string()];
        let out = m.beautify_apply("s1", &spec, &actions).unwrap();
        let file = out.file.expect("save 动作应返回路径");
        assert!(out.preview_b64.is_none(), "出盘臂不返预览（两臂语义互斥）");
        let (w, h, _) = util::decode_rgba_bytes(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!((w, h), (4, 27), "card 4×3 + 阴影带 24 行在卡片之外");
        assert_eq!(
            m.history_store()
                .unwrap()
                .list(&crate::types::HistoryQuery { page: 1, size: 10 })
                .unwrap()
                .total,
            n_before,
            "美化产物是派生物，历史行数不变"
        );
        // 红线：坏 spec 出盘前先拒，不留半个文件
        let bad = crate::beautify::BeautifySpec {
            bg_from: "not-a-color".into(),
            ..spec
        };
        assert_eq!(
            m.beautify_apply("s1", &bad, &actions).unwrap_err().code(),
            "SCREENSHOT_BEAUTIFY_002"
        );
        assert_eq!(std::fs::read_dir(&save_dir).unwrap().count(), 1);
    }

    /// D-29 B0-2：历史字节出口错误路径三类可分辨（未就绪 / 记录不存在 / 文件丢失）
    #[test]
    fn history_export_error_paths_are_distinguishable() {
        let m = ScreenshotModule::new();
        assert_eq!(
            m.history_get("x").unwrap_err().code(),
            "SCREENSHOT_STATE_001"
        );
        assert_eq!(
            m.history_copy("x").unwrap_err().code(),
            "SCREENSHOT_STATE_001"
        );
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ShotStore::open(&dir.path().join("h.db")).unwrap());
        *m.store.write() = Some(store.clone());
        assert_eq!(
            m.history_get("nope").unwrap_err().code(),
            "SCREENSHOT_HISTORY_404"
        );
        store
            .insert(&ShotItem {
                id: "s1".into(),
                created_ms: 1,
                width: 2,
                height: 2,
                file: Some(
                    dir.path()
                        .join("missing.png")
                        .to_string_lossy()
                        .into_owned(),
                ),
                ocr_text: None,
            })
            .unwrap();
        assert_eq!(
            m.history_get("s1").unwrap_err().code(),
            "SCREENSHOT_HISTORY_410"
        );
    }

    /// D-09 验收：ocr 动作写联动帧文件并发布只含路径引用的 ocr_requested 事件
    #[tokio::test]
    async fn dispatch_ocr_request_writes_frame_and_publishes_ref() {
        let dir = tempfile::tempdir().unwrap();
        let m = ScreenshotModule::new();
        let bus = Arc::new(EventBus::new());
        let mut rx = bus.subscribe("screenshot.ocr_requested").unwrap();
        *m.app_data_dir.write() = Some(dir.path().to_path_buf());
        *m.bus.write() = Some(bus);

        let rgba = vec![10u8, 20, 30, 255, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        m.dispatch_ocr_request("task-9", 2, 2, &rgba).unwrap();

        let frame_path = dir.path().join("frames").join("task-9.png");
        assert!(frame_path.is_file());
        // tmp 文件不得残留（规约 5 原子写）
        assert!(!dir.path().join("frames").join(".task-9.png.tmp").exists());

        let ev = rx.recv().await.unwrap();
        assert_eq!(ev.payload["task_id"], "task-9");
        assert_eq!(
            ev.payload["frame_ref"],
            frame_path.to_string_lossy().as_ref()
        );
        // 事件只带路径，不带像素载荷
        assert!(ev.payload.get("png_b64").is_none());
    }

    #[test]
    fn dispatch_ocr_request_requires_init() {
        let m = ScreenshotModule::new();
        let e = m.dispatch_ocr_request("t", 1, 1, &[0; 4]).unwrap_err();
        assert_eq!(e.code(), "SCREENSHOT_STATE_001");
    }

    /// D-26 验收⑤：托盘 region 动作经 bus 发布与热键同通路事件（mode=shot）；init 前为空
    #[tokio::test]
    async fn tray_action_publishes_overlay_request_observable_on_bus() {
        let m = ScreenshotModule::new();
        assert!(m.tray_actions().is_empty(), "init 前不得提供动作");

        let bus = Arc::new(EventBus::new());
        let mut rx = bus
            .subscribe("screenshot.overlay_requested")
            .expect("订阅 tray 事件");
        *m.bus.write() = Some(bus);
        let actions = m.tray_actions();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].item_id, "region");
        (actions[0].action)();
        let ev = rx.recv().await.expect("动作闭包应发布事件");
        assert_eq!(ev.source, "screenshot");
        assert_eq!(ev.payload["mode"], "shot");
        assert_eq!(m.tray_menu_items()[0].label, "截图选区");
    }

    // ---------------- T-B4-4：窗口轨（枚举 + 按句柄抓帧）----------------

    fn fake_frame() -> host_core::ports::Frame {
        host_core::ports::Frame {
            width: 4,
            height: 4,
            // 全非零：`is_black_frame` 采样 16 点，任一非 0 即放行（假端口不该自己把任务判死）
            bgra: Arc::from(vec![7u8; 4 * 4 * 4].into_boxed_slice()),
            dpi_scale: 1.0,
            monitor_id: 0,
        }
    }

    /// 记录型窗口端口：收到的 `CaptureTarget` 全部存下，另备一张固定窗口表
    #[derive(Default)]
    struct FakeCapture {
        targets: std::sync::Mutex<Vec<CaptureTarget>>,
        windows: std::sync::Mutex<Vec<host_core::ports::WindowTarget>>,
    }

    /// 只会两个旧方法的端口：`list_windows` 走 trait 默认实现臂
    struct LegacyCapture;

    fn win(hwnd: i64, title: &str, minimized: bool) -> host_core::ports::WindowTarget {
        host_core::ports::WindowTarget {
            hwnd,
            title: title.into(),
            x: 100,
            y: 20,
            width: 800,
            height: 600,
            minimized,
        }
    }

    impl CapturePort for FakeCapture {
        fn enumerate_monitors(&self) -> Result<Vec<host_core::ports::MonitorInfo>, AppError> {
            Ok(vec![host_core::ports::MonitorInfo {
                id: 0,
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
                dpi_scale: 1.0,
            }])
        }
        fn capture(&self, target: CaptureTarget) -> Result<host_core::ports::Frame, AppError> {
            self.targets.lock().unwrap().push(target);
            Ok(fake_frame())
        }
        fn list_windows(&self) -> Vec<host_core::ports::WindowTarget> {
            self.windows.lock().unwrap().clone()
        }
    }

    impl CapturePort for LegacyCapture {
        fn enumerate_monitors(&self) -> Result<Vec<host_core::ports::MonitorInfo>, AppError> {
            Ok(vec![])
        }
        fn capture(&self, _target: CaptureTarget) -> Result<host_core::ports::Frame, AppError> {
            Ok(fake_frame())
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-4）字面测试名优先于 rustc 命名惯例
    fn startCapture_hwndPassed_routesToWindowTarget_notFullScreen() {
        let m = ScreenshotModule::new();
        let port = Arc::new(FakeCapture {
            windows: std::sync::Mutex::new(vec![
                win(4242, "此电脑", false),
                win(5, "收件箱", true),
            ]),
            ..Default::default()
        });
        *m.capture.write() = Some(port.clone());

        let dto = m.start_capture("shot", Some(4242)).unwrap();
        let got = port.targets.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert!(
            matches!(got[0], CaptureTarget::Window { hwnd: 4242 }),
            "带句柄必须打到窗口轨，实收 {:?}",
            got[0]
        );
        // 定位矩形 = 该窗矩形：覆盖层贴在目标窗之上，选区坐标因此仍是帧内坐标
        assert_eq!((dto.x, dto.y, dto.width, dto.height), (100, 20, 800, 600));
        // 窗口态随帧存、经 task_info 带下去（两条装载路径共用一个判据）
        assert_eq!(
            m.task_info(&dto.task_id).unwrap().hwnd,
            Some(4242),
            "覆盖层据此跳过拖框"
        );

        // 正对照（缺省臂）：不传句柄 → 全屏 bounds，逐字沿用旧行为
        let full = m.start_capture("shot", None).unwrap();
        let got = port.targets.lock().unwrap().clone();
        assert_eq!(got.len(), 2);
        assert!(
            matches!(got[1], CaptureTarget::FullScreen { monitor: 0 }),
            "缺省臂仍是全屏，实收 {:?}",
            got[1]
        );
        assert_eq!(m.task_info(&full.task_id).unwrap().hwnd, None);
        assert_eq!((full.width, full.height), (1920, 1080));

        // 失效句柄：明说原因，而不是悄悄改抓全屏（用户刚点的就是那一行）
        let e = m.start_capture("shot", Some(9999)).unwrap_err();
        assert_eq!(e.code(), "SCREENSHOT_WINDOW_001");
        assert!(e.to_string().contains("9999"), "错误须点名被拒句柄：{e}");
        // 最小化窗：表里看得见，点上必拒（PrintWindow 对最小化窗只给得出空图）
        let e = m.start_capture("shot", Some(5)).unwrap_err();
        assert_eq!(e.code(), "SCREENSHOT_WINDOW_002");
        assert!(e.to_string().contains("收件箱"), "文案须点名是哪个窗：{e}");
        assert_eq!(
            port.targets.lock().unwrap().len(),
            2,
            "两次被拒的调用一次帧都不该抓"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-4）字面测试名优先于 rustc 命名惯例
    fn windowTargets_portWithoutList_returnsEmptyNotError() {
        // trait 默认实现臂：端口没有枚举能力 → 空表（不 panic、不谎报一套坐标、不是错误）
        let m = ScreenshotModule::new();
        *m.capture.write() = Some(Arc::new(LegacyCapture));
        assert!(m.window_targets().is_empty());

        // 正对照：装上会枚举的端口就真有表（否则上面那行可以是"永远返回空"的假绿）
        *m.capture.write() = Some(Arc::new(FakeCapture {
            windows: std::sync::Mutex::new(vec![win(4242, "此电脑", false)]),
            ..Default::default()
        }));
        let listed = m.window_targets();
        assert_eq!(listed.len(), 1);
        assert_eq!((listed[0].hwnd, listed[0].title.as_str()), (4242, "此电脑"));
        assert_eq!((listed[0].width, listed[0].height), (800, 600));
        assert!(!listed[0].minimized);

        // 端口未注册（init 之前）同样是空表：命令层不该因一次枚举失败弹红
        assert!(ScreenshotModule::new().window_targets().is_empty());
    }
}
