//! OcrModule：模块生命周期 + 识别入口 + 引擎注册表（docs/impl/04 O1/O5/O7/O8）
//!
//! 触发路径：
//! - overlay 前端直接 IPC `ocr_recognize`（截图选区 → 立即识别，闭环最短）；
//! - 全局快捷键 Ctrl+Alt+O → 呼出 overlay（mode=ocr，选区后自动识别）；
//! - 截图联动（D-09 第 1 步）：订阅 `screenshot.ocr_requested {task_id, frame_ref}`
//!   → 读取联动帧临时 PNG → 管线 → 发 `ocr.completed {source_task_id, text, engine}`
//!   （截图 UI 与历史回填消费）；失败发 `ocr.failed`，临时帧随消费删除。

use parking_lot::RwLock;

use std::path::PathBuf;
use std::sync::Arc;

use host_core::capability::{HotkeyAction, HotkeyBinding, HotkeyProvider};
use host_core::error::{AppError, ModuleError};
use host_core::events::{Event, EventBus};
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};
use host_core::ports::OcrPort;
use tokio::sync::watch;

use crate::engine::{EngineRegistry, WinOcrEngine};
use crate::pipeline::OcrPipeline;
use crate::tesseract::{
    CommandRunner, SystemRunner, TesseractEngine, TesseractSettings, TESSERACT_ID,
};
use crate::types::{EngineStatusDto, OcrConfig, OcrConfigPatch, OcrRequest, OcrResultDto};

use host_core::util::app_err as mod_err;

pub struct OcrModule {
    engines: RwLock<Option<Arc<EngineRegistry>>>,
    bus: RwLock<Option<Arc<EventBus>>>,
    state: ModuleStateCell,
    /// 事件消费协作文档/停机信道（S4 模式，同 automation-core）
    shutdown: RwLock<Option<watch::Sender<bool>>>,
    /// Tesseract 设置（引擎项随其变更重建，见 [`build_registry`]）
    tess: RwLock<TesseractSettings>,
    /// 子进程执行口（真机 SystemRunner；测试面走 build_registry 直注假执行器）
    runner: Arc<dyn CommandRunner>,
    /// {app_data}：TesseractEngine 的一次性临时帧目录落点
    app_data: RwLock<Option<PathBuf>>,
}

impl OcrModule {
    pub fn new() -> Self {
        Self {
            engines: RwLock::new(None),
            bus: RwLock::new(None),
            state: ModuleStateCell::new(),
            shutdown: RwLock::new(None),
            tess: RwLock::new(TesseractSettings::default()),
            runner: Arc::new(SystemRunner),
            app_data: RwLock::new(None),
        }
    }

    fn registry(&self) -> Result<Arc<EngineRegistry>, AppError> {
        self.engines
            .read()
            .clone()
            .ok_or_else(|| mod_err("OCR_STATE_001", "模块未就绪"))
    }

    /// 识别（阻塞：PNG 解码 + 引擎调用；命令层负责 spawn_blocking + 超时）
    pub fn recognize(&self, req: &OcrRequest) -> Result<OcrResultDto, AppError> {
        let registry = self.registry()?;
        let bytes = host_core::util::b64_decode(req.image_b64.trim())
            .ok_or_else(|| mod_err("OCR_INPUT_002", "Base64 解码失败"))?;
        let result = run_engine(&registry, &bytes, &req.langs)?;
        if let Some(bus) = self.bus.read().clone() {
            publish_completed(&bus, req.source_task_id.as_deref(), &result);
        }
        Ok(result)
    }

    /// 引擎状态（O8：注册表逐引擎探测，无硬编码项）
    pub fn status(&self) -> EngineStatusDto {
        match self.engines.read().clone() {
            Some(registry) => EngineStatusDto {
                engines: registry.status(),
                languages: registry.languages(),
            },
            None => EngineStatusDto {
                engines: vec![],
                languages: vec![],
            },
        }
    }

