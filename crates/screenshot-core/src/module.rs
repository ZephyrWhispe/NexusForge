//! ScreenshotModule：截图任务状态机 + Pin 贴图 + 历史记录（docs/impl/03 P2/P5/P6/P8）
//!
//! 任务流（覆盖层前端驱动，Rust 持有帧数据）：
//! start_capture → 覆盖层 task_info → 选区 confirm → 标注 → finish(合成图+动作)
//!
//! v1 简化（相对 docs/impl/03）：
//! - 捕获走 GDI BitBlt（win-integration capture.rs），Windows.Graphics.Capture 后续迭代；
//! - 最终合成图由前端 canvas 导出（预览即导出），Rust 侧 skia/ab_glyph 渲染延后；
//! - 录屏（P7）独立里程碑交付。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use host_core::capability::{HotkeyAction, HotkeyBinding, HotkeyProvider};
use host_core::error::{AppError, ModuleError};
use host_core::events::{Event, EventBus};
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};
use host_core::ports::{CapturePort, CaptureTarget, ClipboardPort};
use tokio::sync::Mutex as AsyncMutex;

use crate::store::ShotStore;
use crate::types::{
    ConfirmRect, CropDto, FinishDto, FinishRequest, PinDataDto, PinDto, ScreenshotConfig,
    TaskInfoDto, TaskStartDto,
};
use crate::util;

