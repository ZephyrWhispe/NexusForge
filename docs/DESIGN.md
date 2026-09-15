# NexusForge 软件设计方案

> 版本：v1.0 ｜ 日期：2026-09-15 ｜ 状态：**执行依据（开发、测试、部署均须遵循本方案）**
> 来源：基于《集合软件开源蓝本》（DeepSeek 调研对话，17,000+ 行）系统性分析、拆解与优化收敛。
> 约束：仅 Windows 10 1809+ ｜ 非商业 ｜ GPL-3.0 开源 ｜ 本地优先（数据默认本地，同步可选）

---

## 1. 项目定位

一款**深度集成 Windows 原生能力**的集合型可拓展桌面工具：通过统一的模块化架构集成剪切板、代理/VPN、密码库、文件与存储、截图录屏、OCR 翻译、文本文档、笔记知识管理、桌面效率、键鼠共享、终端运维、系统管理、自动化拓展共 13 个模块。每个模块可独立启用/禁用，统一设置、统一快捷键、统一托盘。

设计原则（按优先级排序）：

| 原则 | 说明 |
|------|------|
| Windows 原生优先 | 能用 Windows API 不造轮子（OCR 用 Windows.Media.Ocr、搜索用 USN/MFT、终端用 ConPTY） |
| 模块独立 | 每模块为独立 Rust crate + 独立前端包，可单独启用/禁用/崩溃隔离 |
| 统一宿主 | 模块注册表 + 事件总线 + 配置中心 + 快捷键管理 + 托盘管理集中在 host-core |
| 本地优先 | 不依赖云服务；同步为可选功能且端到端加密 |
| P0 先行 | 先交付核心闭环（宿主 + 剪切板 + 截图 + OCR），再迭代扩展模块 |

---

## 2. 系统架构设计

### 2.1 分层架构

