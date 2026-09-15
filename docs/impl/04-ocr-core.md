# 04 ocr-core OCR 与翻译细化（代码级）

> 依赖：S1–S7；与 screenshot-core 通过事件总线联动 ｜ crate：`crates/ocr-core/`

## 实现步骤总览

| 步骤 | 内容 | 依赖 |
|------|------|------|
| O1 | 引擎抽象（OcrEngine trait + 注册表） | S3 |
| O2 | Windows.Media.Ocr 引擎实现 | O1 |
| O3 | PaddleOCR Sidecar 引擎实现 | O1 |
| O4 | 识别管线（预处理→识别→后处理） | O1–O3 |
| O5 | 触发集成（快捷键/截图联动/划词） | O4, S6 |
| O6 | 翻译引擎抽象 | O4 |
| O7 | 结果覆盖层 + IPC | O4 |
| O8 | 引擎可用性检测与降级 | O1–O3 |

---

## O1 引擎抽象

```rust
#[async_trait]
pub trait OcrEngine: Send + Sync {
    fn id(&self) -> &'static str;                       // "win-ocr" | "paddle"
    fn display_name(&self) -> &'static str;
    async fn available(&self) -> Result<Vec<String>, AppError>;   // 支持的语言 BCP-47
    async fn recognize(&self, input: &OcrInput, lang: &str) -> Result<Vec<OcrLine>, AppError>;
}
pub struct OcrInput { pub frame: Frame, pub hint_langs: Vec<String> }
pub struct OcrLine { pub text: String, pub rect: Rect<f32> /* 归一化 0..1 */, pub confidence: f32 }

pub struct EngineRegistry { engines: Vec<Arc<dyn OcrEngine>>, preferred: RwLock<String> }
impl EngineRegistry {
    pub fn pick(&self, lang: &str) -> Result<Arc<dyn OcrEngine>, AppError>;
    // 算法：preferred 引擎 available() 且含 lang → 用之；否则按注册顺序找首个支持 lang 的；
    // 全部不可用 → AppError::Module{ code:"OCR_ENGINE_001", hint:"在 Windows 设置中安装对应语言包，或下载 PaddleOCR 引擎" }
}
```

## O2 Windows.Media.Ocr 实现

```
位置：win-integration（唯一允许 windows crate 的层），ocr-core 经 Port/Engine 调用
流程：BGRA bytes → SoftwareBitmap(Bgra8) → OcrEngine::TryCreateFromLanguage(lang)
      → engine.RecognizeAsync → 按 Line 聚合 Word 坐标 → OcrLine 列表
```

### 潜在问题
1. `TryCreateFromLanguage` 需 `Globalization` feature；语言包缺失返回 None → 归一为 OCR_ENGINE_001。
2. `RecognizeAsync` 是 WinRT 异步：用 `Windows.Foundation` 的 IAsyncOperation → `tokio::oneshot` 桥接，**不要** `.get()` 阻塞（STA 死锁风险）。
3. 单次识别建议图像 ≤ 4096px，超限先缩放（P4 预处理）。
4. 该引擎无 confidence 输出：统一填 1.0，置信度排序逻辑不依赖它。

## O3 PaddleOCR Sidecar 实现

```rust
pub struct PaddleEngine { sidecar: SidecarManager }   // 复用 host 侧 sidecar 下载/校验/守护
// 通信协议（本地 HTTP，127.0.0.1 随机端口 + token）：
// POST /ocr  {image: base64(png), lang: "ch|en"}  → {lines: [{text, rect:[x,y,w,h], score}]}
// 生命周期：首次使用下载（官方 Release，SHA256 校验）→ 常驻子进程 → 健康检查 GET /health 每 30s → 崩溃自动重启（≤3 次，超出报 OCR_ENGINE_002）
```

### 潜在问题
- 模型文件约 10–20MB 与二进制一起放 `{appData}/bin/paddleocr/`；磁盘不足提前检查并报可操作错误。
- HTTP token 防本机其它进程调用；绑定 127.0.0.1。

