# 03 screenshot-core 截图与录屏细化（代码级）

> 依赖：S1–S7 ｜ crate：`crates/screenshot-core/` ｜ Windows 能力：Windows.Graphics.Capture（主）、GDI PrintWindow（窗口回退）

## 实现步骤总览

| 步骤 | 内容 | 依赖 |
|------|------|------|
| P1 | 类型与配置（CaptureTarget/Frame/Tool/ShotTask） | S2 |
| P2 | 捕获服务（CapturePort 集成、全屏/区域/窗口） | P1, S3 |
| P3 | 选区覆盖层（全屏透明窗口 + 拖拽选区） | P2 |
| P4 | 标注状态机（工具/撤销栈/渲染） | P3 |
| P5 | 任务流水线（后处理动作注册表） | P4 |
| P6 | 贴图置顶窗口 | P4 |
| P7 | 录屏（Graphics.Capture 视频流 + FFmpeg 编码） | P2 |
| P8 | IPC + 事件 + 前端组件 | P5 |

---

## P1 类型与配置

```rust
pub enum CaptureTarget { FullScreen { monitor: u32 }, Region { monitor: u32, rect: Rect<i32> }, Window { hwnd: isize } }
pub struct Frame { pub width: u32, pub height: u32, pub bgra: Arc<[u8]>, pub dpi_scale: f32, pub monitor_id: u32 }

pub enum Tool { Pen { color: Color, width: f32 }, Rect, Ellipse, Arrow, Text { color: Color }, Mosaic, Number { next: u32 } }
pub struct Annotation { pub tool: Tool, pub points: Vec<(f32, f32)>, pub text: Option<String> }  // 坐标为归一化 0..1

pub struct ScreenshotConfig {
    pub default_action: Vec<PostActionKind> = vec![SaveTo, CopyToClipboard],
    pub save_dir: PathBuf,                    // 默认 {Pictures}/NexusForge
    pub format: ImageFormat = Png,
    pub filename_template: String = "shot_{yyyy-MM-dd_HHmmss}",
    pub include_cursor: bool = false,
    pub show_toolbar_after_select: bool = true,
}
```

## P2 捕获服务

```rust
pub struct CaptureService { port: Arc<dyn CapturePort> }
impl CaptureService {
    pub async fn grab(&self, target: CaptureTarget) -> Result<Frame, AppError>;  // 内部 spawn_blocking
    pub fn list_monitors(&self) -> Result<Vec<MonitorInfo>, AppError>;
}
```

**细节**：CapturePort 实现放 win-integration（`Windows.Graphics.Capture` via `windows` crate Graphics_Capture feature）；`capture` 返回前必须 `CopyResource` 到 CPU 可读 staging texture 再 map，返回 `Arc<[u8]>`。窗口捕获回退：`PrintWindow`（flag=`PW_RENDERFULLCONTENT`）。

### 潜在问题
1. **DPI**：物理像素 ↔ 逻辑像素全链路只传物理值 + `dpi_scale`，前端换算用 `scale = devicePixelRatio`；坐标错误是本模块第一高发 bug。
2. **多显示器负坐标**：虚拟桌面原点在主屏左上，副屏可为负 —— Rect 一律用 i32 物理坐标。
3. **受保护窗口**（DRM）捕获返回黑帧：检测全黑（采样 16 点）时返回 `AppError::Permission{ code:"SCREENSHOT_CAPTURE_002", hint:"目标窗口受系统保护" }`。

## P3 选区覆盖层

```rust
// 独立 Tauri 窗口（每显示器一个）：transparent=true, fullscreen 虚拟桌面区域, always_on_top, skip_taskbar
pub struct SelectionOverlay { windows: Vec<WebviewWindow>, frame: Arc<Frame> }
// 流程：grab 全屏帧 → 按 monitor 切片 → 每屏创建覆盖窗口 → 前端把帧作背景图（放大镜 + 暗化遮罩）
// 前端拖拽：mousedown 记 anchor → mousemove 更新选区矩形（emit selection.preview 节流 16ms）
//          → mouseup 确定 → emit selection.confirmed {rect(物理像素)}
// Esc=取消；Enter/双击=确认；Tab=切换窗口捕获模式
```

### 潜在问题
- 覆盖窗口必须覆盖整屏且鼠标穿透关闭；`fullscreen:true` 在多屏会用主屏分辨率——改用显式 `set_position/set_size` 到虚拟桌面坐标。
- 前端背景帧用 `data:` URL 内存约 = 像素×4×1.33，4K 屏约 33MB，可接受；但禁止同时保留多屏原图，确认后立即释放。
- 呼出延迟 < 200ms：预热——模块 start 时预建隐藏覆盖窗口（`visible:false`），呼出只改可见性。

## P4 标注状态机