```
┌─────────────────────────────────────────────────────────────────┐
│                前端 UI 层（React 18 + TypeScript + Fluent UI v9）│
│  主工作台 │ 模块窗口 │ 快速面板 │ 桌面挂件 │ 覆盖层 │ 设置中心    │
├─────────────────────────────────────────────────────────────────┤
│                IPC 层（Tauri 2 invoke/emit，按模块命名空间）      │
├─────────────────────────────────────────────────────────────────┤
│                核心宿主层 host-core（Rust）                       │
│  模块注册表 │ 生命周期 │ 事件总线 │ 配置中心 │ 快捷键 │ 托盘       │
│  权限控制 │ 日志 │ 崩溃恢复 │ 自动更新 │ 迁移框架                 │
├─────────────────────────────────────────────────────────────────┤
│                功能模块层（13 个独立 Rust crate）                 │
│  clipboard │ proxy │ vault │ file │ screenshot │ ocr │ editor    │
│  notes │ desktop │ kvm │ term │ sys │ automation                 │
├─────────────────────────────────────────────────────────────────┤
│                Windows 集成层 win-integration（Rust）             │
│  Shell 扩展 │ Windows Hello │ USN/MFT │ ConPTY │ Task Scheduler  │
│  Toast 通知 │ Graphics.Capture │ 注册表代理设置 │ 服务            │
├─────────────────────────────────────────────────────────────────┤
│                外部进程层 Sidecar（按需下载，不打包）              │
│  sing-box │ PaddleOCR │ Rclone │ 7-Zip │ FFmpeg                  │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 架构优化点（相对蓝本的改进）

| # | 优化项 | 蓝本原状 | 本方案决策 |
|---|--------|----------|-----------|
| O1 | 模块间通信 | 存在模块直接互调的倾向 | **禁止模块间直接调用**，所有跨模块交互只经事件总线（§6.2 事件契约），杜绝环形依赖 |
| O2 | Windows API 依赖收敛 | 各模块分散调用 windows crate | 模块只依赖 host-core 定义的 **port trait**（如 `ClipboardPort`、`CapturePort`），Windows 实现集中在 win-integration 层，模块可在测试中注入 mock |
| O3 | 数据库边界 | 单库多表共用 | **每模块独立 SQLite 库文件**（`{appData}/db/{module}.db`），禁止跨模块读表；跨模块数据只走事件或显式 API |
| O4 | 大对象存储 | 图片直接入库倾向 | 大于 64KB 的内容（图片/文件）一律存 blob 目录，主表只存引用与哈希 |
| O5 | Sidecar 分发 | 内核打包进安装包（200MB+） | **按需下载**：首次使用从官方 Release 拉取，校验 SHA256+签名后存 `{appData}/bin/`，内核独立更新通道 |
| O6 | 跨平台代码 | 遗留 macOS/Linux cfg 分支 | 已确定 Windows-only，**删除全部跨平台 cfg 分支**，降低复杂度 |
| O7 | 错误处理 | 各模块散落的 `Result<_, String>` | 统一 `AppError` 体系 + 模块错误码规范（§8.1），IPC 层禁止裸 String 错误 |
| O8 | 高频事件背压 | 剪贴板/监控事件直发 | 事件总线带 **debounce/合并/限流**（如剪贴板 300ms 合并、监控指标 1s 节流），防止 UI 洪泛 |

### 2.3 目录结构

```
NexusForge/
├── src-tauri/                      # Tauri 壳：main.rs、tauri.conf.json、capabilities/
├── crates/
│   ├── host-core/                  # 宿主核心：module.rs registry.rs events.rs config.rs
│   │                               #   lifecycle.rs hotkey.rs tray.rs crash_recovery.rs
│   │                               #   ports.rs（port trait 定义）
│   ├── win-integration/            # Windows API 适配实现
│   ├── clipboard-core/  proxy-core/  vault-core/    # P0/P1 功能模块
│   ├── file-core/  screenshot-core/  ocr-core/
│   ├── editor-core/  notes-core/  desktop-core/
│   ├── kvm-core/  term-core/  sys-core/  automation-core/
├── src/                            # React 前端
│   ├── modules/                    # 各模块 UI（与 crate 一一对应）
│   ├── dock/ layout/ quick-panels/ widgets/ overlays/
│   ├── settings/ stores/ ipc/ components/
├── resources/                      # 图标、tessdata 等静态资源
├── docs/                           # 本方案与开发文档
├── .github/workflows/              # ci.yml / release.yml / nightly.yml
├── Cargo.toml                      # workspace
└── package.json / vite.config.ts
```

---

## 3. 模块划分与优先级

| 优先级 | 模块 | crate | 核心能力 |
|--------|------|-------|----------|
| **P0** | 宿主核心 | host-core | 模块注册/生命周期/事件总线/配置中心/托盘/快捷键/崩溃恢复 |
| **P0** | 剪切板中枢 | clipboard-core | 历史、搜索(FTS5)、智能分组、堆栈粘贴、敏感数据保护 |
| **P0** | 截图与录屏 | screenshot-core | Windows.Graphics.Capture 捕获、选区、标注、贴图置顶 |
| **P0** | OCR 与翻译 | ocr-core | Windows.Media.Ocr 系统引擎 + PaddleOCR Sidecar 双引擎、截图 OCR、划词翻译 |
| **P1** | 代理与 VPN | proxy-core | sing-box Sidecar、规则分流、系统代理/TUN 互斥、崩溃恢复 |
| **P1** | 安全与凭据 | vault-core | Argon2id+AES-256-GCM、TOTP、Windows Hello 解锁、自动锁定 |
| **P1** | 文件与存储 | file-core | 多面板文件管理、USN/MFT 秒搜、StorageDriver 网盘抽象、批量重命名 |
| **P1** | 桌面效率 | desktop-core | 快速启动器、桌面格子、待办随记 |
| **P1** | 键鼠共享 | kvm-core | TCP 输入事件/文件/剪贴板可靠传输 + UDP 发现心跳、屏幕边缘切换 |
| **P2** | 文本与 PDF | editor-core | Monaco 编辑器、Markdown、PDF 处理 |
| **P2** | 笔记与知识 | notes-core | Markdown 笔记库、自由画布、双链、间隔复习 |
| **P2** | 终端与运维 | term-core | ConPTY + xterm.js、SSH/SFTP、端口转发、WSL、Docker |
| **P2** | 系统管理 | sys-core | winget/scoop/choco 抽象、清理优化、资源监控 |
| **P2** | 自动化与拓展 | automation-core | 条件-动作引擎、Task Scheduler、WASM(wasmtime) 插件沙箱、插件市场 |

模块状态机：`Uninitialized → Stopped → Running → Error`（Error 可重启，见 §8.2）。

---

## 4. 核心功能说明（P0 闭环数据流）

### 4.1 剪切板中枢

- **捕获**：`AddClipboardFormatListener` + 隐藏窗口消息循环（win-integration 实现 `ClipboardPort`）
- **分类**：规则链分类器（URL/JSON/代码/密钥），密钥类命中 → 强制加密 + 默认不进历史
- **存储**：`clip_entries` 主表 + FTS5 外部内容索引 + blob 引用；去重按 `content_hash`
- **安全**：敏感数据 AES-256-GCM 加密；"永不记录"应用黑名单；默认保留 30 天；一键安全清空（覆写删除）
- **防循环**：每条目带 `origin` 标记（local/remote），同步模块忽略远端回写条目

```sql
-- clipboard-core 专属库 {appData}/db/clipboard.db
CREATE TABLE clip_entries (
    id TEXT PRIMARY KEY,
    content_type TEXT NOT NULL,        -- text|richtext|image|file|code
    content TEXT,                      -- 文本内容（敏感项加密后 Base64）
    content_hash TEXT NOT NULL,
    blob_path TEXT,                    -- >64KB 内容走 blob
    source_app TEXT,
    origin TEXT NOT NULL DEFAULT 'local',  -- local|remote（防同步循环）
    pinned INTEGER DEFAULT 0,
    group_name TEXT,                   -- url|code|json|secret|...
    created_at INTEGER NOT NULL,
    usage_count INTEGER DEFAULT 0
);
CREATE INDEX idx_entries_created ON clip_entries(created_at DESC);
CREATE INDEX idx_entries_hash ON clip_entries(content_hash);
CREATE VIRTUAL TABLE clip_fts USING fts5(
    content, content='clip_entries', content_rowid='rowid', tokenize='unicode61');
