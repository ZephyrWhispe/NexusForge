//! Port trait 定义（docs/impl/01 S3，依据 docs/DESIGN.md §6.4 与优化点 O2）
//!
//! 规则：
//! - 模块 crate 只允许依赖本文件定义的 Port trait，**禁止直接依赖 windows crate**；
//! - Windows 实现集中在 win-integration crate，通过 [`Ports`] 注册；
//! - 全部方法为同步签名：阻塞调用由调用方（模块）放入 spawn_blocking。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    pub kill: Box<dyn FnOnce() + Send>,
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

    /// 拿走关闭闭包但不执行（供会话表延迟调用：用户 kill / reader EOF / 模块 stop）
    pub fn into_kill(self) -> Box<dyn FnOnce() + Send> {
        self.kill
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

/// 系统 Shell 缩略图（win-integration：IShellItemImageFactory；file-core F4 视频等预览）
pub trait ThumbPort: Port {
    /// 返回 (width, height, PNG 字节)；无缩略图资源时返回错误
    fn thumbnail(&self, path: &Path, px: u32) -> Result<(u32, u32, Vec<u8>), AppError>;
}

/// 回收站删除（win-integration：SHFileOperationW + FOF_ALLOWUNDO；file-core F2）
pub trait RecycleBinPort: Port {
    /// 整批移入回收站；返回成功条数
    fn delete(&self, paths: &[PathBuf]) -> Result<u32, AppError>;
}

/// ConPTY 伪终端（阶段三 term-core 使用）
pub trait ConptyPort: Port {
    fn spawn(&self, cfg: TermCfg) -> Result<PtyHandle, AppError>;
}

/// 系统代理当前值（HKCU `...\Internet Settings` 投影，docs/impl/05 PR4）
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SysProxyState {
    pub enable: bool,
    /// "127.0.0.1:7890" 或 "http=...;https=..." 形式
    pub server: String,
    /// 分号分隔的例外列表（`<local>` 表示本地主机名直连）
    pub bypass: String,
}

/// 系统代理设置（win-integration/sysproxy.rs：注册表读写 + WinINET 广播；proxy-core PR4 使用）
pub trait SysProxyPort: Port {
    fn read(&self) -> Result<SysProxyState, AppError>;
    /// 写 ProxyEnable/ProxyServer/ProxyOverride（不广播；写后必须调 [`refresh`](Self::refresh)）
    fn write(&self, state: &SysProxyState) -> Result<(), AppError>;
    /// InternetSetOption 广播立即生效
    fn refresh(&self) -> Result<(), AppError>;
    /// 当前进程是否以管理员运行（PR5 TUN 前置条件）
    fn is_admin(&self) -> bool;
}

/// Shell 启动（win-integration/shell.rs：ShellExecuteW；desktop-core D1 启动器使用）
pub trait ShellPort: Port {
    /// 以默认方式打开路径（exe/lnk/文档）；返回错误码 <= 32 视为失败
    fn shell_execute(&self, path: &str) -> Result<(), AppError>;
}

/// Windows Task Scheduler（automation-core A4，docs/impl/07）：schtasks 封装
/// 注册任务以当前用户运行（v1 不用 /RL HIGHEST——需要最高权限的任务才标 UAC 盾）
pub trait TaskSchdPort: Port {
    /// 注册每日任务（存在则 /F 覆盖）；time 格式 HH:MM
    fn ensure_daily(&self, task_name: &str, exe: &str, args: &str, time: &str) -> Result<(), AppError>;
    /// 删除任务（不存在视为成功——幂等）
    fn remove(&self, task_name: &str) -> Result<(), AppError>;
    /// 列出本应用注册的任务名（按前缀过滤）
    fn list(&self) -> Result<Vec<String>, AppError>;
}

/// 注册表窄操作（docs/impl/08 WinOps W0：BAVR 引擎数据面）。
/// 路由契约：HKCU 由本进程实现直写；HKLM 需提权 Helper（未实现前 catalog 全 HKCU）。
pub trait RegistryOps: Port {
    /// 读值（existed=false 表示值不存在——备份语义区分"原值不存在"与"原值为空"）
    fn read_value(&self, key: &str, value_name: &str) -> Result<(RegValue, bool), AppError>;
    /// 写值（key 不存在时自动创建）
    fn write_value(&self, key: &str, value_name: &str, value: &RegValue) -> Result<(), AppError>;
    /// 删除值（restore 用：原值不存在则恢复为"不存在"）
    fn delete_value(&self, key: &str, value_name: &str) -> Result<(), AppError>;
}

/// 注册表值数据（BAVR 备份/恢复的载荷；snake_case 外部标签：{"dword": 0} / {"str": "x"}）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegValue {
    Dword(u32),
    Qword(u64),
    Str(String),
}

