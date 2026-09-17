# 08 WinOps 子系统细化（sys-core 系统管理 · Tweak 引擎扩展）

> 载体：DESIGN.md P2「系统管理 sys-core」（06 文档 SY1–SY4 的深化扩展，SY1 包管理器 / SY4 资源监控不在本篇范围）。
> 细化深度：代码级 —— 实体模型、目录 schema、BAVR 引擎、提权 Helper 协议、IPC/事件、UI 接线全部给出。
> 命名对齐：crate = `sys-core`，模块 id = `sys`，IPC 前缀 = `sys_`；Tweak 目录条目 id 前缀 = `winops.`（如 `winops.update.disable_auto`）；提权辅助进程 = `nexusforge-sys-helper.exe`。
> **技术栈修正（相对原始提案）**：UI 层为 React 18 + Fluent UI React v9（非 Vue 3）；`.NET 参考（ServiceController / TaskScheduler Managed Wrapper）`在 Rust 侧对应 windows crate 的 SCM / taskschd COM API，见 §6。

---

## 1. 架构分层

```
┌────────────────────────────────────────────────────────────┐
│ UI: SysPanel.tsx（React 18 + Fluent v9）                    │
│   分类导航 + 全局搜索 + 目录列表 + 详情/风险徽章 + 任务抽屉   │
└──────────────┬─────────────────────────────────────────────┘
               │ IPC（sys_* 命令）+ 事件（sys.* 主题）
┌──────────────▼─────────────────────────────────────────────┐
│ src-tauri: SysModule（Module trait）+ sys_* 命令注册         │
└──────────────┬─────────────────────────────────────────────┘
               │
┌──────────────▼─────────────────────────────────────────────┐
│ sys-core（新 crate，不依赖 windows crate）                   │
│   目录加载器 ←→ BAVR 引擎 ←→ 备份库(sys.db)                  │
│        │                     │                              │
│        ▼                     ▼                              │
│   Op 执行器 ──按 op 路由──┬── 进程内 Ports（HKCU 等低风险）    │
│                          └── HelperClient（需管理员操作）    │
└──────────────┬──────────────────────┬──────────────────────┘
               │                      │
┌──────────────▼──────────┐  ┌────────▼──────────────────────┐
│ win-integration Ports    │  │ nexusforge-sys-helper（提权）  │
│ RegistryOps/ServiceCtl/  │  │ 命名管道 JSON-RPC，方法白名单  │
│ TaskSchd/Appx/Hosts/Dns/ │  │ 崩溃不连累主进程，空闲自退     │
│ Maintenance/HelperSpawn  │  └───────────────────────────────┘
└──────────────────────────┘
```

路由规则（确定性，便于测试）：
- **HKCU 注册表、纯查询、内存清理（尽力而为）** → 主进程内 Port 直执行。
- **HKLM/服务/计划任务/Appx 卸载/Hosts 写入/DNS 设置/DISM/SFC/还原点** → 一律 HelperClient。Helper 未运行 → 返回 `SYS_ELEVATION_001`（UI 提示"需要管理员授权"→ 触发 spawn → UAC 弹窗）。

---

## 2. 实体模型与目录（代码级）

### 2.1 sys-core 核心类型

```rust
// crates/sys-core/src/model.rs
use serde::{Deserialize, Serialize};

pub type TweakId = String;      // "winops.update.disable_auto"
pub type BackupId = String;     // ULID

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel { Safe, Caution, High }

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TweakCategory { Update, Privacy, Service, Task, Apps, Ui, Network, Maintenance }

/// 目录里的静态条目（编译期嵌入 + catalog/ 外置包覆盖，见 2.2）
#[derive(Clone, Serialize, Deserialize)]
pub struct TweakItem {
    pub id: TweakId,
    pub category: TweakCategory,
    pub name: String,               // 中文显示名
    pub description: String,
    pub risk: RiskLevel,
    pub reversible: bool,
    pub recommended: bool,
    pub requires_admin: bool,
    pub reboot: bool,               // 应用后需重启生效
    pub applicable_os: OsConstraint,
    pub dependencies: Vec<TweakId>, // 应用前必须已应用（拓扑排序输入）
    pub conflicts: Vec<TweakId>,    // 互斥（UI 置灰 + 引擎强校验）
    pub ops: OpPlan,                // 见 2.3
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OsConstraint {
    pub min_build: u32,             // 如 19041
    #[serde(default)]
    pub max_build: Option<u32>,
    #[serde(default)]
    pub editions: Vec<String>,      // 空 = 不限（Home 无 gpedit，策略键部分无效，由 verify 兜底）
}

/// 扫描态与静态目录分离：目录可缓存，状态每次 scan 现算
#[derive(Clone, Serialize)]
pub struct TweakStatus {
    pub id: TweakId,
    pub state: TweakState,
    pub checked_at: i64,
}

#[derive(Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TweakState {
    NotApplied,                     // 未应用（verify 全部不符合）
    Applied,                        // 已应用（verify 全部符合）
    PartiallyApplied,               // 部分符合（提示"重扫或重应用"）
    Unavailable(String),            // OS 版本不满足 / 依赖未满足 / 权限不足
}
```

### 2.2 目录加载：内置嵌入 + 外置包覆盖（参考 CrapFixer 签名数据库，新增签名无需重编译）

```rust
// crates/sys-core/src/catalog.rs
// 来源优先级：{appData}/sys/catalog/*.json > 编译期嵌入(include_str! 目录)
// 同 id 冲突：外置覆盖内置，CatalogReloaded 事件通知 UI；schema 校验失败整包拒绝（不入半包）
pub struct CatalogLoader;

impl CatalogLoader {
    pub fn load(embedded_dir: &[(&str, &str)], external_dir: &std::path::Path)
        -> Result<Catalog, AppError>
    {
        // 1. 解析嵌入目录 → HashMap<TweakId, TweakItem>
        // 2. external_dir 按 *.json 文件名序解析 → 覆盖插入（覆盖时 tracing::warn!）
        // 3. 全图校验：dependencies/conflicts 引用存在性；环检测（同 topo_sort，§3.2）
        // 4. Catalog { items, by_category: HashMap<TweakCategory, Vec<TweakId>> }
    }
}
```