```

### 4.2 截图与录屏

流水线：**捕获（Windows.Graphics.Capture / PrintWindow）→ 选区覆盖层 → 标注 → 任务后处理（保存/复制/贴图/上传插件）**。
- 任务流水线异步化，每步可插拔（参考 ShareX 设计，独立自研实现）
- 贴图置顶 = 无边框置顶窗口 + DWM 材质
- 录屏输出走 FFmpeg Sidecar（按需下载）

### 4.3 OCR 与翻译

管线：**触发（快捷键/截图联动/划词）→ 图像预处理 → 引擎识别 → 后处理（换行合并/段落重排）→ 翻译（可选）→ 结果覆盖层**。
- 引擎抽象 `OcrEngine` trait：默认 Windows.Media.Ocr（系统原生、零依赖），PaddleOCR Sidecar 作为高精度备选，用户可配置优先级
- 引擎不可用时（如系统语言包缺失）自动降级并给出可操作的错误提示

### 4.4 键鼠共享（P1，对应既有 TCP/UDP 设计）

- **发现**：UDP 组播心跳（设备名/指纹/能力），配对用一次性码 + 公钥指纹验证
- **传输**：TCP 可靠通道复用三类流量——输入事件（低延迟小包，可降级采样）、文件（分块 + 校验 + 断点续传）、剪贴板（加密）
- **安全**：会话密钥协商，端到端加密；剪贴板共享复用剪切板中枢的防循环标记

---

## 5. 数据流程设计

### 5.1 全局数据流

```
系统事件源 ──► win-integration(Port 实现) ──► 模块捕获管线 ──► 模块专属 SQLite/blob
                                                    │
                                                    ▼ (归一化事件)
                                              host-core 事件总线 ──► 订阅模块（同步/自动化/UI）
                                                    │
                                                    ▼ (Tauri emit，节流合并)
                                                前端 UI（Zustand store）
