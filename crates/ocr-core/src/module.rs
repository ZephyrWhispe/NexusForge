//! OcrModule：模块生命周期 + 识别入口 + 引擎状态（docs/impl/04 O5/O7/O8）
//!
//! 触发路径（v1）：
//! - overlay 前端直接 IPC `ocr_recognize`（截图选区 → 立即识别，闭环最短）；
//! - 全局快捷键 Ctrl+Alt+O → 呼出 overlay（mode=ocr，选区后自动识别）；
//! - 事件联动（screenshot.ocr_requested → ocr.completed）保留主题，插件阶段接入。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use host_core::capability::{HotkeyAction, HotkeyBinding, HotkeyProvider};
use host_core::error::{AppError, ModuleError};
use host_core::events::{Event, EventBus};
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};
use host_core::ports::OcrPort;
use tokio::sync::Mutex as AsyncMutex;

use crate::pipeline::OcrPipeline;
use crate::types::{EngineInfo, EngineStatusDto, OcrRequest, OcrResultDto};

fn mod_err(code: &str, m: impl Into<String>) -> AppError {
    AppError::module(code, m.into(), None)
}

pub struct OcrModule {
    port: RwLock<Option<Arc<dyn OcrPort>>>,
    bus: RwLock<Option<Arc<EventBus>>>,
    /// 引擎可用性缓存（start 时探测）
    languages: RwLock<Vec<String>>,
    state: AtomicU8,
    /// 保留异步配置槽位（与其它模块一致的结构，v1 无配置项）
    _config: Arc<AsyncMutex<()>>,
}

impl OcrModule {
    pub fn new() -> Self {
        Self {
            port: RwLock::new(None),
            bus: RwLock::new(None),
            languages: RwLock::new(Vec::new()),
            state: AtomicU8::new(0),
            _config: Arc::new(AsyncMutex::new(())),
        }
    }

    /// 识别（阻塞：PNG 解码 + 引擎调用；命令层负责 spawn_blocking + 超时）
    pub fn recognize(&self, req: &OcrRequest) -> Result<OcrResultDto, AppError> {
        let port = self
            .port
            .read()
            .ok()
            .and_then(|g| g.clone())
            .ok_or_else(|| mod_err("OCR_STATE_001", "模块未就绪"))?;
        let (w, h, rgba) = decode_rgba(&req.image_b64)?;

        // 预处理（docs/impl/04 O4 ①）：> 4096px 等比缩到 4096
        let (fw, fh, frame_rgba) = downscale_if_needed(w, h, rgba);
        let frame = host_core::ports::Frame {
            width: fw,
            height: fh,
            bgra: std::sync::Arc::from(rgba_to_bgra(&frame_rgba).into_boxed_slice()),
            dpi_scale: 1.0,
            monitor_id: 0,
        };

        let pipeline = OcrPipeline::new(port);
        let result = pipeline.run(&frame, &req.langs)?;

        // 关联历史回填（经截图模块暴露的记录接口由命令层调用；此处只发事件）
        if let Some(bus) = self.bus.read().ok().and_then(|g| g.clone()) {
            bus.publish(Event::new(
                "ocr.completed",
                "ocr",
                serde_json::json!({
                    "source_task_id": req.source_task_id,
                    "text": result.text,
                    "engine": result.engine,
                }),
            ))
            .ok();
        }
        Ok(result)
    }

    /// 引擎状态（O8：start 时探测，此处读缓存）
    pub fn status(&self) -> EngineStatusDto {
        let langs = self
            .languages
            .read()
            .map(|g| g.clone())
            .unwrap_or_default();
        EngineStatusDto {
            engines: vec![EngineInfo {
                id: "win-ocr".into(),
                name: "Windows.Media.Ocr".into(),
                available: !langs.is_empty() || self.state.load(Ordering::SeqCst) == 2,
            }],
            languages: langs,
        }
    }
}

