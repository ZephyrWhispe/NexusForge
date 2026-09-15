# 01 host-core 宿主核心细化（代码级）

> 依赖：无（最先实现）｜ 上游：DESIGN.md §2/§6 ｜ 模块 crate：`crates/host-core/`

## 实现步骤总览

| 步骤 | 内容 | 依赖 |
|------|------|------|
| S1 | Workspace 与 Tauri 壳骨架 | — |
| S2 | 统一错误体系 | S1 |
| S3 | Module trait / ModuleContext / Port traits | S2 |
| S4 | 事件总线 | S2 |
| S5 | ModuleRegistry + 生命周期 + panic 隔离 | S3,S4 |
| S6 | 配置中心 / 快捷键 / 托盘 / 日志 / 崩溃恢复 | S5 |
| S7 | Tauri Plugin 集成 + IPC 注册 | S5,S6 |

---

## S1 Workspace 与壳骨架

### 输入/输出
- 输入：无
- 输出：可运行的空应用（窗口 + HMR）

### 代码结构

```toml
# 根 Cargo.toml
[workspace]
resolver = "2"
members = ["src-tauri", "crates/*"]

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
rusqlite = { version = "0.32", features = ["bundled", "functions"] }
rusqlite_migration = "1"
uuid = { version = "1", features = ["v7"] }        # 时间有序 id
zeroize = "1"
secrecy = "0.10"
windows = { version = "0.58", features = [] }       # 仅 win-integration 允许依赖
```

目录与职责（见 DESIGN §2.3）。`src-tauri/src/main.rs` 仅 10 行：初始化 tracing → 构建 tauri::Builder → 注册 nexusforge plugin → run。

### 技术细节与潜在问题
- `rusqlite` 必须用 `bundled` feature，避免用户机器 SQLite 版本差异。
- `uuid v7` 而非 v4：主键时间有序，避免 SQLite B-tree 随机写放大。
- workspace `resolver = "2"` 必须显式声明，否则 feature 统一行为不同。
- `.cargo/config.toml` 配置 `rust-lld` 链接器加速 Windows 构建（蓝本 §6.2）。

---

## S2 统一错误体系

### 代码结构

```rust
// crates/host-core/src/error.rs
#[derive(thiserror::Error, Debug)]
pub enum ModuleError {
    #[error("模块初始化失败: {0}")]      Init(String),
    #[error("模块启动失败: {0}")]        Start(String),
    #[error("模块停止失败: {0}")]        Stop(String),
    #[error("配置错误: {0}")]            Config(String),
    #[error("存储错误: {0}")]            Storage(String),
    #[error("能力不支持: {0}")]          Unsupported(String),
    #[error("模块未就绪")]                NotReady,
    #[error("模块已 panic: {0}")]        Panicked(String),
}

#[derive(thiserror::Error, Debug, serde::Serialize)]
#[serde(tag = "kind", content = "data")]
pub enum AppError {
    #[error("{message}")] Module   { code: String, message: String, hint: Option<String> },
    #[error("{message}")] Storage  { code: String, message: String },
    #[error("{message}")] Network  { code: String, message: String, retryable: bool },
    #[error("{message}")] Permission { code: String, message: String, hint: String },
    #[error("{message}")] Config   { code: String, message: String },
}

impl AppError {
    /// 模块侧快捷构造：AppError::module("CLIPBOARD_STORAGE_001", "写入历史失败", Some("检查磁盘空间"))
    pub fn module(code: &str, message: impl Into<String>, hint: Option<&str>) -> Self;
}
impl From<ModuleError> for AppError { /* 自动带出 code: "HOST_MODULE_{variant}" */ }
// 实现 serde::Serialize 使 Tauri IPC 可直接返回 AppError
```

### 算法逻辑
- 错误码常量集中定义在各模块 `codes.rs`，格式 `{MODULE}_{CATEGORY}_{NNN}`，CI 用脚本扫描重复码。

### 潜在问题
- 严禁 `Result<_, String>` 混入新代码 —— clippy 加自定义 lint 注释约束 + code review 把关。

---

## S3 Module trait / ModuleContext / Ports

### 代码结构