// ---------------------------------------------------------------------------
// WinOps W2 扩展数据面（docs/impl/08）：计划任务启停 + 服务控制
// ---------------------------------------------------------------------------

/// 计划任务启停（WinOps Task 动作；复用 schtasks 命令行，与 TaskSchdPort 同层）
pub trait TaskTogglePort: Port {
    /// 查询任务启用状态：None = 任务不存在（备份区分"不存在"与"禁用"）
    fn query_enabled(&self, path: &str) -> Result<Option<bool>, AppError>;
    /// 设置启用/禁用（任务不存在时报错）
    fn set_enabled(&self, path: &str, enabled: bool) -> Result<(), AppError>;
}

/// 服务启动类型（WinOps Service 动作；修改需管理员——非提权进程返回 ACCESS_DENIED）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartType {
    Auto,
    Manual,
    Disabled,
}

/// 服务查询快照（BAVR 备份/verify 数据面）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub start_type: StartType,
    pub running: bool,
}

/// 服务控制（sc.exe 封装；启动类型三态 + 启停，修改需管理员）
pub trait ServiceCtlPort: Port {
    /// 查询服务快照（启动类型 + 运行状态）
    fn query(&self, name: &str) -> Result<ServiceInfo, AppError>;
    /// 设置启动类型
    fn set_start_type(&self, name: &str, start_type: StartType) -> Result<(), AppError>;
    /// 停止服务（已停止 → 幂等成功）
    fn stop(&self, name: &str) -> Result<(), AppError>;
    /// 启动服务（Disabled 服务 → 跳过返回 Ok——clear_cache 幂等语义）
    fn start(&self, name: &str) -> Result<(), AppError>;
}

/// 系统维护动作（docs/impl/08 W6；还原点/DISM/SFC 的安全等级归 MaintenancePort）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairKind {
    DismScanHealth,
    DismRestoreHealth,
    DismComponentCleanup,
    SfcScanNow,
}

/// 系统维护（docs/impl/08 W4–W6：clean_dir/Exec 白名单/还原点/内存清理/DISM-SFC）
pub trait MaintenancePort: Port {
    /// 清空目录内容（保留目录本身）；skip_recent_hours 内的新文件跳过；返回删除条目数
    fn clean_dir(&self, path: &str, recursive: bool, skip_recent_hours: u32) -> Result<u32, AppError>;
    /// 执行白名单程序（powercfg|dism|sfc|netsh|onedrive_uninstall；args 参数模板拼接，
    /// 禁止透传任意字符串）；返回合并输出（截断到 256KB）；超时强杀
    fn exec(&self, program: &str, args: &[String], timeout_ms: u32) -> Result<String, AppError>;
    /// 创建系统还原点（需系统还原已开启 + 管理员；MODIFY_SETTINGS 类型）
    fn restore_point(&self, description: &str) -> Result<(), AppError>;
    /// 清理各进程工作集（EmptyWorkingSet；跳过自身/系统进程；返回成功处理数）
    fn empty_working_set(&self) -> Result<u32, AppError>;
    /// 系统修复（DISM/SFC；同步捕获输出，长超时）
    fn repair(&self, kind: RepairKind) -> Result<String, AppError>;
    /// Defender 实时保护开关（高风险；固定 PowerShell 模板——篡改保护拦截时如实报错）
    fn defender_realtime(&self, disable: bool) -> Result<(), AppError>;
}