impl Default for OcrModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for OcrModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "ocr",
            name: "OCR 识别",
            version: "0.1.0",
            icon: Some("ocr"),
            priority: 10,
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let port = ctx
            .ports
            .get::<dyn OcrPort>()
            .ok_or_else(|| ModuleError::Init("OcrPort 未注册（win-integration 缺失）".into()))?;
        *self.port.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(port);
        *self.bus.write().map_err(|_| ModuleError::Init("锁污染".into()))? =
            Some(ctx.event_bus.clone());
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        // 探测可用语言（失败不阻断启动，识别时给出可操作错误）
        if let Some(port) = self.port.read().ok().and_then(|g| g.clone()) {
            match port.available_languages() {
                Ok(langs) => {
                    if let Ok(mut g) = self.languages.write() {
                        *g = langs;
                    }
                    tracing::info!(count = g_count(&self.languages), "win-ocr 语言探测完成");
                }
                Err(e) => tracing::warn!(error = %e, "win-ocr 语言探测失败"),
            }
        }
        self.state.store(2, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        self.state.store(1, Ordering::SeqCst);
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

fn g_count(langs: &RwLock<Vec<String>>) -> usize {
    langs.read().map(|g| g.len()).unwrap_or(0)
}

impl HotkeyProvider for OcrModule {
    fn global_hotkeys(&self) -> Vec<HotkeyBinding> {
        vec![HotkeyBinding {
            id: "ocr.region".into(),
            label: "OCR 屏幕取词".into(),
            // MOD_CONTROL(0x02) | MOD_ALT(0x01) | MOD_NOREPEAT(0x4000)
            modifiers: 0x02 | 0x01 | 0x4000,
            vk: 0x4F, // 'O'
        }]
    }

    fn hotkey_actions(&self) -> Vec<HotkeyAction> {
        let bus = self.bus.read().ok().and_then(|g| g.clone());
        let Some(bus) = bus else { return vec![] };
        vec![HotkeyAction {
            binding_id: "ocr.region".into(),
            action: Arc::new(move || {
                bus.publish(Event::new(
                    "screenshot.overlay_requested",
                    "ocr",
                    serde_json::json!({ "mode": "ocr" }),
                ))
                .ok();
            }),
        }]
    }
}

// ---------------- 像素工具（与 screenshot-core/util 解耦：模块间不互依赖）----------------

fn decode_rgba(b64: &str) -> Result<(u32, u32, Vec<u8>), AppError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| mod_err("OCR_INPUT_002", format!("Base64 解码失败: {e}")))?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| mod_err("OCR_INPUT_003", format!("PNG 解码失败: {e}")))?;
    let rgba = img.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// > 4096px 等比缩放（docs/impl/04 O2 潜在问题 3）
fn downscale_if_needed(w: u32, h: u32, rgba: Vec<u8>) -> (u32, u32, Vec<u8>) {
    const MAX: u32 = 4096;
    if w <= MAX && h <= MAX {
        return (w, h, rgba);
    }
    let scale = MAX as f32 / (w.max(h) as f32);
    let nw = ((w as f32 * scale) as u32).max(1);
    let nh = ((h as f32 * scale) as u32).max(1);
    match image::RgbaImage::from_raw(w, h, rgba) {
        Some(img) => {
            let resized = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3);
            (nw, nh, resized.into_raw())
        }
        None => (w, h, Vec::new()), // 尺寸与缓冲不匹配属内部错误，交给后续校验
    }
}

/// RGBA → BGRA（OcrPort 契约为 BGRA 帧；alpha 已无意义，置 255）
fn rgba_to_bgra(rgba: &[u8]) -> Vec<u8> {
    let n = rgba.len() / 4;
    let mut out = vec![255u8; n * 4];
    for i in 0..n {
        out[i * 4] = rgba[i * 4 + 2];
        out[i * 4 + 1] = rgba[i * 4 + 1];
        out[i * 4 + 2] = rgba[i * 4];
    }
    out
}