目录条目 schema（`catalog.v1`，用户提案 YAML 的 JSON 落地形态）：

```json
{
  "$schema": "winops/catalog.v1",
  "id": "winops.update.disable_auto",
  "category": "update",
  "name": "禁止 Windows 自动更新",
  "description": "通过策略键 NoAutoUpdate=1 并禁用 wuauserv 服务阻止自动更新。参考 WUB/Wu10Man 的多层拦截策略。",
  "risk": "high",
  "reversible": true,
  "recommended": false,
  "requires_admin": true,
  "reboot": false,
  "applicable_os": { "min_build": 19041, "editions": ["Pro", "Enterprise", "Education"] },
  "dependencies": [],
  "conflicts": ["winops.update.restore_auto"],
  "ops": {
    "backup": [
      { "kind": "registry", "hive": "HKLM", "path": "SOFTWARE\\Policies\\Microsoft\\Windows\\WindowsUpdate\\AU",
        "values": ["NoAutoUpdate", "AUOptions"] },
      { "kind": "service", "name": "wuauserv" }
    ],
    "apply": [
      { "kind": "registry", "hive": "HKLM", "path": "SOFTWARE\\Policies\\Microsoft\\Windows\\WindowsUpdate\\AU",
        "value": "NoAutoUpdate", "type": "dword", "data": 1, "create_key": true },
      { "kind": "service", "name": "wuauserv", "start_type": "disabled" }
    ],
    "verify": [
      { "kind": "registry", "path": "HKLM\\SOFTWARE\\Policies\\Microsoft\\Windows\\WindowsUpdate\\AU",
        "value": "NoAutoUpdate", "expect": 1 },
      { "kind": "service", "name": "wuauserv", "expect_start": "disabled" }
    ]
  }
}
```

要点：
- `backup` 不写则自动从 `apply` 反推（registry/service op 均可反演）；显式 `revert` 数组仅用于备份无法覆盖的操作（如缓存清理类 FileClean）。
- `verify` 是 BAVR 的 V，也是 scan 的状态来源（probe = 只跑 verify）。
- 风险分级规则（合规红线）：`High` 包含一切禁用安全更新 / Defender / 安全中心 / 卸载系统组件的条目，默认不勾选 + 二次确认（§8）。

### 2.3 原子操作（Op）与备份值

```rust
// crates/sys-core/src/op.rs —— 引擎可执行的全部原子操作种类
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Op {
    Registry {
        hive: Hive,                 // Hkcu | Hklm
        path: String,               // 不含 hive 前缀
        value: String,
        #[serde(default)] r#type: RegType,   // dword | qword | sz | expand_sz | multi_sz | none(删除)
        data: RegData,
        #[serde(default)] create_key: bool,
    },
    Service {
        name: String,
        #[serde(default)] start_type: Option<StartType>, // auto | delayed_auto | manual | disabled
        #[serde(default)] action: Option<SvcAction>,     // stop | start
    },
    Task {                          // 计划任务
        path: String,               // "\Microsoft\Windows\...\Consolidator"
        enabled: bool,
    },
    AppxRemove {
        name: String,               // 包名通配，如 "Microsoft.549981C3F5F10"
        #[serde(default)] all_users: bool,
    },
    Exec {                          // 仅 Helper 白名单程序（§4.3）
        program: String,            // "powercfg" | "dism" | "sfc" | "netsh" | ...
        args: Vec<String>,
        #[serde(default = "default_timeout_ms")] timeout_ms: u32,
    },
    HostsPatch {
        group_id: String,           // "winops.telemetry" —— BEGIN/END 标记组，可整组回滚
        endpoints: Vec<String>,
        mode: HostsMode,            // Append | Remove
    },
    FileClean {                     // 如 SoftwareDistribution\Download
        path: String,
        #[serde(default = "default_recursive")] recursive: bool,
        #[serde(default = "default_skip_hours")] skip_recent_hours: u32, // 24h 内新文件跳过
    },
}

/// 备份值：op 各自的"原状"快照，JSON 序列化进 backups.ops_json
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackupValue {
    Registry { existed: bool, r#type: Option<RegType>, data: Option<RegData>, key_created_by_us: bool },
    Service { start_type: Option<StartType>, was_running: bool },
    Task { enabled: bool },
    Hosts { group_absent: bool },           // 组不存在 → 回滚 = 整组删除
    FileClean { /* 无需备份，revert = 空操作（reversible=false 时不可回滚） */ },
    Exec { /* 通常不可回滚，仅 powercfg 等记录原 scheme GUID */ previous: Option<String> },
}
```

### 2.4 备份库（独立 SQLite：`{appData}/db/sys.db`，WAL）

```sql
CREATE TABLE IF NOT EXISTS backups (
  backup_id  TEXT PRIMARY KEY,          -- ULID
  tweak_id   TEXT NOT NULL,
  created_at INTEGER NOT NULL,          -- unix ms
  ops_json   TEXT NOT NULL,             -- Vec<{op: Op, original: BackupValue}>，先落库后执行（崩溃安全）
  result     TEXT NOT NULL              -- applied | verify_failed | reverted
);
CREATE INDEX IF NOT EXISTS idx_backups_tweak ON backups(tweak_id, created_at DESC);

CREATE TABLE IF NOT EXISTS audit_log (
  op_id     TEXT NOT NULL,              -- 一次 sys_apply/revert 任务
  ts        INTEGER NOT NULL,
  actor     TEXT NOT NULL,              -- "ui" | "profile:standard" | "plugin:xxx"
  tweak_id  TEXT NOT NULL,
  action    TEXT NOT NULL,              -- probe|backup|apply|verify|revert
  detail    TEXT NOT NULL               -- JSON：结果/错误码/耗时
);
CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(ts);
-- retention：backups 每 tweak 保留最近 50 份（参考 Hosts Switcher），过期清理在 apply 成功后异步执行
```