```

### 5.2 关键规则

1. **写入顺序**：捕获 → 内存队列 → 批量落库（≤50ms 窗口）→ FTS 索引 → 发事件。崩溃时队列丢弃可接受（剪贴板类）；文件操作类必须"临时文件 + 原子 rename"。
2. **读取**：FTS5 查询 + 游标分页（禁止无 LIMIT 全表查询）；虚拟列表渲染。
3. **blob**：`{appData}/blobs/{module}/{yyyy-mm}/{hash}` ，入库前先写 blob 再写主表，删除时先删主表记录再清理 blob（孤儿 blob 由启动期 GC 清理）。
4. **配置**：`{appData}/config/global.json` + `{appData}/config/{module}.json`，每文件带 `schema_version`，升级自动迁移并先备份；设置 UI 由 `config_schema` 自动生成，不手写表单。
5. **迁移**：数据库用 `rusqlite_migration`，事务执行、失败回滚，保留最近 3 个版本备份。

---

## 6. 接口定义

### 6.1 Module trait（host-core）

```rust
pub trait Module: Send + Sync {
    fn info(&self) -> ModuleInfo;                       // id/name/version/icon
    fn init(&self, ctx: &ModuleContext) -> Result<(), ModuleError>;
    fn start(&self) -> Result<(), ModuleError>;
    fn stop(&self) -> Result<(), ModuleError>;
    fn config_schema(&self) -> ModuleConfig;            // JSON Schema + 当前值
    fn apply_config(&self, values: serde_json::Value) -> Result<(), ModuleError>;
    fn status(&self) -> ModuleState;
}

