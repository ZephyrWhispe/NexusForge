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
    ConfirmRect, CropDto, FinishDto, FinishRequest, PinDataDto, PinDto, ScreenshotConfig,
    ShotDataDto, ShotItem, TaskInfoDto, TaskStartDto,
};
use crate::util;

use host_core::util::app_err as mod_err;

/// OCR 联动帧的临时目录（{appData}/frames）：finish(ocr) 写入 → ocr-core 消费后删除；
/// init 时清空上次进程遗留（帧是一次性交接物，崩溃残留无保留价值）
fn frames_dir(app_data: &std::path::Path) -> std::path::PathBuf {
    app_data.join("frames")
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
                    "description": "{ts} 替换为时间戳",
                    "default": "shot_{ts}"
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
                }
            }
        })
    }

    fn apply_config(&self, values: serde_json::Value) -> Result<(), ModuleError> {
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

    /// 全屏抓帧（阻塞 GDI 调用，命令层负责 spawn_blocking）
    pub fn start_capture(&self, mode: &str) -> Result<TaskStartDto, AppError> {
        let capture = self
            .capture
            .read()
            .clone()
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_001", "模块未就绪"))?;
        let frame = capture.capture(CaptureTarget::FullScreen { monitor: 0 })?;
        if util::is_black_frame(&frame) {
            return Err(AppError::module(
                "SCREENSHOT_CAPTURE_002",
                "捕获到黑帧：目标可能受系统保护",
                Some("受 DRM 保护的窗口无法截取"),
            ));
        }
        // 虚拟桌面 bounds（覆盖层窗口定位；副屏负坐标场景见 docs/impl/03 P2）
        let (vx, vy, vw, vh) = capture
            .enumerate_monitors()
            .ok()
            .and_then(|m| {
                m.first()
                    .map(|i| (i.x, i.y, i.width as i32, i.height as i32))
            })
            .unwrap_or((0, 0, frame.width as i32, frame.height as i32));
        let task_id = uuid::Uuid::now_v7().to_string();
        let mut pending = self.pending.lock();
        pending.clear(); // 单任务模型：替换遗留任务，释放旧帧内存
        pending.insert(
            task_id.clone(),
            PendingTask {
                frame,
                mode: mode.to_owned(),
                info_b64: None,
            },
        );
        Ok(TaskStartDto {
            task_id,
            x: vx,
            y: vy,
            width: vw,
            height: vh,
        })
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

    /// 完成：解码前端合成图 → 执行动作（copy/save/pin/ocr）→ 入历史 → 发事件
    ///
    /// ocr 动作不阻塞识别：帧写 {appData}/frames 后只发 `screenshot.ocr_requested`
    /// 事件，结果经 `ocr.completed` 异步回流回填历史（D-09 第 1 步）
    pub fn finish(&self, task_id: &str, req: &FinishRequest) -> Result<FinishDto, AppError> {
        let (w, h, rgba) = util::decode_png_b64(&req.image_b64)?;

        // 配置动作 = 显式 actions + auto_* 兜底（前端总是显式传；auto_* 用于面板默认行为）
        let mut actions = req.actions.clone();
        let cfg = self
            .config
            .try_lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        if actions.is_empty() {
            if cfg.auto_save {
                actions.push("save".into());
            }
            if cfg.auto_copy {
                actions.push("copy".into());
            }
            if cfg.auto_pin {
                actions.push("pin".into());
            }
        }

        let mut file: Option<String> = None;
        let mut pin_id: Option<String> = None;
        let mut request_ocr = false;
        for action in &actions {
            match action.as_str() {
                "copy" => self.action_copy(w, h, &rgba)?,
                "save" => file = Some(self.action_save(w, h, &rgba)?),
                "pin" => {
                    let id = self.action_pin(
                        w,
                        h,
                        &rgba,
                        req.pin_x.unwrap_or(100),
                        req.pin_y.unwrap_or(100),
                    )?;
                    pin_id = Some(id);
                }
                // docs/impl/03 P5：Ocr 动作只转发事件，识别在 ocr-core 侧异步完成，
                // 结果经 ocr.completed 回流（历史回填 + 截图 UI），此处登记待触发
                "ocr" => request_ocr = true,
                other => tracing::warn!(action = other, "未知截图后处理动作，已忽略"),
            }
        }

        // 入历史
        let item = crate::types::ShotItem {
            id: task_id.to_owned(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            width: w,
            height: h,
            file: file.clone(),
            ocr_text: None,
        };
        if let Some(store) = self.store.read().clone() {
            store.insert(&item).ok();
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
        if request_ocr {
            if let Err(e) = self.dispatch_ocr_request(task_id, w, h, &rgba) {
                tracing::warn!(error = %e, "截图联动 OCR 请求失败，本次识别跳过");
            }
        }
        self.discard(task_id);
        Ok(FinishDto { file, pin_id })
    }

    /// 写联动帧到 {appData}/frames/{task_id}.png（tmp+rename，规约 5）并发布
    /// `screenshot.ocr_requested {task_id, frame_ref}`（DESIGN O1：截图与 OCR
    /// 只经事件交互，不经函数调用；帧文件由消费端读取后删除）
    fn dispatch_ocr_request(
        &self,
        task_id: &str,
        w: u32,
        h: u32,
        rgba: &[u8],
    ) -> Result<(), AppError> {
        use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};
        let Some(dir) = self.app_data() else {
            return Err(mod_err("SCREENSHOT_STATE_001", "模块未初始化"));
        };
        let frames = frames_dir(&dir);
        std::fs::create_dir_all(&frames)
            .map_err(|e| mod_err("SCREENSHOT_OCR_001", format!("创建帧目录失败: {e}")))?;
        let path = frames.join(format!("{task_id}.png"));
        let tmp = frames.join(format!(".{task_id}.png.tmp"));
        let encoded = {
            let f = std::fs::File::create(&tmp)
                .map_err(|e| mod_err("SCREENSHOT_OCR_001", format!("创建帧文件失败: {e}")))?;
            PngEncoder::new(f).write_image(rgba, w, h, ExtendedColorType::Rgba8)
        };
        if let Err(e) = encoded {
            let _ = std::fs::remove_file(&tmp);
            return Err(mod_err(
                "SCREENSHOT_OCR_001",
                format!("帧 PNG 编码失败: {e}"),
            ));
        }
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
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

    fn action_copy(&self, w: u32, h: u32, rgba: &[u8]) -> Result<(), AppError> {
        use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};
        let mut buf = std::io::Cursor::new(Vec::new());
        PngEncoder::new(&mut buf)
            .write_image(rgba, w, h, ExtendedColorType::Rgba8)
            .map_err(|e| mod_err("SCREENSHOT_ENCODE_001", format!("PNG 编码失败: {e}")))?;
        let clipboard = self
            .clipboard
            .read()
            .clone()
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_001", "ClipboardPort 未就绪"))?;
        clipboard.write(&host_core::ports::ClipContent::Image {
            format: "png".into(),
            width: w,
            height: h,
            bytes: std::sync::Arc::from(buf.into_inner().into_boxed_slice()),
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

    fn action_save(&self, w: u32, h: u32, rgba: &[u8]) -> Result<String, AppError> {
        let cfg = self
            .config
            .try_lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let dir = self.resolve_save_dir(&cfg);
        std::fs::create_dir_all(&dir)
            .map_err(|e| mod_err("SCREENSHOT_SAVE_001", format!("创建目录失败: {e}")))?;

        let ts = chrono::Local::now().format("%Y-%m-%d_%H%M%S").to_string();
        let stem = cfg.filename_template.replace("{ts}", &ts);
        // 文件名冲突：追加 _1 递增（docs/impl/03 P5）
        let mut path = dir.join(format!("{stem}.png"));
        let mut n = 1;
        while path.exists() {
            path = dir.join(format!("{stem}_{n}.png"));
            n += 1;
        }

        // 临时文件 + rename（规约 5）
        let tmp = dir.join(format!(
            ".{}.tmp",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        {
            let f = std::fs::File::create(&tmp)
                .map_err(|e| mod_err("SCREENSHOT_SAVE_002", format!("创建文件失败: {e}")))?;
            use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};
            PngEncoder::new(f)
                .write_image(rgba, w, h, ExtendedColorType::Rgba8)
                .map_err(|e| mod_err("SCREENSHOT_SAVE_003", format!("写入失败: {e}")))?;
        }
        std::fs::rename(&tmp, &path)
            .map_err(|e| mod_err("SCREENSHOT_SAVE_004", format!("落盘失败: {e}")))?;
        Ok(path.to_string_lossy().to_string())
    }

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
        {
            let f = std::fs::File::create(&path)
                .map_err(|e| mod_err("SCREENSHOT_PIN_003", format!("创建文件失败: {e}")))?;
            use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};
            PngEncoder::new(f)
                .write_image(rgba, w, h, ExtendedColorType::Rgba8)
                .map_err(|e| mod_err("SCREENSHOT_PIN_004", format!("写入失败: {e}")))?;
        }
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

    fn history_png(&self, id: &str) -> Result<(ShotItem, Vec<u8>), AppError> {
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
        let (item, bytes) = self.history_png(id)?;
        Ok(ShotDataDto {
            id: item.id,
            png_b64: host_core::util::b64_encode(&bytes),
        })
    }

    /// 再复制：png 原字节交 ClipboardPort，CF_DIB 转换由 win-integration 负责（module.rs action_copy 同通道）
    pub fn history_copy(&self, id: &str) -> Result<(), AppError> {
        let (item, bytes) = self.history_png(id)?;
        let clipboard = self
            .clipboard
            .read()
            .clone()
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_001", "ClipboardPort 未就绪"))?;
        clipboard.write(&host_core::ports::ClipContent::Image {
            format: "png".into(),
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
}