---

## 3. BAVR 引擎（代码级）

### 3.1 驱动 trait（host-core::ports 新增，win-integration + mock 双实现）

```rust
// crates/host-core/src/ports/sys.rs —— 7 个窄 Port（与 ThumbPort 等同等粒度）
#[async_trait]
pub trait RegistryOpsPort: Send + Sync {
    fn read(&self, hive: Hive, path: &str, value: &str) -> Result<Option<(RegType, RegData)>, AppError>;
    fn write(&self, hive: Hive, path: &str, value: &str, t: RegType, data: &RegData, create_key: bool) -> Result<(), AppError>;
    fn delete(&self, hive: Hive, path: &str, value: &str) -> Result<(), AppError>;   // 值不存在 = Ok
}

#[async_trait]
pub trait ServiceCtlPort: Send + Sync {
    fn list(&self) -> Result<Vec<ServiceInfo>, AppError>;            // name/display/start_type/state
    fn query(&self, name: &str) -> Result<ServiceInfo, AppError>;
    fn set_start_type(&self, name: &str, st: StartType) -> Result<(), AppError>;
    fn stop(&self, name: &str, timeout_ms: u32) -> Result<(), AppError>;  // ControlService + 轮询至 STOPPED
    fn start(&self, name: &str) -> Result<(), AppError>;
}

#[async_trait]
pub trait TaskSchedulerPort: Send + Sync {
    fn list(&self, folder: &str) -> Result<Vec<TaskInfo>, AppError>; // path/state/last_run/next_run/actions
    fn set_enabled(&self, path: &str, enabled: bool) -> Result<(), AppError>;
}

#[async_trait]
pub trait AppxPort: Send + Sync {
    fn list(&self, name_glob: &str) -> Result<Vec<AppxInfo>, AppError>;
    fn remove_current_user(&self, package_full_name: &str) -> Result<(), AppError>;
    fn remove_provisioned(&self, package_name: &str) -> Result<(), AppError>;  // DISM，需管理员
}

#[async_trait]
pub trait HostsFilePort: Send + Sync {
    fn read(&self) -> Result<String, AppError>;
    /// 按标记组追加/删除；原子写（同目录 tmp + ReplaceFileW）；返回写后全文
    fn patch_group(&self, group_id: &str, endpoints: &[String], mode: HostsMode) -> Result<String, AppError>;
}

#[async_trait]
pub trait DnsConfigPort: Send + Sync {
    fn list_adapters(&self) -> Result<Vec<AdapterInfo>, AppError>;   // GetAdaptersAddresses
    fn set_dns(&self, adapter_name: &str, servers: &[String]) -> Result<(), AppError>;
    fn flush_cache(&self) -> Result<(), AppError>;                   // DnsFlushResolverCache
}

#[async_trait]
pub trait MaintenancePort: Send + Sync {
    fn create_restore_point(&self, name: &str) -> Result<(), AppError>;        // SRSetRestorePointW，可降级
    fn empty_working_set(&self, exclude: &[u32]) -> Result<u32, AppError>;     // 返回处理进程数
    /// DISM/SFC 长任务：输出经 cb 流式回传（→ sys.repair_output 事件，200ms 节流）
    fn run_repair(&self, kind: RepairKind, cb: Box<dyn Fn(String) + Send>) -> Result<i32, AppError>;
}

#[async_trait]
pub trait HelperSpawnPort: Send + Sync {
    /// ShellExecuteExW(SEE_MASK_FLAG_NO_UI + runas) 提权拉起 helper；
    /// spec 含管道名、token（环境变量注入）、主进程 PID/exe 路径
    fn spawn(&self, spec: &HelperSpec) -> Result<u32, AppError>;     // -> helper pid
}
```

### 3.2 引擎执行流

```rust
// crates/sys-core/src/engine.rs
pub struct TweakEngine {
    catalog: RwLock<Arc<Catalog>>,
    repo: BackupRepo,                 // sys.db
    sys_ports: SysPorts,              // 上列 Port 的聚合句柄
    helper: HelperClient,             // §4
    running: Mutex<Option<TaskId>>,   // 全局单任务互斥（UI 置灰 + 引擎拒绝）
}

impl TweakEngine {
    pub async fn apply(&self, ids: &[TweakId], opts: ApplyOptions, actor: &str) -> Result<TaskReport, AppError> {
        let _guard = self.acquire_task()?;                       // SYS_BUSY_001
        let order = topo_sort(ids, &self.catalog)?;              // 依赖拓扑；环 → SYS_CATALOG_002
        self.check_conflicts(&order)?;                           // conflicts 已应用 → SYS_CONFLICT_001

        let mut backup_id = None;
        // ---- B：一次性全量备份，先落库再执行（崩溃可恢复）----
        let records = self.backup_ops(&order).await?;            // [OpBackup {op, original}]
        backup_id = Some(self.repo.insert_backup(&order, &records)?);

        // ---- A：顺序应用，失败即补偿已成功项 ----
        let mut done: Vec<(&TweakId, Vec<&Op>)> = vec![];
        for tid in &order {
            for op in &self.ops_of(tid)?.apply {
                match self.exec_op(op).await {
                    Ok(()) => { /* audit_log */ }
                    Err(e) => {
                        // 补偿：逆序 restore done 中该 tweak 的 op（best-effort，逐条 audit）
                        self.compensate(&done).await;
                        self.repo.mark(backup_id, "verify_failed")?;   // 记录失败现场
                        return Err(e);
                    }
                }
            }
            done.push((tid, /*ops*/));
            emit!(sys.task_progress { tweak_id: tid, phase: "applied" });  // 200ms 节流在 events 层
        }

        // ---- V：verify，失败重试 1 次后如实上报（不自动回滚，UI 给"一键还原"入口）----
        let vr = self.verify_ids(&order).await;
        if vr.iter().any(|v| v.state == TweakState::PartiallyApplied) {
            self.repo.mark(backup_id, "verify_failed")?;
            emit!(sys.verify_result { backup_id, results: vr });
        }
        self.repo.mark(backup_id, "applied")?;
        Ok(TaskReport { backup_id: backup_id.unwrap(), results: vr })
    }

    pub async fn revert(&self, backup_id: &BackupId) -> Result<TaskReport, AppError> {
        // 读 ops_json → 逆序 restore(op, original)；tombstone（原值缺失）→ RegDeleteValue；
        // key_created_by_us → 删值后若键空则删键；HostsPatch → 整组删除
    }
}

fn topo_sort<'a>(ids: &[TweakId], cat: &Catalog) -> Result<Vec<TweakId>, AppError> {
    // Kahn：边 id → dep（dep 先应用）；入度 0 入队按目录声明序稳定排序；
    // 队列耗尽仍有剩余 → 环 → Err(SYS_CATALOG_002)
    // 依赖未应用且不在本次集合 → 该项 Unavailable("依赖未满足")，跳过并记录
}
```