## O4 识别管线

```
输入 OcrInput
 ① 预处理：宽或高 > 3000px → 等比缩到 3000（防引擎超时）；灰度化可选（paddle 提升小字号召回）
 ② 引擎识别（O1 pick）
 ③ 行排序重建：按 rect.y 聚类成段（阈值 = 行高中位数 × 0.6）→ 段内按 x 排序 → 段间按 y 排序
 ④ 文本合并：同段行间——中文直接拼接；西文按尾字符（无连字符加空格）；修正常见换行断词 "-"
 ⑤ 输出 OcrResult { lines, text_joined, lang }
```

```rust
pub struct OcrPipeline { engines: Arc<EngineRegistry>, pre: Preprocessor }
impl OcrPipeline {
    pub async fn run(&self, input: OcrInput) -> Result<OcrResult, AppError>;   // 带超时 30s
}
```

### 潜在问题
- 超时 30s 到点必须取消（`tokio::select!` + 引擎侧取消支持）；Paddle 取消 = 断开本次 HTTP。
- 行高全部接近时（纯列表）段聚类会失败——保底：全部行视为一段按 y 排序。

## O5 触发集成

| 触发方式 | 路径 |
|----------|------|
| 全局快捷键（默认 `Ctrl+Alt+O`） | 截屏（复用 screenshot 的 CaptureService）→ 管线 → 覆盖层 |
| 截图模块联动 | 订阅 `screenshot.ocr_requested` → 管线 → 发 `ocr.completed`（截图 UI 显示结果） |
| 划词 | `SendInput` 模拟 Ctrl+C → 订阅 `clipboard.captured`（80ms 窗口）→ 文本翻译/OCR 判定 |

**联动规约**：ocr-core 与 screenshot-core 之间只允许上述事件交互（DESIGN O1），禁止直接函数调用。

## O6 翻译引擎

```rust
#[async_trait]
pub trait TranslateEngine: Send + Sync {
    fn id(&self) -> &'static str;
    async fn translate(&self, text: &str, from: &str, to: &str) -> Result<String, AppError>;
}
// v1 内置：仅"离线词库"占位 + 插件位；网络引擎（如可配置的自建 API）阶段四再接
// 语义：翻译失败不影响 OCR 结果展示 —— 结果面板分两栏，翻译栏独立错误态
```

## O7 结果覆盖层 + IPC

```rust
// 覆盖层：跟随选区的置顶小窗，结果文本可选中复制；"复制全部"按钮写剪贴板（经 ClipboardPort，标记回写窗口）
#[tauri::command] ocr_recognize(input: OcrInputDto) -> Result<OcrResultDto, AppError>
#[tauri::command] ocr_translate(text: String, to: String) -> Result<String, AppError>
#[tauri::command] ocr_engine_status() -> Result<Vec<EngineStatus>, AppError>   // 可用性/默认引擎设置
// 事件：ocr.completed {source_task_id?, result} | ocr.failed {reason}
```

## O8 可用性检测与降级

- 模块 start 时异步探测：win-ocr 语言列表 +（若配置）paddle health；结果缓存并随 `ocr_engine_status` 暴露。
- 用户默认引擎不可用 → 自动用可用引擎并在结果条提示"已切换到 {引擎}"，同时写日志。

## 验收（O 出口）

- [ ] 快捷键截屏 → 结果覆盖层 P95 < 1.5s（win-ocr 引擎，1080p 区域）
- [ ] 中文/英文混排行序正确（多列布局按段聚类）
- [ ] 未安装语言包 → 明确报错并可一键跳转系统语言设置
- [ ] Paddle 引擎崩溃 → 自动重启 ≤ 3 次，超出后 UI 呈现可操作错误
- [ ] 识别结果"复制全部"不触发剪贴板重复记录（回写窗口生效）
