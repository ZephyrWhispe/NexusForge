//! Port trait 定义（docs/impl/01 S3，依据 docs/DESIGN.md §6.4 与优化点 O2）
//!
//! 规则：
//! - 模块 crate 只允许依赖本文件定义的 Port trait，**禁止直接依赖 windows crate**；
//! - Windows 实现集中在 win-integration crate，通过 [`Ports`] 注册；
//! - 全部方法为同步签名：阻塞调用由调用方（模块）放入 spawn_blocking。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// Port 能力标记 trait。实现方（win-integration / 模块能力）以 `Arc<dyn XxxPort>` 注册。
///
/// 全仓库 blanket 实现：任何 Send+Sync+'static 类型自动满足（Port 无方法，
/// 仅作注册表键约束）。trait 对象（如 `dyn TrayProvider`）因此可直接注册。
pub trait Port: Send + Sync + 'static {}

impl<T: ?Sized + Send + Sync + 'static> Port for T {}

// ---------------------------------------------------------------------------
// 跨层数据传输对象（由 host-core 统一定义，模块与 win-integration 共用）
// ---------------------------------------------------------------------------

/// 归一化矩形（截图选区 / OCR 行框；坐标约定见 docs/impl/03 P2）
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Rect<T> {
    pub x: T,
    pub y: T,
    pub w: T,
    pub h: T,
}

/// 剪贴板内容（docs/impl/02 C1 的 Port 层最小投影）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ClipContent {
    Text {
        text: String,
        html: Option<String>,
    },
    Image {
        /// "png" | "dib"（回写时由 win-integration 决定编码）
        format: String,
        width: u32,
        height: u32,
        bytes: Arc<[u8]>,
    },
    Files {
        paths: Vec<PathBuf>,
    },
}

/// 显示器信息（虚拟桌面坐标系，副屏可为负坐标）
#[derive(Clone, Debug, Serialize)]
pub struct MonitorInfo {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub dpi_scale: f32,
}

/// 捕获目标（docs/impl/03 P1）
#[derive(Clone, Debug)]
pub enum CaptureTarget {
    FullScreen { monitor: u32 },
    Region { monitor: u32, rect: Rect<i32> },
    Window { hwnd: isize },
}

/// 一帧 BGRA 像素（GPU 纹理已拷贝到 CPU 可读内存）
#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Arc<[u8]>,
    pub dpi_scale: f32,
    pub monitor_id: u32,
}

/// OCR 单行结果（归一化 0..1 坐标，docs/impl/04 O1）
#[derive(Clone, Debug, Serialize)]
pub struct OcrLine {
    pub text: String,
    pub rect: Rect<f32>,
    pub confidence: f32,
}

/// USN 搜索命中项
#[derive(Clone, Debug, Serialize)]
pub struct FileHit {
    pub path: PathBuf,
    pub score: f32,
}

/// 终端会话配置（docs/impl/06 T1，阶段三使用）
#[derive(Clone, Debug)]
pub struct TermCfg {
    pub shell: String,
    pub cwd: Option<PathBuf>,
    pub cols: u16,
    pub rows: u16,
    pub env: HashMap<String, String>,
}

/// ConPTY 会话句柄：Drop 必须回收全部系统句柄（docs/impl/06 风险标注）
pub struct PtyHandle {
    pub input_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    pub output_rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    pub resize_tx: tokio::sync::mpsc::Sender<(u16, u16)>,
    kill: Box<dyn FnOnce() + Send>,
}

impl PtyHandle {
    pub fn new(
        input_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
        output_rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
        resize_tx: tokio::sync::mpsc::Sender<(u16, u16)>,
        kill: Box<dyn FnOnce() + Send>,
    ) -> Self {
        Self { input_tx, output_rx, resize_tx, kill }
    }

    /// 显式终止会话（幂等：Drop 亦会调用）
    pub fn kill(self) {
        (self.kill)();
    }
}

// ---------------------------------------------------------------------------
// 具体端口
// ---------------------------------------------------------------------------