路由分发：

```rust
async fn exec_op(&self, op: &Op) -> Result<(), AppError> {
    match op {
        Op::Registry { hive: Hive::Hkcu, .. } => self.sys_ports.registry.write(...),   // 进程内
        Op::Registry { .. } | Op::Service { .. } | Op::Task { .. }
        | Op::AppxRemove { all_users: true, .. } | Op::HostsPatch { .. }
        | Op::FileClean { .. } | Op::Exec { .. } => self.helper.call_op(op).await,     // 提权
        Op::AppxRemove { all_users: false, .. } => self.sys_ports.appx.remove_current_user(...).await,
    }
}
```

### 3.3 幂等与自愈边界

- `apply` 幂等：verify 已 Applied 的项默认跳过（`ApplyOptions.force` 可强制重放）。
- **WUB 式"防自愈"的 v1 边界**：不做守护进程与系统对抗。模块 start 时对 `risk==High` 且已应用项静默重 verify，回归 → `sys.verify_result` 事件 + UI 黄条提示"系统已恢复该设置，可重新应用"。守护计划任务列入 backlog（阶段四 A4 具备 Task Scheduler 集成后可复用）。

---

## 4. 提权 Helper 进程（代码级）

### 4.1 生命周期

```
首次需要提权操作：
  main → HelperSpawnPort::spawn({ pipe: rf["\\.\pipe\nexusforge-sys-helper-v1-{main_pid}"],
                                   token: 64B 随机 hex（环境变量 NF_HELPER_TOKEN 注入）,
                                   parent_pid, parent_exe })
       → UAC 弹窗（ShellExecuteExW runas）→ helper 启动
  helper → 连接管道 → 发 hello 通知 {token, pid, version}
  main   → 校验 token + OpenProcess(pid) 路径 == 主 exe 同目录 → ready
  空闲 120s 无请求 → helper 自行退出；helper 崩溃 → HelperState::Down，下次调用重新 spawn
```

### 4.2 管道协议（tokio named pipe + JSON-RPC 2.0，帧格式复用 kvm 风格）

```
传输：\\.\pipe\nexusforge-sys-helper-v1-{pid}（字节模式，单连接双向）
帧：[u32 BE len][payload]
payload = 单个 JSON-RPC 消息：
  请求 main→helper：{"jsonrpc":"2.0","id":1,"method":"registry.apply","params":{...}}
  响应 helper→main：{"jsonrpc":"2.0","id":1,"result":{...}}
                    / {"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"ACCESS_DENIED"}}
  通知 helper→main：{"jsonrpc":"2.0","method":"event","params":{"topic":"sys.repair_output","data":...}}
超时：单请求默认 30s（DISM/SFC 类 exec 命令除外，以事件流保活，空闲 5min 判死）
```

### 4.3 方法白名单（helper 只实现下列方法，拒绝一切其他调用）

```
helper.ping
registry.backup / registry.apply / registry.restore          （HKLM/HKCU 全量）
service.list / service.query / service.set_start / service.stop / service.start
task.list / task.set_enabled                                  （taskschd COM）
appx.remove_provisioned                                       （DISM /Remove-ProvisionedAppxPackage）
hosts.read / hosts.patch / hosts.reset_group
dns.list / dns.set / dns.flush                                （netsh 白名单参数模板）
exec.powercfg / exec.dism / exec.sfc / exec.netsh / exec.onedrive_uninstall
maintenance.restore_point / maintenance.empty_working_set / maintenance.clean_update_cache
```

安全约束（全部强制）：
- exec 类 program 校验白名单精确匹配（不含路径解析，仅 `powercfg|dism|sfc|netsh` 等固定名），args 走参数模板拼接，**禁止**透传任意字符串。
- helper 校验调用方：main 传入 `parent_pid` → OpenProcess + QueryFullProcessImageNameW 必须 == 主 exe 同目录同名；token 不匹配 → 断开。
- 管道 ACL 限当前用户 SID；单实例互斥（管道已存在 → 退出）。
- helper 不写日志到磁盘（审计由主进程 audit_log 承担），崩溃 dump 禁用（与 V 模块同纪律）。

### 4.4 代码落点

```rust
// crates/sys-core/src/helper.rs —— HelperClient（纯 tokio，不碰 windows crate）
pub struct HelperClient { state: RwLock<HelperState>, pipe: Mutex<Option<PipeClient>> }
impl HelperClient {
    pub async fn ensure_up(&self) -> Result<(), AppError>;       // Down → HelperSpawnPort.spawn → 握手
    pub async fn call_op(&self, op: &Op) -> Result<(), AppError>; // Op → method 映射 + 超时
    pub fn state(&self) -> HelperState;                           // Up | Down | Spawning
}
// crates/win-integration/src/helper.rs —— HelperSpawnPort 实现（ShellExecuteExW runas）
// 测试：sys-core 用 mock PipeServer（同名管道假 helper）跑全协议；真机集成测试 #[ignore] 默认跳过
```