// 能力 trait：模块按需实现，宿主按能力聚合（侧边栏/托盘/快捷键/Shell 扩展/服务）
pub trait HotkeyProvider: Module { fn global_hotkeys(&self) -> Vec<HotkeyBinding>; }
pub trait TrayProvider: Module   { fn tray_menu_items(&self) -> Vec<TrayMenuItem>; }
pub trait ServiceProvider: Module { /* 安装/卸载 Windows 服务 */ }
```

`ModuleContext` 注入依赖（logger、event_bus、config_store、ports），**测试时注入 mock context，模块 crate 可独立单测**。

### 6.2 事件总线契约（跨模块通信唯一通道）

```rust
pub struct Event {
    pub topic: &'static str,     // "clipboard.captured" / "screenshot.taken" / "ocr.completed"
    pub source: &'static str,    // 模块 id
    pub payload: serde_json::Value,
    pub ts: i64,
}
// 订阅：event_bus.subscribe("clipboard.captured", handler)
// 发布：event_bus.publish(event)  —— 主题必须注册到 TOPIC_REGISTRY，带文档与示例
```

主题命名规范：`{module}.{action past-tense}`。主题注册表集中维护，禁止运行时动态造主题。

### 6.3 IPC 命令规范（Tauri command）

- 命名：`{module}_{action}`（snake_case），按模块命名空间注册到 Tauri
- 错误：统一返回 `Result<T, AppError>`，AppError 序列化为 `{ code, message, hint? }`（禁止裸 String）
- 高频推送用 `emit` + 节流，不用前端轮询

```rust
#[tauri::command]
pub async fn clipboard_search(query: String, group: Option<String>,
    content_type: Option<String>, limit: Option<u32>,
    state: tauri::State<'_, ModuleRegistry>) -> Result<Vec<ClipEntry>, AppError> { /* … */ }

#[tauri::command] pub async fn clipboard_paste(id: String) -> Result<(), AppError>;
#[tauri::command] pub async fn clipboard_pin(id: String, pinned: bool) -> Result<(), AppError>;
#[tauri::command] pub async fn clipboard_delete(id: String) -> Result<(), AppError>;
#[tauri::command] pub async fn clipboard_clear(keep_pinned: bool) -> Result<(), AppError>;
#[tauri::command] pub async fn clipboard_stack_push(id: String) -> Result<(), AppError>;
#[tauri::command] pub async fn clipboard_stack_pop() -> Result<Option<ClipEntry>, AppError>;
```

### 6.4 Port trait（win-integration 适配点）

```rust
pub trait ClipboardPort { fn start_listener(&self, cb: Box<dyn Fn(ClipContent)>) -> Result<()>; }
pub trait CapturePort   { fn capture_screen(&self, mode: CaptureMode) -> Result<Frame>; }
pub trait OcrPort       { fn recognize(&self, image: &Frame, lang: &str) -> Result<OcrResult>; }
pub trait HelloPort     { fn verify(&self, reason: &str) -> Result<()>; }
pub trait UsnIndexPort  { fn search(&self, query: &str, limit: u32) -> Result<Vec<FileHit>>; }
pub trait ConptyPort    { fn spawn(&self, cfg: TermCfg) -> Result<PtyHandle>; }
```

模块依赖 port trait，win-integration 提供 Windows 实现——模块单测无需真实系统调用。

---

## 7. 技术选型

| 层级 | 技术 | 说明 |
|------|------|------|
| 应用框架 | Tauri 2.x | 壳/IPC/窗口管理（WebView2） |
| 后端 | Rust 1.80+ / Tokio | workspace 多 crate；模块 panic 隔离在独立 tokio task |
| Windows API | `windows` crate 0.58+ | 按 §2.3 的 feature 清单收敛在 win-integration |
| 前端 | React 18 + TS 5 + Vite 5 | 类型安全 + 快速构建 |
| UI 组件 | Fluent UI React v9 | 原生 Windows 11 视觉、Mica/Acrylic、Per-Monitor V2 高 DPI |
| 状态 | Zustand | 轻量全局状态 |
| 存储 | SQLite(WAL) + rusqlite + FTS5 | 每模块独立库文件（O3） |
| 终端/编辑/图表 | xterm.js / Monaco / uPlot | 对应模块按需引入 |
| 插件 | wasmtime (WASM) | 沙箱执行、超时终止、API 版本兼容检查 |
| 加密 | Argon2id + AES-256-GCM + zeroize + secrecy | 只用标准原语；内存用后清零、VirtualLock 防换出 |
| 打包/更新 | NSIS + MSI，Tauri updater 签名更新 | 分发走 GitHub Release + winget/Scoop |
| 监控 | 结构化日志（本地优先）+ 可选 Sentry | 崩溃报告默认不上传（隐私优先） |

依赖治理：dependabot 自动 PR + 每月集中处理；`THIRD_PARTY_LICENSES.md` 记录参考项目与依赖协议（GPL 项目只借鉴设计，不复制代码）。

---

## 8. 异常处理与安全机制（优化重点）

### 8.1 统一错误体系

```rust
#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("模块错误: {0}")]  Module(#[from] ModuleError),
    #[error("存储错误: {0}")]  Storage(String),
    #[error("网络错误: {0}")]  Network(String),
    #[error("权限不足: {0}")]  Permission(String),
    #[error("配置错误: {0}")]  Config(String),
}
// code 规范: {MODULE}_{CATEGORY}_{NNN}，如 CLIPBOARD_STORAGE_001
// message 面向用户说明"发生了什么"；hint 给出"怎么办"（附操作按钮语义）
```

### 8.2 模块崩溃隔离

- 每模块运行在独立 `tokio::task`，panic 被 catch 并标记模块为 `Error` 状态；宿主与其它模块不受影响
- UI 显示"该模块已停止，点击重启"；重启走 `stop → init → start` 标准生命周期
- WASM 插件：执行超时自动终止，不得阻塞宿主

### 8.3 系统级恢复

- **系统代理残留**：启动时检测残留代理设置并提示恢复；panic hook 尽力还原；支持 `--restore-proxy` 紧急命令行参数
- **数据一致性**：SQLite WAL 自动恢复；文件操作原子写；启动时扫描未完成操作提示继续/回滚
- **配置/DB 迁移**：版本化 + 事务 + 备份回滚（§5.2 规则 5）
- **全局快捷键冲突**：宿主统一注册，冲突时按优先级仲裁并通知失败方；设置中心提供冲突检测面板

### 8.4 模块间冲突防护

| 冲突 | 防护 |
|------|------|
| 系统代理 vs TUN | 互斥开关，UI 明确提示 |
| 剪贴板同步循环 | `origin` 标记（§4.1） |
| 文件锁 | `LockFileEx` 协调，同步前检查占用 |
| TUN 自环流量 | 防火墙规则排除自身进程 |

### 8.5 数据安全红线（P0，写第一行代码前落实）

1. 剪贴板敏感数据（密钥/密码/身份证）加密存储 + 永不记录黑名单
2. 密码库：Argon2id KDF、AES-256-GCM、zeroize/VirtualLock、自动锁定即清零密钥
3. 同步：端到端加密，服务端只见密文；支持"仅局域网"模式
4. Tauri Capabilities 默认全拒，按模块显式授权

---

## 9. 实现路径

### 9.1 四阶段路线图

| 阶段 | 周期 | 交付 |
|------|------|------|
| 一：核心基础 | 第 1–3 月 | workspace 骨架、Module trait/注册表/事件总线、设置 UI（schema 自动生成）；**P0 模块：剪切板、截图、OCR**；CI 就绪 |
| 二：系统集成 | 第 4–6 月 | 代理(VPN)、密码库、文件管理(基础)、键鼠共享、桌面效率；托盘/快捷键统一整合 |
| 三：扩展能力 | 第 7–9 月 | 网盘/多协议驱动、文本 PDF、笔记知识、系统管理、终端运维 |
| 四：智能化打磨 | 第 10–12 月 | 条件-动作引擎、WASM 插件与市场、跨设备同步、性能优化、发布 1.0 |

阶段里程碑均有量化验收标准（如：启动 < 1.5s、剪切板捕获延迟 < 100ms、内存基线 < 250MB、FTS 搜索 < 50ms）。

### 9.2 测试策略（贯穿各阶段）

| 层级 | 要求 |
|------|------|
| 单元测试 | 各模块 crate 独立可测（mock ModuleContext/Port）；核心逻辑（分类器、加密、任务流水线）行覆盖 ≥ 80% |
| 集成测试 | 模块 × 宿主：注册/启停/配置迁移/事件路由；SQLite 迁移脚本前后兼容测试 |
| 端到端 | 关键场景：复制→历史出现；截图→标注→保存；OCR 触发→结果覆盖层；解锁→复制密码→自动清除；代理开→系统代理生效→崩溃→恢复 |
| 性能回归 | CI 每次提交跑基准：启动/内存/搜索响应/模块加载，退化超阈值即失败 |

### 9.3 CI/CD 与部署

- `ci.yml`：rustfmt + clippy + 前端 lint + 单测 + 集成测 + 性能基准（Windows runner）
- `release.yml`：NSIS+MSI 双格式、Tauri updater 签名、SignPath/自签证书、GitHub Release
- `nightly.yml`：夜间构建供尝鲜
- 分发：GitHub Releases（主）→ winget / Scoop → 官网直链
- 版本：语义化版本；配置与 DB 迁移随版本号管理；发布前执行发布检查清单

---

## 10. 蓝本优化决策记录（与蓝本差异一览）

1. **技术栈定案**：Tauri 2 + Rust（用户 2026-09-15 确认），Python 早先偏好废弃
2. **O1–O8 架构优化**（§2.2）：模块间只走事件总线、port trait 收敛系统依赖、每模块独立库、大对象出库、Sidecar 按需下载、删除跨平台代码、统一错误体系、事件背压
3. **异常处理体系化**（§8）：蓝本的零散风险清单收敛为错误码规范 + 崩溃隔离 + 系统级恢复 + 冲突防护四层机制
4. **优先级落地**：按用户 P0-first 原则，第一阶段只交付宿主 + 剪切板/截图/OCR 闭环，键鼠共享（TCP/UDP 既有设计）列入 P1
5. **合规策略**：GPL-3.0、代理模块只做框架不内置节点、分发避开应用商店、密码库只用标准加密原语