fn mod_err(code: &str, m: impl Into<String>) -> AppError {
    AppError::module(code, m.into(), None)
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
    state: AtomicU8,
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
            state: AtomicU8::new(0),
        }
    }

    fn app_data(&self) -> Option<PathBuf> {
        self.app_data_dir.read().ok().and_then(|g| g.clone())
    }

    /// pins.json 原子写（临时文件 + rename，规约 5）
    fn persist_pins(&self, pins: &[PinRecord]) -> Result<(), ModuleError> {
        let Some(dir) = self.app_data() else { return Ok(()) };
        let path = dir.join("pins.json");
        let json = serde_json::to_vec_pretty(pins)
            .map_err(|e| ModuleError::Storage(e.to_string()))?;
        let tmp = dir.join("pins.json.tmp");
        std::fs::write(&tmp, json).map_err(|e| ModuleError::Storage(e.to_string()))?;
        std::fs::rename(&tmp, &path).map_err(|e| ModuleError::Storage(e.to_string()))?;
        Ok(())
    }

    /// init 时恢复 pins：文件已丢失的记录直接丢弃（docs/impl/03 P6）
    fn restore_pins(&self) {
        let Some(dir) = self.app_data() else { return };
        let Ok(bytes) = std::fs::read(dir.join("pins.json")) else { return };
        let pins: Vec<PinRecord> = serde_json::from_slice(&bytes).unwrap_or_default();
        let alive: Vec<PinRecord> = pins
            .into_iter()
            .filter(|p| dir.join(&p.file).is_file())
            .collect();
        let dropped = {
            let mut g = self.pins.lock().expect("pins 锁");
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
            priority: 10,
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let store = Arc::new(
            ShotStore::open(&ctx.app_data_dir.join("db").join("screenshot.db"))
                .map_err(|e| ModuleError::Storage(e.to_string()))?,
        );
        let capture = ctx
            .ports
            .get::<dyn CapturePort>()
            .ok_or_else(|| ModuleError::Init("CapturePort 未注册（win-integration 缺失）".into()))?;
        let clipboard = ctx
            .ports
            .get::<dyn ClipboardPort>()
            .ok_or_else(|| ModuleError::Init("ClipboardPort 未注册".into()))?;

        *self.app_data_dir.write().map_err(|_| ModuleError::Init("锁污染".into()))? =
            Some(ctx.app_data_dir.clone());
        *self.store.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(store);
        *self.capture.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(capture);
        *self.clipboard.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(clipboard);
        *self.bus.write().map_err(|_| ModuleError::Init("锁污染".into()))? =
            Some(ctx.event_bus.clone());
        self.restore_pins();
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        self.state.store(2, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        self.pending.lock().expect("pending 锁").clear();
        self.state.store(1, Ordering::SeqCst);
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
        match self.state.load(Ordering::SeqCst) {
            0 => ModuleState::Uninitialized,
            1 => ModuleState::Stopped,
            _ => ModuleState::Running,
        }
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
        let bus = self.bus.read().ok().and_then(|g| g.clone());
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
            .ok()
            .and_then(|g| g.clone())
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
            .and_then(|m| m.first().map(|i| (i.x, i.y, i.width as i32, i.height as i32)))
            .unwrap_or((0, 0, frame.width as i32, frame.height as i32));
        let task_id = uuid::Uuid::now_v7().to_string();
        let mut pending = self.pending.lock().expect("pending 锁");
        pending.clear(); // 单任务模型：替换遗留任务，释放旧帧内存
        pending.insert(
            task_id.clone(),
            PendingTask { frame, mode: mode.to_owned(), info_b64: None },
        );
        Ok(TaskStartDto { task_id, x: vx, y: vy, width: vw, height: vh })
    }

    /// 覆盖层取背景帧（PNG Base64，编码一次后缓存）
    pub fn task_info(&self, task_id: &str) -> Result<TaskInfoDto, AppError> {
        let mut pending = self.pending.lock().expect("pending 锁");
        let task = pending
            .get_mut(task_id)
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_002", "任务不存在或已结束"))?;
        let b64 = match &task.info_b64 {
            Some(b) => b.clone(),
            None => {
                let rgba = util::bgra_to_rgba(&task.frame);
                let b64 =
                    util::encode_png_b64(task.frame.width, task.frame.height, &rgba)?;
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
        let pending = self.pending.lock().expect("pending 锁");
        let task = pending
            .get(task_id)
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_002", "任务不存在或已结束"))?;
        let (w, h, rgba) = util::crop_bgra(
            &task.frame,
            host_core::ports::Rect { x: rect.x, y: rect.y, w: rect.w, h: rect.h },
        )?;
        let png_b64 = util::encode_png_b64(w, h, &rgba)?;
        Ok(CropDto { png_b64, width: w, height: h })
    }

    pub fn discard(&self, task_id: &str) {
        self.pending.lock().expect("pending 锁").remove(task_id);
    }

    /// 完成：解码前端合成图 → 执行动作（copy/save/pin）→ 入历史 → 发事件
    pub fn finish(
        &self,
        task_id: &str,
        req: &FinishRequest,
    ) -> Result<FinishDto, AppError> {
        let (w, h, rgba) = util::decode_png_b64(&req.image_b64)?;

        // 配置动作 = 显式 actions + auto_* 兜底（前端总是显式传；auto_* 用于面板默认行为）
        let mut actions = req.actions.clone();
        let cfg = self.config.try_lock().map(|g| g.clone()).unwrap_or_default();
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
        if let Some(store) = self.store.read().ok().and_then(|g| g.clone()) {
            store.insert(&item).ok();
        }

        if let Some(bus) = self.bus.read().ok().and_then(|g| g.clone()) {
            bus.publish(Event::new(
                "screenshot.taken",
                "screenshot",
                serde_json::json!({ "task_id": task_id, "file": file }),
            ))
            .ok();
        }
        self.discard(task_id);
        Ok(FinishDto { file, pin_id })
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
            .ok()
            .and_then(|g| g.clone())
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
        let cfg = self.config.try_lock().map(|g| g.clone()).unwrap_or_default();
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
        let tmp = dir.join(format!(".{}.tmp", path.file_name().unwrap_or_default().to_string_lossy()));
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
            let mut pins = self.pins.lock().expect("pins 锁");
            pins.push(record);
            let snapshot = pins.clone();
            drop(pins);
            self.persist_pins(&snapshot).map_err(|e| {
                mod_err("SCREENSHOT_PIN_005", e.to_string())
            })?;
        }
        Ok(id)
    }

    // ---------------- Pin 贴图（P6）----------------

    pub fn pins(&self) -> Vec<PinDto> {
        self.pins
            .lock()
            .expect("pins 锁")
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
        let pins = self.pins.lock().expect("pins 锁");
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
            png_b64: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                bytes,
            ),
            x: record.x,
            y: record.y,
            width: record.width,
            height: record.height,
            zoom: record.zoom,
            opacity: record.opacity,
        })
    }

    pub fn pin_update(&self, id: &str, zoom: f32, opacity: f32) -> Result<(), AppError> {
        let mut pins = self.pins.lock().expect("pins 锁");
        let record = pins
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| mod_err("SCREENSHOT_PIN_006", "贴图不存在"))?;
        record.zoom = zoom.clamp(0.2, 5.0);
        record.opacity = opacity.clamp(0.2, 1.0);
        let snapshot = pins.clone();
        drop(pins);
        self.persist_pins(&snapshot).map_err(|e| mod_err("SCREENSHOT_PIN_005", e.to_string()))?;
        Ok(())
    }

    pub fn pin_close(&self, id: &str) -> Result<(), AppError> {
        let removed = {
            let mut pins = self.pins.lock().expect("pins 锁");
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
        if let Some(store) = self.store.read().ok().and_then(|g| g.clone()) {
            store.set_ocr_text(task_id, text).ok();
        }
    }

    /// 历史库句柄（screenshot_history_list IPC 用）
    pub fn history_store(&self) -> Option<Arc<ShotStore>> {
        self.store.read().ok().and_then(|g| g.clone())
    }

    /// 文本写入剪贴板（OCR 结果"复制全部"等；写系统剪贴板，
    /// 剪贴板模块将其作为正常捕获入库）
    pub fn copy_text(&self, content: &host_core::ports::ClipContent) -> Result<(), AppError> {
        let clipboard = self
            .clipboard
            .read()
            .ok()
            .and_then(|g| g.clone())
            .ok_or_else(|| mod_err("SCREENSHOT_STATE_001", "ClipboardPort 未就绪"))?;
        clipboard.write(content)
    }
}