    /// 运行态配置快照（`ocr_config_get` 读口：设置中心显示的是模块真正在用的值，
    /// 写侧仍只有 host_config_set 一个入口）
    pub fn config(&self) -> OcrConfig {
        let mut cfg = OcrConfig::default();
        if let Some(registry) = self.engines.read().clone() {
            cfg.langs = registry.config_langs();
            cfg.preferred_engine = registry.preferred();
        }
        let tess = self.tess.read();
        cfg.tesseract_enabled = tess.enabled;
        cfg.tesseract_exe = tess.exe.clone();
        cfg.tesseract_data_dir = tess.data_dir.clone();
        cfg.tesseract_timeout_ms = tess.timeout_ms;
        cfg
    }

    /// （重）接截图联动消费协程：协程持有的是接手那一刻的 `Arc<EngineRegistry>`，
    /// 而 Tesseract 开关会重建注册表——不重接就会出现"直接 IPC 走新引擎集、
    /// 截图联动走旧引擎集"的两条路径分叉。非运行态零副作用（stop 已负责停机）。
    fn rewire_consumer(&self) {
        if self.state.get() != ModuleState::Running {
            return;
        }
        let bus = self.bus.read().clone();
        let registry = self.engines.read().clone();
        let (Some(bus), Some(registry)) = (bus, registry) else {
            return;
        };
        if let Some(tx) = self.shutdown.write().take() {
            tx.send(true).ok();
        }
        let (tx, rx) = watch::channel(false);
        *self.shutdown.write() = Some(tx);
        // 句柄即弃：协程终结由停机信道控制，与 JoinHandle 无关
        drop(spawn_ocr_requested_consumer(bus, registry, rx));
    }
}

/// 按设置重建引擎注册表（§9.1-⑭：注册表引擎集不可变，"第二引擎"是重建出来的而非 push 进去的）
///
/// `base` 是其余引擎项（含构造期的 win-ocr、测试注入的假引擎）；Tesseract 项按
/// `tess.enabled` 追加，关着的时候连"不可用"都不该出现在引擎状态面上。
pub(crate) fn build_registry(
    base: Vec<Arc<dyn crate::engine::OcrEngine>>,
    tess: &TesseractSettings,
    runner: Arc<dyn CommandRunner>,
    app_data: PathBuf,
) -> Arc<EngineRegistry> {
    let mut engines = base;
    if tess.enabled {
        engines.push(Arc::new(TesseractEngine::new(
            Arc::new(RwLock::new(tess.clone())),
            runner,
            app_data,
        )));
    }
    Arc::new(EngineRegistry::new(engines))
}

/// 共享识别核心：图像字节（PNG 等）→ 预处理 → 管线（直接 IPC 与事件联动两条路径共用）
///
/// `langs` 是**本次请求的显式覆盖**；空表不是"无偏好"，而是交注册表按用户配置解析
/// （单一决策点 engine::resolve_langs）——联动路径传 `&[]` 即"跟随设置"。
pub(crate) fn run_engine(
    registry: &EngineRegistry,
    bytes: &[u8],
    langs: &[String],
) -> Result<OcrResultDto, AppError> {
    let (w, h, rgba) = decode_rgba(bytes)?;
    // 预处理（docs/impl/04 O4 ①）：> 4096px 等比缩到 4096
    let (fw, fh, frame_rgba) = downscale_if_needed(w, h, rgba);
    let frame = host_core::ports::Frame {
        width: fw,
        height: fh,
        bgra: Arc::from(rgba_to_bgra(&frame_rgba).into_boxed_slice()),
        dpi_scale: 1.0,
        monitor_id: 0,
    };
    OcrPipeline::new(registry).run(&frame, langs)
}