/// 剪贴板（win-integration：AddClipboardFormatListener + 消息循环线程）
pub trait ClipboardPort: Port {
    /// 启动监听；变更时回调 (内容, 来源应用进程名)。回调在专用 OS 消息循环线程触发。
    fn start_listener(&self, cb: Box<dyn Fn(ClipContent, Option<String>) + Send + Sync>)
        -> Result<(), AppError>;
    /// 写回系统剪贴板；调用方须先标记回写窗口（docs/impl/02 C3 ③ 防循环）
    fn write(&self, content: &ClipContent) -> Result<(), AppError>;
}

/// 屏幕捕获（win-integration：Windows.Graphics.Capture，回退 PrintWindow）
pub trait CapturePort: Port {
    fn enumerate_monitors(&self) -> Result<Vec<MonitorInfo>, AppError>;
    /// 阻塞调用：调用方放入 spawn_blocking；受保护窗口返回黑帧检测错误
    fn capture(&self, target: CaptureTarget) -> Result<Frame, AppError>;
}

/// OCR（win-integration：Windows.Media.Ocr 系统引擎）
pub trait OcrPort: Port {
    fn available_languages(&self) -> Result<Vec<String>, AppError>;
    fn recognize(&self, image: &Frame, lang: &str) -> Result<Vec<OcrLine>, AppError>;
}

/// Windows Hello 生物认证（阶段二 vault-core 使用）
pub trait HelloPort: Port {
    fn verify(&self, reason: &str) -> Result<(), AppError>;
}

/// USN/MFT 全盘文件索引（阶段二 file-core 使用）
pub trait UsnIndexPort: Port {
    fn search(&self, query: &str, limit: u32) -> Result<Vec<FileHit>, AppError>;
}

/// ConPTY 伪终端（阶段三 term-core 使用）
pub trait ConptyPort: Port {
    fn spawn(&self, cfg: TermCfg) -> Result<PtyHandle, AppError>;
}

/// RegisterHotKey 底层封装（宿主 HotkeyManager 专用，S6.2 使用）
pub trait HotkeyWinPort: Port {
    fn register(&self, hotkey_id: i32, modifiers: u32, vk: u32) -> Result<(), AppError>;
    fn unregister(&self, hotkey_id: i32) -> Result<(), AppError>;
    /// 设置 OS 事件分发器：WM_HOTKEY 到达时以 hotkey_id 回调（仅需设置一次）
    fn set_dispatcher(&self, dispatcher: Arc<dyn Fn(i32) + Send + Sync>);
}

/// 本机数据加密（DPAPI 封装，剪贴板敏感条目存储用，docs/impl/02 C4）
pub trait CryptoPort: Port {
    fn protect(&self, plaintext: &[u8]) -> Result<Vec<u8>, AppError>;
    fn unprotect(&self, ciphertext: &[u8]) -> Result<Vec<u8>, AppError>;
}

// ---------------------------------------------------------------------------
// 键鼠共享输入口（docs/impl/05 K4/K5；win-integration 实现，kvm-core 经 Port 调用）
// ---------------------------------------------------------------------------

/// 底层输入事件（捕获与注入共用投影；可跨网络序列化）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RawInput {
    KeyDown { vk: u16, scan: u32 },
    KeyUp { vk: u16, scan: u32 },
    /// 虚拟桌面物理像素绝对坐标
    MouseMove { x: i32, y: i32 },
    /// button: 0 左 1 右 2 中
    MouseDown { button: u8, x: i32, y: i32 },
    MouseUp { button: u8, x: i32, y: i32 },
    /// delta: 正=上/右滚
    Wheel { delta: i32, x: i32, y: i32 },
}

/// 低级输入捕获（win-integration：WH_KEYBOARD_LL/WH_MOUSE_LL 专用线程）
pub trait InputHookPort: Port {
    /// 启动捕获；回调在钩子专用线程触发，必须快（<5ms），慢逻辑自行入队。
    /// 返回 false 表示事件被抑制（接管模式下不交还 OS，docs/impl/05 K4）。
    fn start_capture(&self, cb: Box<dyn Fn(&RawInput) -> bool + Send + Sync>)
        -> Result<(), AppError>;
    /// 停止捕获并卸载钩子
    fn stop_capture(&self) -> Result<(), AppError>;
}