---

## 5. Op → Windows API 映射（win-integration 实现要点）

| Port | 实现 | 关键 API / 要点 | 降级路径 |
|---|---|---|---|
| RegistryOpsPort | `win-integration/src/sys/registry.rs` | RegCreateKeyExW(KEY_READ\|KEY_WRITE) / RegQueryValueExW / RegSetValueExW / RegDeleteValueW；tombstone 语义（原值缺失 → restore 即删值） | 无（本就是基础 API） |
| ServiceCtlPort | `sys/service.rs` | OpenSCManagerW → OpenServiceW；启动类型 ChangeServiceConfigW（2/3/4 映射 auto/manual/disabled，delayed_auto = Start=2 + ChangeServiceConfig2W DELAYED_AUTO_START）；启停 ControlServiceW + QueryServiceStatus 轮询（500ms 步进，超时可控） | SCM 拒绝 → ELEVATION_REQUIRED |
| TaskSchedulerPort | `sys/taskschd.rs` | COM：CoInitializeEx(MTA 专用线程) → CoCreateInstance(CLSID_CTaskScheduler) → ITaskService::Connect → GetFolder("\")→GetTasks(TASK_ENUM_HIDDEN)；IRegisteredTask::Set_Enabled(VARIANT_BOOL)；专用线程池复用（COM 单元纪律） | COM 失败 → `schtasks /Change /TN <path> /Disable`（Helper exec） |
| AppxPort | `sys/appx.rs` | WinRT Windows.Management.Deployment::PackageManager（FindPackagesAsync 通配 / RemovePackageAsync 当前用户）；provisioned 走 Helper DISM | 包被系统占用 → 报错不重试；提示"可从 Store 重装" |
| HostsFilePort | `sys/hosts.rs` | 读 `System32\drivers\etc\hosts`；标记组 `# >>> NexusForge BEGIN (<group_id>)` / `END`（TelemetryGuard 式可精确回滚）；写 = 同目录 tmp + ReplaceFileW（保留属性）；写后 DnsFlushResolverCache | 文件被占用/EDR 拦截 → 明确报错提示 |
| DnsConfigPort | `sys/dns.rs` | 枚举 GetAdaptersAddresses；设置走 Helper `netsh interface ip set dns name="<adapter>" static <dns>` + `ipconfig /flushdns`（参数模板拼接，adapter 名做合法性过滤 `[A-Za-z0-9 ()-]`） | netsh 失败 → ELEVATION_REQUIRED |
| MaintenancePort | `sys/maintenance.rs` | 还原点 SRSetRestorePointW（srclient；需系统还原已开启 + 磁盘余量）；内存清理 EnumProcesses → OpenProcess(SET_QUOTA\|QUERY) → EmptyWorkingSet（跳过自身/系统进程）；DISM/SFC 经 Helper exec 流式输出 | 还原点失败 → 可降级仅备份（BAVR 照常）；内存清理部分进程失败计数上报 |
| HelperSpawnPort | `helper.rs` | ShellExecuteExW（SEE_MASK_NOCLOSEPROCESS，lpVerb="runas"），等待进程出现并握手（3s 超时 → SYS_ELEVATION_002） | UAC 取消 → Down，UI 提示 |

win-integration 新增文件：`src/sys/mod.rs { registry.rs, service.rs, taskschd.rs, appx.rs, hosts.rs, dns.rs, maintenance.rs, helper.rs }`；`Cargo.toml` windows features 追加 `Win32_System_Registry, Win32_System_Services, Win32_System_TaskScheduler, Win32_System_Com, Win32_NetworkManagement_IpHelper, Win32_NetworkManagement_Dns, Win32_UI_Shell`（Appx 用 `Management_Deployment` WinRT feature）。

---

## 6. Tweak 目录内容清单（v1 首发家族与代表条目）

> 注册表路径为目录数据的权威来源，此处列代表条目；完整清单随 catalog JSON 交付（W3–W6 各家族一批）。

### 6.1 update（参考 WUB / Wu10Man / Windows Update Mini Tool）
| TweakId | 关键操作 |
|---|---|
| winops.update.disable_auto | AU: NoAutoUpdate=1, AUOptions=2；wuauserv → disabled |
| winops.update.pause_feature_only | 仅暂停功能更新：TargetReleaseVersion=1 + TargetReleaseVersionInfo=<当前版本>（保留安全更新） |
| winops.update.exclude_driver | ExcludeWUDriversInQualityUpdate=1 |
| winops.update.clear_cache | Exec(stop wuauserv/bits) + FileClean(SoftwareDistribution\Download) + 恢复服务启动类型（reversible=false，提示性备份） |

防自愈：disable_auto 应用后由 §3.3 重扫回归检测兜底（v1 不做守护任务）。

### 6.2 privacy（参考 TelemetryGuard / Privatezilla / wintools）——**明确不触碰 Windows Update / Defender / Store / 激活**
| TweakId | 关键操作 |
|---|---|
| winops.privacy.telemetry_off | DataCollection: AllowTelemetry=0；DiagTrack/dmwappushservice disabled；WER Disabled=1 |
| winops.privacy.telemetry_tasks_off | 计划任务：Consolidator、UsbCeip、ProgramDataUpdater、Compatibility Appraiser 等 → disabled |
| winops.privacy.copilot_off | TurnOffWindowsCopilot=1 (HKCU Policies)；ShowCopilotButton=0 |
| winops.privacy.recall_off | WindowsAI: DisableAIDataAnalysis=1 (HKLM Policies) |
| winops.privacy.ad_id_off | AdvertisingInfo Enabled=0 (HKCU)；ContentDeliveryManager 各 SubscribedContent-*=0、SilentInstalledAppsEnabled=0 |
| winops.privacy.location_off | LocationAndSensors DisableLocation=1；SensorPermissions 逐项 deny |
| winops.privacy.activity_history_off | EnableActivityFeed=0、PublishUserActivities=0、UploadUserActivities=0 |
| winops.privacy.hosts_block_telemetry | HostsPatch(telemetry 组，~26 端点，TelemetryGuard Strict 清单) |

### 6.3 service（参考 WinServicesTool 收藏分组 / PSSM 风险标注）
- 服务清单 = ServiceCtlPort::list 全量 + 内置风险标注表（安全禁用 / 谨慎 / 高风险）+ 用户收藏分组（存 sys.config 的 favorites 数组）。
- 修改启动类型即临时 Op（引擎内联生成单 op TweakItem），同样走 BAVR 备份。

### 6.4 task（参考 FluentTaskScheduler 仪表板）
- TaskSchedulerPort::list 全量展示（含 last_run/next_run/上次结果），支持批量 disable/enable；"推荐禁用清单"条目复用 Task op（如 6.2 telemetry_tasks_off 的展开视图）。

### 6.5 apps（参考 Win11Debloat / Winhance 系统组件与冗余软件区分）
| TweakId | 关键操作 |
|---|---|
| winops.apps.remove_copilot_appx | AppxRemove("Microsoft.Copilot", all_users) |
| winops.apps.remove_bing_weather 等 | 预置清单 ~40 项 UWP（勾选式，非一键全删） |
| winops.apps.remove_onedrive | Exec(onedrive_uninstall：`%SystemRoot%\SysWOW64\OneDriveSetup.exe /uninstall` 或 System32 版) + 残留注册表清理 |
| winops.apps.block_edge_reinstall | 注册表阻止 Edge 自动重装（高风险标注） |

系统组件（Store / Defender / WebView2 / VCLibs 等）在目录中 `risk=high` 且默认隐藏于"预置清单"，仅出现在"自定义筛选"高级视图。

### 6.6 ui（参考 Windhawk 安全可逆 / WinUtil 预设）
| TweakId | 关键操作 |
|---|---|
| winops.ui.classic_context_menu | HKCU\Software\Classes\CLSID\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\InprocServer32 空默认值 + Exec(重启 explorer，需确认) |
| winops.ui.taskbar_widgets_off | Advanced: TaskbarDa=0 |
| winops.ui.taskbar_chat_off | Advanced: TaskbarMn=0 |
| winops.ui.taskbar_align_left | Advanced: TaskbarAl=0 |
| winops.ui.start_recommendations_off | Start_IrisRecommendations=0 |
| winops.ui.power_ultimate | Exec(powercfg -duplicatescheme e9a42b02-...) → 记录原 scheme 于 BackupValue::Exec |
| winops.ui.temp_clean | FileClean(%TEMP% 等，白名单 24h + 回收站可选) |

### 6.7 network（参考 Hosts Switcher / SwitchHosts / Optimizer）
- DNS 快速切换：DnsConfigPort + 预设方案（自动/Cloudflare/Google/自定义），切换前记录原 DNS（BackupValue 同机制，backups.dns 专用组）。
- Hosts 编辑器：HostsFilePort::read 直出编辑 + patch_group 保存；智能备份去重（同内容不重复入库，保留 50 份）。
- Teredo/ISATAP 关闭：Exec(netsh interface teredo set state disabled)（谨慎级）。

### 6.8 maintenance + 高风险族（W7）
- DISM/SFC：RepairKind::{DismScanHealth, DismRestoreHealth, DismComponentCleanup, SfcScanNow}，输出流 → `sys.repair_output`。
- Defender 实时保护开关（High，篡改保护拦截时如实报错并给出官方指引）；安全中心服务仅展示不改。

### 6.9 Profile 预设（对应用户"新机初始化"场景）

```json
// {appData}/sys/profiles/standard.json —— 内置 standard / minimal / aggressive 三档
{
  "id": "standard",
  "name": "标准初始化",
  "include_risky": false,
  "steps": [
    "winops.privacy.telemetry_off", "winops.privacy.ad_id_off", "winops.privacy.copilot_off",
    "winops.ui.taskbar_widgets_off", "winops.ui.classic_context_menu",
    "winops.update.exclude_driver"
  ]
}
```

任务编排归位：用户提案的 YAML 编排（scan→apply→verify→notify + 条件分支）**不在 WinOps v1 内置**——由阶段四 automation-core 的 `Action::IpcCommand{module:"sys", cmd:"sys_apply", ...}`（07 文档 A3）编排；v1 的 profile 即"新机初始化"的串行实现（apply 顺序执行 + 完成事件 + reboot_required 汇总提示）。

---

## 7. src-tauri 集成

### 7.1 接线（state.rs 增量，模式与现有模块一致）

```rust
// Ports 注册
ports.register::<dyn RegistryOpsPort>(Arc::new(win_integration::sys::registry::RegistryOps::new()));
// ... 其余 7 个 Port 同理（HelperSpawnPort 走 ShellExecuteExW）
// 模块注册
let sys = Arc::new(SysModule::new());
config.register_schema("sys", sys.config_schema());
registry.register(sys.clone())?;
// HostState 增字段 sys: Arc<SysModule>
```

```rust
// SysModule（crates/sys-core/src/module.rs）
impl Module for SysModule {
    fn info(&self) -> ModuleInfo { ModuleInfo { id: "sys", name: "系统管理", version: "0.1.0", icon: Some("wrench"), priority: 80 } }
    fn init(&self, ctx) -> Result<(), ModuleError> {
        // 1. CatalogLoader::load（嵌入 + external_dir）  2. 打开 sys.db（WAL）
        // 3. profiles 目录扫描  4. 高风险回归检测（§3.3，异步不阻塞 init）
    }
    fn start(&self) -> ...   // start 时同样触发一次回归 verify（防自愈提示）
    fn config_schema(&self) -> json!({
        "type": "object",
        "properties": {
            "confirm_risky": { "type": "boolean", "default": true, "title": "高风险项二次确认" },
            "backup_retention": { "type": "integer", "default": 50, "minimum": 5, "maximum": 200 },
            "helper_idle_exit_sec": { "type": "integer", "default": 120 },
            "host_block_telemetry": { "type": "boolean", "default": false }
        }
    })
}
```

### 7.2 IPC 命令（19 个，命名 `{module}_{action}`，阻塞调用一律 spawn_blocking，注意闭包 move 捕获需提前 clone —— 见 M6 教训 svc2/id2 模式）

| 命令 | 签名要点 |
|---|---|
| sys_catalog | () → Vec<TweakItemDto>（目录 + 缓存的可用性） |
| sys_scan | (categories?: Vec<TweakCategory>) → Vec<TweakStatusDto>（并发 verify，逐项 200ms 超时） |
| sys_apply | (ids: Vec<String>, include_risky: bool) → TaskReportDto |
| sys_verify | (ids: Vec<String>) → Vec<VerifyResultDto> |
| sys_revert | (backup_id: String) → TaskReportDto |
| sys_revert_tweak | (tweak_id: String) → TaskReportDto（取最近 backup） |
| sys_backups | (tweak_id?: String) → Vec<BackupMetaDto> |
| sys_profiles | () → Vec<ProfileDto> |
| sys_apply_profile | (id: String, include_risky: bool) → TaskReportDto |
| sys_services | (filter?: String) → Vec<ServiceInfoDto>（含风险标注） |
| sys_service_set_start | (name, start_type) → ()（单 op 内联 Tweak 走 BAVR） |
| sys_tasks | (folder?: String) → Vec<TaskInfoDto> |
| sys_task_set_enabled | (path: String, enabled: bool) → () |
| sys_hosts_get / sys_hosts_save | content ↔ patch（编辑器用） |
| sys_dns_list / sys_dns_set / sys_dns_flush | 适配器/方案切换 |
| sys_restore_point | (name: String) → () |
| sys_repair | (kind: RepairKind) → RepairHandleDto（输出走事件） |
| sys_clean_memory | () → u32 |
| sys_helper_status | () → HelperStateDto；sys_catalog_reload() / sys_export_log(format) → PathDto |

### 7.3 事件主题（events.rs TOPIC_REGISTRY 增量 —— **缺注册会静默拒发**）

```
sys.task_progress  { task_id, tweak_id, phase: backup|apply|verify|done, current, total, message? }
sys.task_done      { task_id, backup_id?, summary }
sys.task_failed    { task_id, error: AppError }
sys.verify_result  { backup_id?, results: [{tweak_id, state}] }
sys.repair_output  { kind, line }          （200ms 节流）
sys.helper_state   { state: up|down|spawning }
```

### 7.4 DTO 纪律（M6 教训：serde 缺字段不报错，TS↔Rust 必须逐字段核对）

每个 DTO 在 `src/ipc/client.ts` 定义对应 interface，字段名/可选性两边逐一对齐；新增字段必须同时改两侧并在测试中断言 JSON 快照。

---

## 8. 前端（React 18 + Fluent UI v9）

### 8.1 SysPanel.tsx（src/modules/sys/，参考 Winhance 搜索即导航）

```
┌─────────────┬───────────────────────────────┬──────────────┐
│ 分类导航      │ 搜索框（过滤 id/name/描述）      │ 详情面板       │
│ (8 类+Profile)│ 列表：复选框|名称|风险徽章|状态徽章│  描述/依赖/冲突 │
│              │ 推荐星标|reboot 标记            │  最近备份列表   │
│              │ (虚拟列表，万级条目)             │  应用/还原按钮  │
├─────────────┴───────────────────────────────┴──────────────┤
│ 状态条：Helper 状态点 | 任务进度条(sys.task_progress) | 创建还原点 │
└─────────────────────────────────────────────────────────────┘
```

- 风险徽章：Safe=绿 / Caution=橙 / High=红；High 未勾选时复选框默认禁用，勾选触发确认 Dialog（需勾选"我已了解此操作的风险"+ 显示将修改的具体项）。
- 任务抽屉：apply/revert 进行中 → 右侧滑出（复用 F 操作队列交互），sys.task_done 后可"一键还原"。
- 主题跟随全局（项目统一主题管理，无需 WinUtil 式独立实现）。
- `src/layout/modules.ts` 注册 `sys` 模块（running 状态接 host_modules_status）。

### 8.2 client.ts API（类型化，18 个）

```ts
export const sysApi = {
  catalog: () => invoke<TweakItemDto[]>('sys_catalog'),
  scan: (cats?: TweakCategory[]) => invoke<TweakStatusDto[]>('sys_scan', { categories: cats }),
  apply: (ids: string[], includeRisky: boolean) => invoke<TaskReportDto>('sys_apply', { ids, includeRisky }),
  verify: (ids: string[]) => invoke<VerifyResultDto[]>('sys_verify', { ids }),
  revert: (backupId: string) => invoke<TaskReportDto>('sys_revert', { backupId }),
  revertTweak: (tweakId: string) => invoke<TaskReportDto>('sys_revert_tweak', { tweakId }),
  backups: (tweakId?: string) => invoke<BackupMetaDto[]>('sys_backups', { tweakId }),
  profiles: () => invoke<ProfileDto[]>('sys_profiles'),
  applyProfile: (id: string, includeRisky: boolean) => invoke<TaskReportDto>('sys_apply_profile', { id, includeRisky }),
  services: (filter?: string) => invoke<ServiceInfoDto[]>('sys_services', { filter }),
  serviceSetStart: (name: string, startType: StartType) => invoke<void>('sys_service_set_start', { name, startType }),
  tasks: (folder?: string) => invoke<TaskInfoDto[]>('sys_tasks', { folder }),
  taskSetEnabled: (path: string, enabled: boolean) => invoke<void>('sys_task_set_enabled', { path, enabled }),
  hostsGet: () => invoke<string>('sys_hosts_get'),
  hostsSave: (content: string) => invoke<void>('sys_hosts_save', { content }),
  dnsList: () => invoke<AdapterInfoDto[]>('sys_dns_list'),
  dnsSet: (adapter: string, servers: string[]) => invoke<void>('sys_dns_set', { adapter, servers }),
  helperStatus: () => invoke<HelperStateDto>('sys_helper_status'),
};
// 事件订阅：useNfEvent('sys.task_progress' | 'sys.task_done' | ... )
```

---

## 9. 安全 / 合规 / 审计

- **风险分级强约束**：High 条目 `include_risky=false` 时引擎直接拒绝（SYS_RISK_001），UI 二次确认只是第一道闸，引擎校验是第二道。
- 禁用安全更新 / Defender / 卸载系统组件：description 中强制包含后果说明文案（目录校验规则：risk=high 必须含"风险"字段说明，CatalogLoader 落实）。
- UAC：仅在首次需要提权时弹一次；`sys_helper_status` 常显于状态条，用户可感知提权进程存在；Helper 空闲自退。
- 审计：audit_log 全量记录（谁/何时/动了什么/结果），`sys_export_log` 导出 CSV/JSON（用户提案要求）。
- 合规红线（沿用项目既有决定）：只借鉴开源项目设计不复制代码（GPL/AGPL 项目 Windhawk、Seelen UI 仅参考思路）；参考项目调研清单与来源登记进 THIRD_PARTY_LICENSES.md 备查；WinOps 不内置任何规避激活/破解类条目。
- 权限声明：模块设置页展示所需权限清单（注册表写入 / 服务控制 / 计划任务 / 管理员提权 / 本地回环通信），对应 DESIGN 插件权限声明要求。

---

## 10. 步骤表与里程碑（W0–W7 对应用户提案阶段 0–7）

| # | 任务 | 依赖 | 出口物 |
|---|---|---|---|
| W0 | 基础架构：model/op/catalog/loader + BackupRepo(sys.db) + TweakEngine（mock Ports 全链路单测）+ Helper 骨架（spawn/管道/握手/白名单/ping） | S3 | sys-core crate + 30+ 单测全绿 |
| W1 | RegistryOps/HostsFile/DnsConfig 三 Port + helper registry/hosts/dns 方法 + UI：SysPanel 骨架（目录/搜索/详情/任务抽屉） | W0 | 目录首批条目（ui 低风险族）可应用/回滚 |
| W2 | ServiceCtl/TaskScheduler Port + COM 线程纪律 + UI：服务页（风险标注/收藏/启停）+ 任务页（批量启停） | W1 | 服务/任务家族 catalog 条目 |
| W3 | privacy 家族全部条目 + HostsPatch 组回滚 + profile 机制 | W2 | privacy 全绿 + standard profile |
| W4 | update 家族 + 回归检测（防自愈提示）+ clear_cache 组合 op | W3 | update 家族 + 回归告警链路 |
| W5 | AppxPort + apps 家族（预置清单/系统组件隔离） | W2 | apps 家族 |
| W6 | MaintenancePort（还原点/DISM/SFC 流式/内存清理/临时清理）+ network 收尾 | W5 | 维护与网络家族 |
| W7 | Defender 等高风险族 + 审计导出 + e2e 验收（HKCU 沙箱键全链路） | W6 | 1.0 范围内 WinOps 收口 |

验收闸（对应 06 阶段三验收增量）：
- [ ] HKCU 沙箱测试键全链路：scan→apply→verify→revert 后注册表与备份库状态与操作前逐字节一致
- [ ] apply 中途 kill -9 主进程：重启后 backups 有完整备份记录，revert 可恢复到操作前
- [ ] Helper 被强杀：下次操作自动重新 spawn（UAC 一次），主进程无崩溃
- [ ] 环检测：catalog 含依赖环 → CatalogLoader 整包拒绝并给出环路径
- [ ] High 条目在 include_risky=false 时引擎返回 SYS_RISK_001（UI 与引擎双闸验证）
- [ ] DISM 输出经事件流实时可见，任务期间 UI 不冻结；workspace 测试全绿 + tsc 0 错 + vite build 通过

---

## 11. 风险标注汇总

- **合规优先**：Defender/安全更新禁用条目必须显式风险文案 + 引擎双闸；不提供任何"关闭后隐藏来源"类混淆能力。
- **篡改保护**：Defender 开关可能被系统篡改保护拦截 → 如实返回 ACCESS_DENIED 并给出官方关闭篡改保护的指引，禁止 helper 绕过。
- **WUB 式对抗不可取**：不做守护进程/权限锁死对抗系统自愈（会造成更新系统组件损坏 + 杀软误报），以重扫回归提示代替（v1 边界，守护任务待 automation A4）。
- **COM 纪律**：taskschd 全部调用收敛到专用 MTA 线程（kvm 钩子线程同款手法：专用线程 + channel），禁止随手 CoInitialize。
- **Hosts 与 EDR**：写入被拦截时报错不重试；标记组必须成对校验（无 END 的悬挂 BEGIN 视为损坏，修复后再编辑）。
- **netsh/DNS**：adapter 名注入风险（换行/引号）→ 白名单字符集校验后才拼接。
- **UWP 卸载不可逆边界**：provisioned 移除后新用户不再预装 —— all_users=true 必须二次确认；Store 可重装提示常显。
- **进程内 / 提权路径分工固定**：HKLM 混入进程内直写是越权漏洞，exec_op 的 match 必须穷尽 review（新增 Op kind 时先改路由再写实现）。
- **幂等 verify**：verify 条件必须能在"策略生效但 UI 未刷新"场景下通过（读注册表/SCM 真源，不读 UI 感知源）。