pub(crate) fn publish_completed(
    bus: &EventBus,
    source_task_id: Option<&str>,
    result: &OcrResultDto,
) {
    // 关联历史回填（截图模块订阅 ocr.completed 写回历史；此处只发事件）
    bus.publish(Event::new(
        "ocr.completed",
        "ocr",
        serde_json::json!({
            "source_task_id": source_task_id,
            "text": result.text,
            "engine": result.engine,
        }),
    ))
    .ok();
}

fn publish_failed(bus: &EventBus, source_task_id: &str, reason: &str) {
    bus.publish(Event::new(
        "ocr.failed",
        "ocr",
        serde_json::json!({
            "source_task_id": source_task_id,
            "reason": reason,
        }),
    ))
    .ok();
}

/// 订阅 `screenshot.ocr_requested` 的消费协程（D-09 第 1 步接线，可独立测试）：
/// 读取 frame_ref 临时帧 → 识别 → 发 ocr.completed / ocr.failed → 删除临时帧。
/// frame_ref 只可能来自本进程截图模块写入的 {appData}/frames 临时文件（总线无外部发布方）。
pub(crate) fn spawn_ocr_requested_consumer(
    bus: Arc<EventBus>,
    registry: Arc<EngineRegistry>,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let mut rx = match bus.subscribe("screenshot.ocr_requested") {
        Ok(rx) => rx,
        Err(e) => {
            tracing::warn!(error = %e, "screenshot.ocr_requested 订阅失败，截图联动 OCR 不可用");
            return tokio::spawn(async {});
        }
    };
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                biased;
                ch = shutdown.changed() => {
                    if ch.is_err() || *shutdown.borrow_and_update() { break; }
                    continue;
                }
                received = rx.recv() => match received {
                    Ok(ev) => ev,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                },
            };
            let task_id = event
                .payload
                .get("task_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned();
            let frame_ref = event
                .payload
                .get("frame_ref")
                .and_then(|v| v.as_str())
                .map(PathBuf::from);
            let Some(frame_ref) = frame_ref else {
                tracing::warn!(task_id, "ocr_requested 缺少 frame_ref，已跳过");
                publish_failed(&bus, &task_id, "缺少 frame_ref");
                continue;
            };
            let reg = registry.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                let read = std::fs::read(&frame_ref)
                    .map_err(|e| mod_err("OCR_INPUT_004", format!("读取联动帧失败: {e}")));
                let result = read.and_then(|bytes| run_engine(&reg, &bytes, &[]));
                // 临时帧随消费删除（无论成败，避免 {appData}/frames 堆积）
                let _ = std::fs::remove_file(&frame_ref);
                result
            })
            .await;
            match outcome {
                Ok(Ok(result)) => publish_completed(
                    &bus,
                    (!task_id.is_empty()).then_some(task_id.as_str()),
                    &result,
                ),
                Ok(Err(e)) => {
                    tracing::warn!(task_id, error = %e, "联动 OCR 识别失败");
                    publish_failed(&bus, &task_id, &e.to_string());
                }
                Err(e) => publish_failed(&bus, &task_id, &format!("识别任务异常: {e}")),
            }
        }
    })
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
            priority: priority_of("ocr"),
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let port = ctx
            .ports
            .get::<dyn OcrPort>()
            .ok_or_else(|| ModuleError::Init("OcrPort 未注册（win-integration 缺失）".into()))?;
        // O1：唯一内置引擎以注册表项形式登记（D-09 第 1 步）；第二引擎（Tesseract CLI）
        // 按设置在此重建——默认关，故启动期注册表仍只有 win-ocr 一项
        *self.app_data.write() = Some(ctx.app_data_dir.clone());
        let base: Vec<Arc<dyn crate::engine::OcrEngine>> = vec![Arc::new(WinOcrEngine::new(port))];
        *self.engines.write() = Some(build_registry(
            base,
            &self.tess.read(),
            self.runner.clone(),
            ctx.app_data_dir.clone(),
        ));
        *self.bus.write() = Some(ctx.event_bus.clone());
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        let registry = self
            .engines
            .read()
            .clone()
            .ok_or_else(|| ModuleError::Start("引擎注册表未初始化".into()))?;
        // 探测可用语言与引擎（失败不阻断启动，识别时给出可操作错误）
        match registry.pick(&[]) {
            Ok((engine, _)) => tracing::info!(
                engine = engine.id(),
                langs = registry.languages().len(),
                "OCR 引擎探测完成"
            ),
            Err(e) => tracing::warn!(error = %e, "OCR 引擎探测失败"),
        }
        // D-09 第 1 步：接线截图联动（每次 start 重建协作停机信道，S4）
        self.state.set(ModuleState::Running);
        self.rewire_consumer();
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        if let Some(tx) = self.shutdown.write().take() {
            tx.send(true).ok();
        }
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    /// 引擎词表来自注册表实际 id（不手写字面表）：T-B4-11 的第二引擎落地即自动出现在
    /// 设置中心，无需再改本文件。构造期（init 前）注册表尚不存在，此时**不给 enum**
    /// 而非给一个假词表——bootstrap_modules 在 init 之后重刷一遍 schema。
    fn config_schema(&self) -> serde_json::Value {
        let ids: Vec<String> = self
            .engines
            .read()
            .clone()
            .map(|registry| registry.status().into_iter().map(|e| e.id).collect())
            .unwrap_or_default();
        let mut engine_prop = serde_json::json!({
            "type": "string", "title": "优先 OCR 引擎",
            "description": "识别时优先尝试的引擎；不可用自动降级到其余引擎"
        });
        if !ids.is_empty() {
            engine_prop["enum"] = serde_json::json!(ids);
            engine_prop["default"] = serde_json::json!(self.config().preferred_engine);
        }
        serde_json::json!({
            "type": "object",
            "properties": {
                "preferred_engine": engine_prop,
                "langs": {
                    "type": "array", "items": { "type": "string" },
                    "title": "偏好语言（BCP-47，逗号分隔）",
                    "description": "识别时优先尝试的语言，按序取引擎支持的首个；留空 = 由系统语言决定（单次请求里手选的语言优先于本设置）",
                    "default": []
                },
                // Tesseract CLI 第二引擎四键（T-B4-11）：默认关；路径校验在 apply_config
                "tesseract_enabled": {
                    "type": "boolean",
                    "title": "启用 Tesseract 引擎（本地命令行）",
                    "description": "用你自装的 tesseract.exe 作第二引擎（子进程调用，不经 shell）；关闭时它不出现在引擎列表里",
                    "default": false
                },
                "tesseract_exe": {
                    "type": "string",
                    "title": "Tesseract 可执行文件（绝对路径）",
                    "description": "必须是绝对路径，例 C:\\Program Files\\Tesseract-OCR\\tesseract.exe；裸程序名与相对路径会走 PATH 解析，等同给任意同名程序开门，故直接拒",
                    "default": ""
                },
                "tesseract_data_dir": {
                    "type": "string",
                    "title": "tessdata 目录（留空 = 用引擎自带缺省）",
                    "description": "语言包所在目录，传给 --tessdata-dir",
                    "default": ""
                },
                "tesseract_timeout_ms": {
                    "type": "integer",
                    "title": "单次识别超时（毫秒）",
                    "description": "超时即强杀子进程并报错，不会挂着不动",
                    "minimum": 1000,
                    "maximum": 120000,
                    "default": 20000
                }
            }
        })
    }

    /// 补丁 → 生效值 → 校验 → 重建注册表 → 一次 swap：任一校验不过则运行态零改动
    /// （含"启用 Tesseract 却没填绝对路径"与"未知优先引擎"两臂，不留半写状态）
    fn apply_config(&self, values: serde_json::Value) -> Result<(), ModuleError> {
        let patch: OcrConfigPatch = serde_json::from_value(values)
            .map_err(|e| ModuleError::Config(format!("ocr 配置无效: {e}")))?;
        let Some(registry) = self.engines.read().clone() else {
            return Err(ModuleError::Config("引擎注册表未初始化".into()));
        };
        let Some(app_data) = self.app_data.read().clone() else {
            return Err(ModuleError::Config(
                "ocr 模块未初始化（缺 app_data 目录）".into(),
            ));
        };
        // 缺键 = 不动运行态：生效值 = 现运行态 ⊕ 补丁（OcrConfigPatch 的 None 臂保留现值）
        let next = self.config().merged(&patch);
        let tess = next.tesseract();
        tess.validate()
            .map_err(|e| ModuleError::Config(e.to_string()))?;
        if next.preferred_engine == TESSERACT_ID && !tess.enabled {
            // 单看 set_preferred 只会得到"未知引擎 tesseract"，而真因是本次把引擎关了
            return Err(ModuleError::Config(
                "优先引擎为 Tesseract 时不能同时关闭它（请同批把优先引擎改为其余项）".into(),
            ));
        }
        let rebuilt = build_registry(
            registry.engines_except(TESSERACT_ID),
            &tess,
            self.runner.clone(),
            app_data,
        );
        rebuilt.set_config_langs(&next.langs);
        rebuilt
            .set_preferred(&next.preferred_engine)
            .map_err(|e| ModuleError::Config(e.to_string()))?;
        *self.tess.write() = tess;
        *self.engines.write() = Some(rebuilt);
        self.rewire_consumer();
        Ok(())
    }

    fn status(&self) -> ModuleState {
        self.state.get()
    }

    fn set_status(&self, state: ModuleState) {
        self.state.set(state);
    }
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
        let bus = self.bus.read().clone();
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