```rust
// crates/host-core/src/module.rs
#[derive(Clone, Serialize)]
pub struct ModuleInfo {
    pub id: &'static str,          // "clipboard" 等全局唯一
    pub name: &'static str,        // 中文显示名
    pub version: &'static str,
    pub icon: Option<&'static str>,
    pub priority: u8,              // 快捷键/托盘冲突仲裁用，小者优先
}

#[derive(Clone, Copy, PartialEq, Serialize)]
pub enum ModuleState { Uninitialized, Stopped, Running, Error }

/// 依赖注入容器 —— 模块测试时构造 MockModuleContext
pub struct ModuleContext {
    pub app_data_dir: PathBuf,
    pub event_bus: Arc<EventBus>,
    pub config: Arc<ConfigStore>,
    pub logger: tracing::Span,
    /// Port 集合：按能力接口查询，模块不得向下转型
    ports: Ports,
}
impl ModuleContext {
    pub fn port<T: Port>(&self) -> Option<Arc<T>>;   // 例如 ctx.port::<dyn ClipboardPort>()
}

pub trait Module: Send + Sync {
    fn info(&self) -> ModuleInfo;
    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError>;
    fn start(&self) -> Result<(), ModuleError>;
    fn stop(&self) -> Result<(), ModuleError>;
    fn config_schema(&self) -> serde_json::Value;      // JSON Schema
    fn apply_config(&self, values: serde_json::Value) -> Result<(), ModuleError>;
    fn status(&self) -> ModuleState;
}
```

```rust
// crates/host-core/src/ports.rs —— 全部 Port 汇总（win-integration 提供实现）
pub trait Port: Send + Sync {}

#[async_trait::async_trait]
pub trait ClipboardPort: Port {
    /// 启动监听；变更时回调 cb。回调在专用 OS 消息循环线程触发。
    fn start_listener(&self, cb: Box<dyn Fn(ClipContent) + Send + Sync>) -> Result<(), AppError>;
    fn write(&self, content: &ClipContent) -> Result<(), AppError>;   // 回写系统剪贴板
}
pub trait CapturePort: Port {
    fn enumerate_monitors(&self) -> Result<Vec<MonitorInfo>, AppError>;
    /// mode=FullScreen(window)/Region/Window；返回 BGRA 像素帧
    fn capture(&self, target: CaptureTarget) -> Result<Frame, AppError>;
}
pub trait OcrPort: Port {
    fn available_languages(&self) -> Result<Vec<String>, AppError>;
    fn recognize(&self, image: &Frame, lang: &str) -> Result<Vec<OcrLine>, AppError>;
}
pub trait HotkeyWinPort: Port { /* RegisterHotKey 的底层封装，供 HotkeyManager 用 */ }
pub trait HelloPort: Port { fn verify(&self, reason: &str) -> Result<(), AppError>; }
pub trait UsnIndexPort: Port { fn search(&self, q: &str, limit: u32) -> Result<Vec<FileHit>, AppError>; }
pub trait ConptyPort: Port { /* 终端模块阶段三使用 */ }
```

### 潜在问题
- Port trait 对象安全：方法参数/返回值禁止泛型与 `impl Trait`，统一用具体类型或 `Box<dyn>`。
- `ClipboardPort::write` 需处理"写入时忽略自身事件"——由 clipboard-core 的 origin 标记配合（见 02 文档 C3）。

---

## S4 事件总线

### 代码结构

```rust
// crates/host-core/src/events.rs
pub struct EventBus { /* 内部：RwLock<HashMap<Topic, broadcast::Sender<Event>>> */ }

impl EventBus {
    /// 启动时注册全部主题；未注册主题的 publish 返回错误（防运行时动态造主题）
    pub fn register_topic(&self, topic: &'static str, doc: &str) -> Result<(), AppError>;
    pub fn publish(&self, event: Event) -> Result<(), AppError>;
    /// 返回订阅句柄；drop 即退订
    pub fn subscribe(&self, topic: &str) -> Result<broadcast::Receiver<Event>, AppError>;
    /// 带去抖订阅：同 (topic, dedup_key) 在 window 内只投递最后一条 —— UI 消费用
    pub fn subscribe_debounced(&self, topic: &str, window: Duration)
        -> Result<broadcast::Receiver<Event>, AppError>;
}

pub struct Event {
    pub topic: &'static str,
    pub source: &'static str,
    pub payload: serde_json::Value,
    pub ts: i64,                   // 毫秒
}
```