```rust
pub struct AnnotationSession {
    base: Arc<Frame>,
    annotations: Vec<Annotation>,
    undo_stack: Vec<Op>,        // Op::Add(idx) / Op::Remove(idx) / Op::Edit{idx, before}
    redo_stack: Vec<Op>,
    active_tool: Tool,
}
impl AnnotationSession {
    pub fn begin_stroke(&mut self, pt: (f32,f32));
    pub fn extend_stroke(&mut self, pt: (f32,f32));
    pub fn end_stroke(&mut self) -> usize;          // 返回新 annotation idx，压入 undo
    pub fn undo(&mut self) -> bool;                 // redo_stack 同步
    pub fn render(&self) -> Result<Frame, AppError>; // 离屏合成：base 上绘制全部 annotation
}
```

**渲染实现**：标注合成放**前端 Canvas**（交互实时性），`render()` 最终合成放 **Rust 侧 skia-safe 或 ab_glyph+image crate**（导出一致性）。两者用同一归一化坐标模型。

### 潜在问题
- 文本标注的字体：Rust 侧用 `Segoe UI`（加载系统字体文件），与前端 Canvas 默认字体对齐，字号按 dpi_scale 缩放。
- 马赛克：对 points 包围盒内像素做 8×8 块均值化；实现在 Rust `render()`，前端只显示示意。

## P5 任务流水线（后处理动作）

```rust
#[async_trait]
pub trait PostAction: Send + Sync {
    fn kind(&self) -> PostActionKind;                       // SaveTo|CopyToClipboard|Pin|Upload|Ocr|Custom
    async fn run(&self, input: &ShotOutput) -> Result<PostActionResult, AppError>;
}
pub struct ShotOutput { pub frame: Frame, pub file: Option<PathBuf>, pub task_id: String }
// Pipeline: 顺序执行用户配置的 action 列表；任一失败：记录并继续（SaveTo 失败除外→整体报错）
// Upload 为插件点（阶段四接入）；Ocr 动作转发给 ocr-core（经事件总线 screenshot.ocr_requested，
// 由 ocr-core 订阅处理并把结果发回 clipboard.captured —— 跨模块只走事件，DESIGN O1）
```

### 潜在问题
- 保存文件名冲突：模板后追加 `_1` 递增；文件写入用临时文件+rename（规约 5）。
- 流水线中 `Pin` 动作依赖 P6 窗口，需在 start 后可用；未就绪时降级为复制。

## P6 贴图置顶

```rust
// 每个 Pin 一个无边框置顶 Tauri 窗口：decorations=false, always_on_top, shadow=true
// 内容 = 合成后 PNG；支持滚轮缩放(0.2–5.0)、Alt+滚轮透明度(0.2–1.0)、拖拽移动、双击关闭
// 布局持久化：{appData}/pins.json，重启恢复已存在 pin（文件丢失则丢弃该项）
```

## P7 录屏

- 捕获：`GraphicsCaptureSession` 帧池 → 每帧转 BGRA 写入 spsc 环形缓冲（容量 90 帧）。
- 编码：FFmpeg Sidecar 子进程（`-f rawvideo -pix_fmt bgra -s WxH -r 30 -i - -c:v libx264 -preset veryfast out.mp4`），stdin 管道喂帧。
- GIF：先产 mp4，再二遍调 ffmpeg 生成调色板 GIF。
- 停止：flush 环形缓冲 → 关 stdin → 等待退出码 0（超时 5s kill 并报错）。

### 潜在问题
- FFmpeg 按需下载（DESIGN O5），录制启动前检查存在性，缺失时提示下载（错误码 `SCREENSHOT_RECORD_001`）。
- 4K 30fps 原始帧带宽 ~500MB/s：环形缓冲仅保底 3 秒，编码跟不上即丢帧并在结果 UI 标注"存在丢帧"。

## P8 IPC + 事件

```rust
screenshot_start(target: CaptureTarget) -> Result<TaskId, AppError>   // → 覆盖层流程
screenshot_confirm(rect) / screenshot_cancel()
screenshot_apply_tools(task_id, annotations, tool_state) -> Result<ShotOutput, AppError>
screenshot_record_start(target, opts) / screenshot_record_stop(task_id)
screenshot_pin(output) / screenshot_history_list(page) -> Page<ShotItem>
// 事件：screenshot.taken {task_id, file?} | screenshot.ocr_requested {task_id, frame_ref}
```

## 验收（P 出口）

- [ ] 快捷键 → 选区层出现 < 200ms；100%/150%/200% DPI 下选区坐标与实际一致
- [ ] 副屏（负坐标）截图选区正确
- [ ] 标注 undo/redo 正确；导出 PNG 与预览一致
- [ ] 贴图缩放/透明度/重启恢复可用
- [ ] 录屏 1 分钟 mp4 可播放、停止后无僵尸 ffmpeg 进程