/// Appx 包快照（docs/impl/08 W5）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppxPackage {
    /// 包名（Id.Name，不含版本/arch，如 "Microsoft.BingWeather"）
    pub name: String,
    /// 全名（含版本/arch/publisher hash，移除时使用）
    pub full_name: String,
}

/// Appx 包管理（docs/impl/08 W5；WinRT PackageManager + provisioned 走 DISM/PowerShell）
pub trait AppxPort: Port {
    /// 列出当前用户已安装且包名前缀匹配的包（name_filter 空 = 全部）
    fn list(&self, name_filter: &str) -> Result<Vec<AppxPackage>, AppError>;
    /// 移除当前用户的匹配包（不需管理员；全部匹配项逐一移除，返回移除数）
    fn remove_current_user(&self, name_filter: &str) -> Result<u32, AppError>;
    /// 移除 provisioned 包（防新建用户/系统重置后再装；需管理员；PowerShell 模板）
    fn remove_provisioned(&self, name_filter: &str) -> Result<u32, AppError>;
}

// ---------------------------------------------------------------------------
// 提权 Helper（docs/impl/08 §4 W3）：HKLM/服务/系统任务经提权进程执行
// ---------------------------------------------------------------------------

/// 提权 Helper 启动规格（§4.1：管道名 + token + 父进程三元组）
#[derive(Clone, Debug)]
pub struct HelperSpec {
    /// 命名管道全名（`\\.\pipe\nexusforge-sys-helper-v1-{parent_pid}`）
    pub pipe_name: String,
    /// 握手 token（64 hex；经环境变量 NF_HELPER_TOKEN 注入，spawn 后立即清除）
    pub token: String,
    /// 主进程 PID（helper 派生管道名 + 校验调用方镜像路径）
    pub parent_pid: u32,
    /// helper exe 全路径（必须与主 exe 同目录——bundle 纪律）
    pub helper_exe: PathBuf,
    /// 空闲自退秒数（无请求超时退出）
    pub idle_exit_secs: u32,
}

/// 提权 Helper 拉起（win-integration：ShellExecuteExW runas → UAC 弹窗）
pub trait HelperSpawnPort: Port {
    /// 提权拉起 helper；返回 helper PID（0 = 系统未回传句柄，握手重试兜底）
    fn spawn(&self, spec: &HelperSpec) -> Result<u32, AppError>;
}

/// Docker Engine HTTP over named pipe 响应（docs/impl/06 T6）
#[derive(Clone, Debug)]
pub struct HttpResp {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Docker Engine named pipe 客户端（win-integration/docker.rs：`\\.\pipe\docker_engine`）
pub trait DockerPipePort: Port {
    /// 发 HTTP/1.1 请求（Connection: close 短连接；body 为 None 即 GET）
    fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<HttpResp, AppError>;
}

/// 磁盘空间（sys-core SY4）
#[derive(Clone, Debug, Serialize)]
pub struct DiskSpace {
    /// 盘符如 "C:\\"
    pub mount: String,
    pub free: u64,
    pub total: u64,
}

/// 性能采样（win-integration/perf.rs：PDH + GlobalMemoryStatusEx，SY4；实现内部 1s 采样线程缓存最新值）
pub trait PerfPort: Port {
    /// CPU 使用率 0-100
    fn cpu_percent(&self) -> Result<f64, AppError>;
    /// (已用, 总量) 字节
    fn mem_bytes(&self) -> Result<(u64, u64), AppError>;
    /// 各盘空间
    fn disk_spaces(&self) -> Result<Vec<DiskSpace>, AppError>;
    /// 网络总吞吐（字节/秒，全部接口求和）
    fn net_bps(&self) -> Result<f64, AppError>;
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