fn decode_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), AppError> {
    let img = image::load_from_memory(bytes)
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
            let resized =
                image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3);
            (nw, nh, resized.into_raw())
        }
        None => (w, h, Vec::new()), // 尺寸与缓冲不匹配属内部错误，交给后续校验
    }
}

/// RGBA → BGRA（OcrEngine 契约为 BGRA 帧；alpha 已无意义，置 255）
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::OcrEngine;
    use host_core::events::Event;
    use host_core::ports::{Frame, OcrLine, Rect};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingEngine {
        calls: Arc<AtomicUsize>,
    }
    impl OcrEngine for CountingEngine {
        fn id(&self) -> &'static str {
            "mock"
        }
        fn display_name(&self) -> &'static str {
            "Mock Engine"
        }
        fn available(&self) -> Result<Vec<String>, AppError> {
            Ok(vec!["zh-CN".into()])
        }
        fn recognize(&self, frame: &Frame, _lang: &str) -> Result<Vec<OcrLine>, AppError> {
            assert_eq!((frame.width, frame.height), (2, 2));
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![OcrLine {
                text: "你好".into(),
                rect: Rect {
                    x: 0.0,
                    y: 0.1,
                    w: 0.5,
                    h: 0.1,
                },
                confidence: 1.0,
            }])
        }
    }

    fn test_registry() -> (Arc<EngineRegistry>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(EngineRegistry::new(vec![Arc::new(CountingEngine {
                calls: calls.clone(),
            })])),
            calls,
        )
    }

    fn png_bytes() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    /// D-09 验收：publish screenshot.ocr_requested → 消费协程识别 → ocr.completed
    /// 闭环，临时帧随消费删除，stop 信号令协程退出
    #[tokio::test]
    async fn ocr_requested_roundtrip_publishes_completed() {
        let bus = Arc::new(EventBus::new());
        let mut done = bus.subscribe("ocr.completed").unwrap();
        let (registry, calls) = test_registry();
        let dir = tempfile::tempdir().unwrap();
        let frame_path = dir.path().join("t-link-1.png");
        std::fs::write(&frame_path, png_bytes()).unwrap();

        let (tx, rx) = watch::channel(false);
        let consumer = spawn_ocr_requested_consumer(bus.clone(), registry, rx);
        bus.publish(Event::new(
            "screenshot.ocr_requested",
            "screenshot",
            json!({ "task_id": "t-link-1", "frame_ref": frame_path.to_string_lossy() }),
        ))
        .unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(5), done.recv())
            .await
            .expect("应收到 ocr.completed")
            .unwrap();
        assert_eq!(ev.payload["source_task_id"], "t-link-1");
        assert_eq!(ev.payload["text"], "你好");
        assert_eq!(ev.payload["engine"], "mock");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!frame_path.exists(), "临时帧应随消费删除");
        tx.send(true).ok();
        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(2), consumer)
            .await
            .expect("停机信号后消费协程应退出")
            .unwrap();
    }

    /// 联动帧不可读（路径缺失）→ ocr.failed 携 task_id 与原因，不静默吞
    #[tokio::test]
    async fn ocr_requested_unreadable_frame_publishes_failed() {
        let bus = Arc::new(EventBus::new());
        let mut failed = bus.subscribe("ocr.failed").unwrap();
        let (registry, _) = test_registry();
        let (_tx, rx) = watch::channel(false);
        spawn_ocr_requested_consumer(bus.clone(), registry, rx);
        bus.publish(Event::new(
            "screenshot.ocr_requested",
            "screenshot",
            json!({ "task_id": "t-bad", "frame_ref": "Z:/definitely/not/here.png" }),
        ))
        .unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(5), failed.recv())
            .await
            .expect("应收到 ocr.failed")
            .unwrap();
        assert_eq!(ev.payload["source_task_id"], "t-bad");
        assert!(
            ev.payload["reason"]
                .as_str()
                .unwrap_or("")
                .contains("读取联动帧失败"),
            "reason: {}",
            ev.payload["reason"]
        );
        // payload 缺 frame_ref 同样走失败通道（协程不 panic）
        bus.publish(Event::new(
            "screenshot.ocr_requested",
            "screenshot",
            json!({ "task_id": "t-noref" }),
        ))
        .unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(5), failed.recv())
            .await
            .expect("缺 frame_ref 应收到 ocr.failed")
            .unwrap();
        assert_eq!(ev.payload["source_task_id"], "t-noref");
    }

    /// 直接 IPC 路径不变：recognize 出结果并广播 ocr.completed（携 source_task_id）
    #[tokio::test(flavor = "current_thread")]
    async fn direct_recognize_publishes_completed() {
        let bus = Arc::new(EventBus::new());
        let mut done = bus.subscribe("ocr.completed").unwrap();
        let (registry, _) = test_registry();
        let m = OcrModule::new();
        *m.engines.write() = Some(registry);
        *m.bus.write() = Some(bus);
        let req = OcrRequest {
            image_b64: host_core::util::b64_encode(&png_bytes()),
            langs: vec![],
            source_task_id: Some("t-direct".into()),
        };
        let result = m.recognize(&req).unwrap();
        assert_eq!(result.engine, "mock");
        assert_eq!(result.text, "你好");
        let ev = done.recv().await.unwrap();
        assert_eq!(ev.payload["source_task_id"], "t-direct");
    }

    /// 用户可配置优先级（DESIGN §4.3）经模块 config 通道落注册表
    /// （T-B4-11 后注册表随设置重建，故断言读穿 `m.registry()` 而非旧句柄）
    #[test]
    fn apply_config_sets_preferred_and_rejects_unknown() {
        let m = OcrModule::new();
        let dir = tempfile::tempdir().unwrap();
        let (registry, _) = test_registry();
        *m.engines.write() = Some(registry);
        *m.app_data.write() = Some(dir.path().to_path_buf());
        let preferred = || m.registry().unwrap().preferred();
        let err = m
            .apply_config(json!({"preferred_engine": "nope"}))
            .unwrap_err();
        assert!(err.to_string().contains("未知引擎"), "{err}");
        assert_eq!(preferred(), "mock", "拒绝后保持原优先级");
        m.apply_config(json!({"preferred_engine": "mock"})).unwrap();
        assert_eq!(preferred(), "mock");
        // 缺省字段 = 不动优先级
        m.apply_config(json!({})).unwrap();
        assert_eq!(preferred(), "mock");
    }

    struct NamedEngine {
        id: &'static str,
    }
    impl OcrEngine for NamedEngine {
        fn id(&self) -> &'static str {
            self.id
        }
        fn display_name(&self) -> &'static str {
            self.id
        }
        fn available(&self) -> Result<Vec<String>, AppError> {
            Ok(vec!["zh-CN".into()])
        }
        fn recognize(&self, _frame: &Frame, _lang: &str) -> Result<Vec<OcrLine>, AppError> {
            Ok(vec![])
        }
    }

    /// T-B4-10 红线：设置中心可选引擎的词表来自注册表实际 id，不是手写字面表
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-10）字面测试名优先于 rustc 命名惯例
    fn configSchema_engineEnumFollowsRegistryNotHardcoded() {
        let m = OcrModule::new();
        assert!(
            m.config_schema()["properties"]["preferred_engine"]
                .get("enum")
                .is_none(),
            "init 前注册表不存在：宁可不给 enum（设置中心退化为自由文本），也不给一份手写假词表"
        );
        *m.engines.write() = Some(Arc::new(EngineRegistry::new(vec![
            Arc::new(NamedEngine { id: "alpha" }),
            Arc::new(NamedEngine { id: "beta" }),
        ])));
        let schema = m.config_schema();
        let ids: Vec<&str> = schema["properties"]["preferred_engine"]["enum"]
            .as_array()
            .expect("注册表已建，enum 应在")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["alpha", "beta"], "enum 恰等于注册表 id 列表");
        assert_eq!(schema["properties"]["preferred_engine"]["default"], "alpha");
        assert!(
            !schema.to_string().contains("win-ocr"),
            "注册表里没有 win-ocr 时 schema 也不得出现它：{schema}"
        );
        // langs 必须落在 SchemaForm 四形内（09 §8.1-⑪ 设置面约束）
        assert_eq!(schema["properties"]["langs"]["type"], "array");
        assert_eq!(schema["properties"]["langs"]["items"]["type"], "string");
    }

    /// T-B4-10：langs 经配置通道落运行态；坏引擎值仍按既有 003 拒且不留半写状态
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-10）字面测试名优先于 rustc 命名惯例
    fn applyConfig_langsPersistAndUnknownEngineStillRejects() {
        let m = OcrModule::new();
        let dir = tempfile::tempdir().unwrap();
        let (registry, _) = test_registry();
        *m.engines.write() = Some(registry);
        *m.app_data.write() = Some(dir.path().to_path_buf());
        let config_langs = || m.registry().unwrap().config_langs();
        m.apply_config(json!({"langs": ["zh-CN", "en-US"], "preferred_engine": "mock"}))
            .unwrap();
        assert_eq!(
            config_langs(),
            vec!["zh-CN".to_string(), "en-US".to_string()]
        );
        assert_eq!(m.config().langs, config_langs(), "读口即运行态本身");
        assert_eq!(m.config().preferred_engine, "mock");
        let err = m
            .apply_config(json!({"langs": ["fr-FR"], "preferred_engine": "nope"}))
            .unwrap_err();
        assert!(err.to_string().contains("未知引擎"), "{err}");
        assert_eq!(
            config_langs(),
            vec!["zh-CN".to_string(), "en-US".to_string()],
            "整次派发被拒 ⇒ 同批的 langs 也不得半写"
        );
        m.apply_config(json!({})).unwrap();
        assert_eq!(
            config_langs(),
            vec!["zh-CN".to_string(), "en-US".to_string()],
            "缺键 = 不动运行态（与 preferred_engine 同纪律）"
        );
    }
}