### 核心算法
- **背压**：每主题 `broadcast::channel(cap=1024)`；channel 满时 `try_send`，失败则丢弃并 `tracing::warn!`（UI 类事件允许丢帧，持久化不允许走总线）。
- **去抖**：`subscribe_debounced` 内部为每 (topic,key) 维护 `HashMap<Key, Instant>`，未到 window 则覆盖 pending 值并重置定时器。
- **死信**：订阅端 `Lagged` 错误必须处理——记录 warn 并重新拉取（见各模块消费端模板）。

### 潜在问题
- broadcast 接收端处理慢会 Lagged，消费端必须设计为"拉最新状态"而非依赖逐条事件（UI 收到事件后调 IPC 拉数据）。
- 主题注册表用 `static TOPIC_REGISTRY: &[(&str, &str)]` 编译期列出，运行期 `register_topic` 校验其存在。

---

## S5 ModuleRegistry + 生命周期 + panic 隔离

### 代码结构

```rust
// crates/host-core/src/registry.rs
pub struct ModuleRegistry {
    modules: RwLock<HashMap<String, Arc<dyn Module>>>,
    order: RwLock<Vec<String>>,                    // 注册顺序 = 启动顺序
    tasks: RwLock<HashMap<String, JoinHandle<()>>>,// 模块守护 task
}

impl ModuleRegistry {
    pub fn register(&self, m: Arc<dyn Module>) -> Result<(), AppError>;   // id 去重
    pub fn get(&self, id: &str) -> Option<Arc<dyn Module>>;
    pub fn init_all(&self, ctx: Arc<ModuleContext>) -> Vec<(String, Result<(), ModuleError>)>;
    pub fn start_all(&self) -> Vec<(String, Result<(), ModuleError>)>;
    pub fn stop_all(&self);                        // 逆序停止
    pub fn restart(&self, id: &str) -> Result<(), AppError>;              // stop→init→start
    pub fn status_all(&self) -> Vec<(String, ModuleState)>;
}
```

### 核心算法：panic 隔离启动

```rust
// 伪代码 start_module(m):
let handle = tokio::spawn(async move {
    match tokio::task::spawn_blocking(move || m.start()).await {
        Ok(Ok(())) => set_state(Running),
        Ok(Err(e)) => { set_state(Error); tracing::error!(...) }
        Err(join_err) if join_err.is_panic() => {
            set_state(Error);                      // panic 被捕获，宿主存活
            publish_event("host.module_crashed", { module: id });
        }
        Err(e) => { set_state(Error); }
    }
});
// 循环 stop 逆序执行；stop 也包 spawn_blocking 防模块 stop 卡死宿主（超时 5s 强制返回）
```

### 状态机
`Uninitialized --init--> Stopped --start--> Running --stop--> Stopped`；任意态异常/panic → `Error`；`Error --restart--> Running`。

### 潜在问题
- `init_all` 中某模块失败**不阻断**其它模块（记录结果向量，UI 汇总显示）。
- `stop` 超时强制返回时，对应 tokio task 用 `handle.abort()`，并在状态机标注 `Error`（进程级资源泄漏风险记入日志）。
- 生命周期方法收 `&self`，模块内部状态一律 `RwLock`/`Mutex`，避免 `&mut self` 导致注册表持锁设计复杂化。

---

## S6 配置中心 / 快捷键 / 托盘 / 日志 / 崩溃恢复

### S6.1 ConfigStore

```rust
pub struct ConfigStore { /* global: RwLock<GlobalConfig>, modules: RwLock<HashMap<String, ModuleCfgFile>> */ }
pub struct GlobalConfig { pub schema_version: u32, pub enabled_modules: Vec<String>, pub hotkeys: HotkeyTable, /* … */ }
struct ModuleCfgFile { schema_version: u32, values: serde_json::Value }

impl ConfigStore {
    pub fn load(&self) -> Result<(), AppError>;                  // 启动调用：读+迁移+备份
    pub fn get_module<T: DeserializeOwned>(&self, id: &str) -> Result<T, AppError>;
    pub fn set_module(&self, id: &str, v: serde_json::Value) -> Result<(), AppError>;  // 版本校验→写临时文件→rename→发 config.changed 事件
}
```