/// 输入注入（win-integration：SendInput，绝对坐标按虚拟桌面归一化 0..65535）
pub trait InputInjectPort: Port {
    fn inject(&self, events: &[RawInput]) -> Result<(), AppError>;
}

/// 虚拟桌面矩形（屏幕原点 + 尺寸，副屏可为负坐标；K7 边缘切换与归一化共用）
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// 虚拟桌面信息（win-integration：GetSystemMetrics SM_X/YVIRTUALSCREEN 等）
pub trait ScreenInfoPort: Port {
    fn virtual_desktop(&self) -> Result<ScreenRect, AppError>;
}

// ---------------------------------------------------------------------------
// Port 注册表
// ---------------------------------------------------------------------------

struct PortBox<T: ?Sized>(Arc<T>);

/// Port 注册表：win-integration 在应用启动时注册实现，模块经 [`crate::module::ModuleContext`] 查询。
#[derive(Default)]
pub struct Ports {
    inner: RwLock<HashMap<std::any::TypeId, Arc<dyn std::any::Any + Send + Sync>>>,
    /// 多实例注册（如多个模块同时实现 TrayProvider / HotkeyProvider 能力）
    multi: RwLock<HashMap<std::any::TypeId, Vec<Arc<dyn std::any::Any + Send + Sync>>>>,
}

impl Ports {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个 Port 实现（同类型重复注册以最后者为准）
    pub fn register<T: ?Sized + Port>(&self, impl_: Arc<T>) {
        self.inner
            .write()
            .expect("Ports 注册表写锁")
            .insert(std::any::TypeId::of::<T>(), Arc::new(PortBox(impl_)) as _);
    }

    /// 按 trait 查询实现，如 `ctx.ports.get::<dyn ClipboardPort>()`
    pub fn get<T: ?Sized + Port>(&self) -> Option<Arc<T>> {
        self.inner
            .read()
            .expect("Ports 注册表读锁")
            .get(&std::any::TypeId::of::<T>())
            .and_then(|any| any.clone().downcast::<PortBox<T>>().ok())
            .map(|boxed| boxed.0.clone())
    }

    /// 追加注册（同类型可多个实例）；get_all 依注册顺序取回
    pub fn register_multi<T: ?Sized + Port>(&self, impl_: Arc<T>) {
        self.multi
            .write()
            .expect("Ports 多实例注册表写锁")
            .entry(std::any::TypeId::of::<T>())
            .or_default()
            .push(Arc::new(PortBox(impl_)) as _);
    }

    /// 取回全部多实例实现（注册顺序）
    pub fn get_all<T: ?Sized + Port>(&self) -> Vec<Arc<T>> {
        self.multi
            .read()
            .expect("Ports 多实例注册表读锁")
            .get(&std::any::TypeId::of::<T>())
            .map(|v| {
                v.iter()
                    .filter_map(|any| any.clone().downcast::<PortBox<T>>().ok())
                    .map(|boxed| boxed.0.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct FakeClipboard {
        writes: AtomicU32,
    }
    impl ClipboardPort for FakeClipboard {
        fn start_listener(
            &self,
            _cb: Box<dyn Fn(ClipContent, Option<String>) + Send + Sync>,
        ) -> Result<(), AppError> {
            Ok(())
        }
        fn write(&self, _content: &ClipContent) -> Result<(), AppError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn port_register_and_downcast() {
        let ports = Ports::new();
        assert!(ports.get::<dyn ClipboardPort>().is_none());

        let fake = Arc::new(FakeClipboard { writes: AtomicU32::new(0) });
        ports.register::<dyn ClipboardPort>(fake.clone());

        let got = ports.get::<dyn ClipboardPort>().expect("应能取回注册的实现");
        got.write(&ClipContent::Text { text: "hi".into(), html: None })
            .unwrap();
        got.write(&ClipContent::Text { text: "hi2".into(), html: None })
            .unwrap();
        assert_eq!(fake.writes.load(Ordering::SeqCst), 2);

        // 未注册的 Port 返回 None
        assert!(ports.get::<dyn HelloPort>().is_none());
    }
}