**迁移算法**：`schema_version < 当前` 时：① 复制原文件到 `config/backups/{module}.{ver}.{ts}.json`（保留 3 份）② 执行注册的迁移链 `Vec<fn(Value)->Result<Value>>` ③ 失败回滚备份并向 UI 发警告事件。
**潜在问题**：`set_module` 必须先经模块 `config_schema` 的 JSON Schema 校验（用 `jsonschema` crate）再落盘，防止 UI 侧写入脏配置。

### S6.2 HotkeyManager（冲突仲裁）

```rust
pub struct HotkeyManager { /* registrations: RwLock<HashMap<HotkeyId, Owner{module, priority}>> */ }
impl HotkeyManager {
    pub fn register(&self, binding: HotkeyBinding, owner: ModuleInfo) -> Result<(), AppError>;
    pub fn unregister(&self, id: HotkeyId) -> Result<(), AppError>;
}
// 算法：同一键组合 → 已有 owner.priority 小者保留；新注册失败返回
// AppError::Permission{ code:"HOST_HOTKEY_001", hint:"在设置中更换快捷键" }
// 底层：win-integration 的 HotkeyWinPort 用 RegisterHotKey；注册失败（系统占用）返回同码但 hint 不同
```

### S6.3 TrayManager
聚合所有 `TrayProvider` 的菜单项，按模块 priority 排序 + 分隔线分组；模块 `Error` 状态时其菜单项置灰。用 `tray-icon` crate（Tauri 官方）。

### S6.4 日志
`tracing_subscriber`：`EnvFilter`（RUST_LOG 可覆盖）+ 每日滚动文件（`tracing-appender`，保留 7 天）+ 隐私过滤器 layer（对含 `password/token/secret` 字段的 JSON 自动脱敏）。

### S6.5 CrashRecovery
- `std::panic::set_hook`：写崩溃文件 `{appData}/crash/{ts}.json`（堆栈、模块 id、最近 50 条操作日志）→ 尽力执行已注册的恢复钩子（系统代理还原等）→ 恢复默认 hook 重新 panic。
- 启动扫描：① 残留系统代理检测（注册表 ProxyEnable=1 且非本应用写入）→ UI 提示一键恢复 ② `{appData}/pending_ops/` 未完成文件操作 → 提示继续/回滚 ③ crash 目录非空 → 引导用户查看。
- 提供 `--restore-proxy` 与 `--safe-mode`（只启动宿主不启动模块）命令行参数。

---

## S7 Tauri Plugin 集成

```rust
// crates/host-core/src/plugin.rs
pub struct NexusForgePlugin { registry: Arc<ModuleRegistry>, /* … */ }
impl tauri::Plugin for NexusForgePlugin {
    fn build(&self, app: &mut tauri::Builder) { /* manage state、注册全部 IPC 命令、窗口事件 */ }
}
// IPC 命令（宿主级）：
// host_modules_status() -> Vec<ModuleStatusDto>
// host_module_enable(id, enabled) / host_module_restart(id)
// host_config_get(id) / host_config_set(id, values)
// host_event_subscribe(topic)  —— 内部映射为 tauri emit 到窗口
```

**潜在问题**：
- `tauri::State` 只能托管 `'static` 数据，registry/context 全部 `Arc` 化。
- 事件转发到前端时统一格式 `{topic, source, payload, ts}`，前端 `ipc/` 层提供类型化 `onEvent<Topic>()`。
- Tauri 2 capabilities：`capabilities/default.json` 只开放宿主级命令；模块命令在各自模块启用时动态追加权限集。

## 验收（host-core 出口）

- [ ] 假模块 panic → 宿主存活、状态 Error、事件 `host.module_crashed` 可订阅
- [ ] 配置 schema_version 升级 → 自动迁移 + 备份生成
- [ ] 两模块注册同快捷键 → 低 priority 者收到明确错误
- [ ] `--safe-mode` 只启动宿主；`--restore-proxy` 清理残留代理
