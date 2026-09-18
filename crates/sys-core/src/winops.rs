//! WinOps Tweak 引擎（docs/impl/08 W0–W4）：catalog 数据驱动 + BAVR 执行语义。
//!
//! - BAVR：Backup（先落库原值）→ Apply → Verify；verify 失败即补偿回滚（apply 内闭环）
//!   ——verify 之后的"回归重扫提示"是另一独立动作，不自动回滚（文档定稿）
//! - catalog：嵌入 JSON（编译期）+ 外置目录覆盖（同 id 覆盖、其余追加）——CrapFixer 式
//!   免编译扩展安全项；registry 动作 HKCU/HKLM 均可（W3 起提权 Helper 解锁 HKLM 写入）
//! - requires_admin：Service/Task/HKLM 动作需管理员——扫描三态（needs_admin）+ apply 引擎闸；
//!   非提权进程由命令层构造 helper-backed 数据面（RoutingRegistry：HKLM→helper）后解锁
//! - 数据面走 host-core Port（RegistryOps/TaskTogglePort/ServiceCtlPort；模块不直依 windows）

use std::path::Path;

use serde::{Deserialize, Serialize};

use host_core::ports::{AppxPort, MaintenancePort, RegValue, RegistryOps, ServiceCtlPort, StartType, TaskTogglePort};

use crate::error::{Result as SysResult, SysError};

/// 嵌入式目录（CrapFixer 式安全默认集；外置目录可覆盖/追加）
const EMBEDDED_CATALOG: &str = include_str!("winops/catalog.json");

// ---------------------------------------------------------------------------
// 模型（catalog JSON ↔ 结构体）
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tweak {
    pub id: String,
    pub name: String,
    pub category: String,
    #[serde(default)]
    pub description: String,
    /// 含需管理员动作（Service / 系统计划任务）——非提权进程扫描标 needs_admin、apply 拒绝
    #[serde(default)]
    pub requires_admin: bool,
    /// 维护型 tweak（clear_cache 等）：无"已应用"状态（scan 恒 NotApplied、verify 不做状态回读）
    #[serde(default)]
    pub maintenance: bool,
    pub actions: Vec<TweakAction>,
}

/// 服务启停动作（与 start_type 互斥使用；stop/start 为瞬态操作）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SvcAction {
    Stop,
    Start,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TweakAction {
    Registry {
        key: String,
        value_name: String,
        /// "dword" | "qword" | "string"（写入口径提示；读回比较按 RegValue 变体）
        value_type: String,
        data: RegValue,
    },
    /// 服务控制（需管理员；sc.exe 封装）——start_type 持久配置 / action 瞬态启停
    Service {
        name: String,
        #[serde(default)]
        start_type: Option<StartType>,
        #[serde(default)]
        action: Option<SvcAction>,
    },
    /// 计划任务启停（系统内置任务需管理员；schtasks /Change 封装）
    Task {
        path: String,
        enabled: bool,
    },
    /// 文件清理（维护型；SoftwareDistribution\Download 等需提权——helper 路径白名单硬限制）
    FileClean {
        path: String,
        #[serde(default = "default_recursive")]
        recursive: bool,
        #[serde(default = "default_skip_hours")]
        skip_recent_hours: u32,
    },
    /// Appx 包移除（all_users=false 当前用户本地移除；true provisioned 移除需管理员——helper）
    AppxRemove {
        name: String,
        #[serde(default)]
        all_users: bool,
    },
    /// 白名单程序执行（powercfg|dism|sfc|netsh|onedrive_uninstall——win-integration 层精确匹配）
    Exec {
        program: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default = "default_exec_timeout")]
        timeout_ms: u32,
    },
    /// 创建系统还原点（W6 维护；系统还原未开启如实报错）
    RestorePoint {
        #[serde(default)]
        description: String,
    },
    /// 清理各进程工作集（内存整理）
    EmptyWorkingSet {},
    /// Defender 实时保护开关（高风险——篡改保护拦截时如实报错）
    DefenderRealtime {
        disable: bool,
    },
    /// 预留：HostsPatch 等 W7+ 动作类型（catalog 出现即报"需更高版本"）
    Unsupported { kind: String },
}

fn default_recursive() -> bool {
    true
}

fn default_skip_hours() -> u32 {
    24
}

fn default_exec_timeout() -> u32 {
    30_000
}

/// 扫描三态（needs_admin 优先于状态判定：无权限时状态无意义）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanState {
    Applied,
    NotApplied,
    NeedsAdmin,
}

/// 备份条目（按动作类型区分恢复语义；serde tag 兼容 backup.json v2 格式）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackupItem {
    /// 注册表值（existed=false → 恢复为"不存在"）
    Registry(RegistryBackup),
    /// 服务状态（恢复 = 写回原 start_type + 原运行中则拉起；was_running 默认兼容 v2 旧记录）
    Service { name: String, start_type: StartType, #[serde(default)] was_running: bool },
    /// 计划任务（existed=false → 无法恢复，跳过）
    Task { path: String, existed: bool, enabled: bool },
    /// 文件清理（无需备份——恢复为空操作，维护型不可逆）
    FileClean,
    /// Appx 包（恢复为空操作——卸载后可从 Store 重装；was_installed 供回归检测）
    Appx { name: String, all_users: bool, was_installed: bool },
    /// Exec 执行（无原状备份——powercfg 等不可逆；占位条目）
    Exec,
    /// Defender 开关（无原状备份——恢复由对称 catalog 条目承担；占位条目）
    DefenderRealtime,
}

/// 单注册表值备份（existed 区分"原不存在"与"原为空"）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistryBackup {
    pub tweak_id: String,
    pub key: String,
    pub value_name: String,
    pub existed: bool,
    pub old_value: Option<RegValue>,
}

/// Port 聚合（registry 必备；tasks/services/maintenance/appx 随动作类型可选）
pub struct SysPorts<'a> {
    pub registry: &'a dyn RegistryOps,
    pub tasks: Option<&'a dyn TaskTogglePort>,
    pub services: Option<&'a dyn ServiceCtlPort>,
    pub maintenance: Option<&'a dyn MaintenancePort>,
    pub appx: Option<&'a dyn AppxPort>,
}

/// 应用报告
#[derive(Clone, Debug, Serialize)]
pub struct ApplyReport {
    pub tweak_id: String,
    pub backup: Vec<BackupItem>,
    pub verified: bool,
}

// ---------------------------------------------------------------------------
// catalog 加载
// ---------------------------------------------------------------------------

/// 解析目录 JSON（单个文件；顶层数组或 {"tweaks": [...]} 两种形态）
fn parse_catalog(raw: &str) -> SysResult<Vec<Tweak>> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| SysError::Catalog(format!("目录解析失败: {e}")))?;
    let arr = match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(o) => o
            .get("tweaks")
            .cloned()
            .and_then(|t| t.as_array().cloned())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    serde_json::from_value(serde_json::Value::Array(arr))
        .map_err(|e| SysError::Catalog(format!("目录解析失败: {e}")))
}

/// 合并目录：外置同 id 覆盖内置，其余追加（顺序：内置在前）
fn merge_tweaks(mut base: Vec<Tweak>, extra: Vec<Tweak>) -> Vec<Tweak> {
    for t in extra {
        match base.iter_mut().find(|b| b.id == t.id) {
            Some(slot) => *slot = t,
            None => base.push(t),
        }
    }
    base
}

/// 加载目录（external_dir = {appData}/winops/catalog，*.json 全部并入）
pub fn load_catalog(external_dir: Option<&Path>) -> SysResult<Vec<Tweak>> {
    let mut tweaks = parse_catalog(EMBEDDED_CATALOG)?;
    if let Some(dir) = external_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    match std::fs::read_to_string(&path)
                        .map_err(|er| SysError::Catalog(er.to_string()))
                        .and_then(|raw| parse_catalog(&raw))
                    {
                        Ok(extra) => tweaks = merge_tweaks(tweaks, extra),
                        Err(er) => {
                            // 外置目录损坏单文件跳过（不阻断内置目录）
                            tracing::warn!(path = %path.display(), error = %er, "外置目录文件损坏，跳过");
                        }
                    }
                }
            }
        }
    }
    Ok(tweaks)
}

// ---------------------------------------------------------------------------
// BAVR 执行
// ---------------------------------------------------------------------------

/// 单 action 当前是否已处于目标状态（查询出错按"未应用"处理——不阻断整表扫描）
fn action_applied(ports: &SysPorts, action: &TweakAction) -> SysResult<bool> {
    match action {
        TweakAction::Registry { key, value_name, data, .. } => match ports.registry.read_value(key, value_name) {
            Ok((v, existed)) => Ok(existed && v == *data),
            Err(e) => Err(SysError::Registry(e.to_string())),
        },
        TweakAction::Service { name, start_type, .. } => match ports.services {
            Some(svc) => match svc.query(name) {
                Ok(info) => Ok(start_type.is_some_and(|st| info.start_type == st)),
                Err(_) => Ok(false),
            },
            None => Ok(false), // 端口未注册（旧集成）——按未应用
        },
        TweakAction::Task { path, enabled } => match ports.tasks {
            Some(t) => match t.query_enabled(path) {
                Ok(Some(cur)) => Ok(cur == *enabled),
                Ok(None) => Ok(false), // 任务不存在
                Err(_) => Ok(false),
            },
            None => Ok(false),
        },
        // 瞬态/清理/无原状动作无持久目标状态（verify 走 maintenance 短路；防御性返回 true）
        TweakAction::FileClean { .. } => Ok(true),
        TweakAction::Exec { .. } | TweakAction::RestorePoint { .. } | TweakAction::EmptyWorkingSet {} | TweakAction::DefenderRealtime { .. } => Ok(true),
        // 当前用户移除：包不在 = 已应用；provisioned 移除无法廉价验证 → 防御性 true
        TweakAction::AppxRemove { name, all_users: false } => match ports.appx {
            Some(a) => match a.list(name) {
                Ok(pkgs) => Ok(pkgs.is_empty()),
                Err(_) => Ok(false),
            },
            None => Ok(false),
        },
        TweakAction::AppxRemove { all_users: true, .. } => Ok(true),
        TweakAction::Unsupported { kind } => Err(SysError::Catalog(format!("动作类型 {kind} 需更高版本支持"))),
    }
}

/// 扫描状态（三态：requires_admin 且非管理员 → needs_admin，不做状态查询；
/// 维护型 tweak 无"已应用"概念 → 恒 NotApplied，可重复执行）
pub fn scan(ports: &SysPorts, tweaks: &[Tweak], is_admin: bool) -> Vec<(Tweak, ScanState)> {
    tweaks
        .iter()
        .map(|t| {
            let state = if t.requires_admin && !is_admin {
                ScanState::NeedsAdmin
            } else if t.maintenance {
                ScanState::NotApplied
            } else {
                let applied = t.actions.iter().all(|a| action_applied(ports, a).unwrap_or(false));
                if applied { ScanState::Applied } else { ScanState::NotApplied }
            };
            (t.clone(), state)
        })
        .collect()
}

/// 备份单 action 原值
fn backup_action(ports: &SysPorts, tweak_id: &str, action: &TweakAction) -> SysResult<BackupItem> {
    match action {
        TweakAction::Registry { key, value_name, .. } => {
            let (old, existed) = ports
                .registry
                .read_value(key, value_name)
                .map_err(|e| SysError::Registry(e.to_string()))?;
            Ok(BackupItem::Registry(RegistryBackup {
                tweak_id: tweak_id.to_string(),
                key: key.clone(),
                value_name: value_name.clone(),
                existed,
                old_value: if existed { Some(old) } else { None },
            }))
        }
        TweakAction::Service { name, .. } => {
            let svc = ports.services.ok_or_else(|| SysError::Apply("服务端口未注册（无法备份服务状态）".into()))?;
            let info = svc
                .query(name)
                .map_err(|e| SysError::Apply(format!("服务 {name} 备份失败: {e}")))?;
            Ok(BackupItem::Service { name: name.clone(), start_type: info.start_type, was_running: info.running })
        }
        TweakAction::Task { path, .. } => {
            let t = ports.tasks.ok_or_else(|| SysError::Apply("计划任务端口未注册（无法备份任务状态）".into()))?;
            let enabled = t
                .query_enabled(path)
                .map_err(|e| SysError::Apply(format!("任务 {path} 备份失败: {e}")))?
                .ok_or_else(|| SysError::Apply(format!("任务 {path} 不存在")))?;
            Ok(BackupItem::Task { path: path.clone(), existed: true, enabled })
        }
        // 清理动作不可逆（reversible=false）——占位条目，恢复为空操作
        TweakAction::FileClean { .. } => Ok(BackupItem::FileClean),
        // Appx：记录"原已安装"（恢复 = Store 重装提示，不自动重装）
        TweakAction::AppxRemove { name, all_users } => {
            let was_installed = ports
                .appx
                .map(|a| a.list(name).map(|pkgs| !pkgs.is_empty()).unwrap_or(false))
                .unwrap_or(false);
            Ok(BackupItem::Appx { name: name.clone(), all_users: *all_users, was_installed })
        }
        TweakAction::Unsupported { kind } => Err(SysError::Catalog(format!("动作类型 {kind} 需更高版本支持"))),
        // Exec/还原点/内存清理/Defender：无原状可备份（不可逆）——占位条目
        TweakAction::Exec { .. } | TweakAction::RestorePoint { .. } | TweakAction::EmptyWorkingSet {} | TweakAction::DefenderRealtime { .. } => Ok(BackupItem::Exec),
    }
}

/// 应用单个 action（写目标值）
fn apply_action(ports: &SysPorts, action: &TweakAction) -> SysResult<()> {
    match action {
        TweakAction::Registry { key, value_name, data, .. } => ports
            .registry
            .write_value(key, value_name, data)
            .map_err(|e| SysError::Registry(e.to_string())),
        TweakAction::Service { name, start_type, action } => {
            let svc = ports.services.ok_or_else(|| SysError::Apply("服务端口未注册".into()))?;
            if let Some(st) = start_type {
                svc.set_start_type(name, *st).map_err(|e| SysError::Apply(format!("服务 {name} 配置失败: {e}")))?;
            }
            match action {
                Some(SvcAction::Stop) => {
                    svc.stop(name).map_err(|e| SysError::Apply(format!("服务 {name} 停止失败: {e}")))
                }
                Some(SvcAction::Start) => {
                    svc.start(name).map_err(|e| SysError::Apply(format!("服务 {name} 启动失败: {e}")))
                }
                None => Ok(()),
            }
        }
        TweakAction::Task { path, enabled } => {
            let t = ports.tasks.ok_or_else(|| SysError::Apply("计划任务端口未注册".into()))?;
            t.set_enabled(path, *enabled).map_err(|e| SysError::Apply(format!("任务 {path} 启停失败: {e}")))
        }
        TweakAction::FileClean { path, recursive, skip_recent_hours } => {
            let m = ports.maintenance.ok_or_else(|| SysError::Apply("维护端口未注册（无法清理文件）".into()))?;
            m.clean_dir(path, *recursive, *skip_recent_hours)
                .map_err(|e| SysError::Apply(format!("清理 {path} 失败: {e}")))?;
            Ok(())
        }
        TweakAction::AppxRemove { name, all_users } => {
            let a = ports.appx.ok_or_else(|| SysError::Apply("Appx 端口未注册".into()))?;
            let n = if *all_users {
                a.remove_provisioned(name)
                    .map_err(|e| SysError::Apply(format!("移除 provisioned 包 {name} 失败: {e}")))?
            } else {
                a.remove_current_user(name)
                    .map_err(|e| SysError::Apply(format!("移除包 {name} 失败: {e}")))?
            };
            if n == 0 {
                // 未匹配任何包：可能已卸载（幂等成功），也可能包名不存在——报告语义不变
                tracing::warn!(name = %name, "Appx 移除未匹配到包（可能已卸载）");
            }
            Ok(())
        }
        TweakAction::Exec { program, args, timeout_ms } => {
            let m = ports.maintenance.ok_or_else(|| SysError::Apply("维护端口未注册".into()))?;
            m.exec(program, args, *timeout_ms)
                .map_err(|e| SysError::Apply(format!("执行 {program} 失败: {e}")))?;
            Ok(())
        }
        TweakAction::RestorePoint { description } => {
            let m = ports.maintenance.ok_or_else(|| SysError::Apply("维护端口未注册".into()))?;
            m.restore_point(description)
                .map_err(|e| SysError::Apply(format!("创建还原点失败: {e}")))
        }
        TweakAction::EmptyWorkingSet {} => {
            let m = ports.maintenance.ok_or_else(|| SysError::Apply("维护端口未注册".into()))?;
            m.empty_working_set()
                .map_err(|e| SysError::Apply(format!("内存清理失败: {e}")))?;
            Ok(())
        }
        TweakAction::DefenderRealtime { disable } => {
            let m = ports.maintenance.ok_or_else(|| SysError::Apply("维护端口未注册".into()))?;
            m.defender_realtime(*disable)
                .map_err(|e| SysError::Apply(format!("Defender 实时保护开关失败: {e}")))
        }
        TweakAction::Unsupported { kind } => Err(SysError::Catalog(format!("动作类型 {kind} 需更高版本支持"))),
    }
}

/// 恢复备份（rollback / 补偿共用；按动作类型各自恢复语义）
pub fn restore_backup(ports: &SysPorts, backup: &[BackupItem]) -> SysResult<()> {
    for b in backup {
        match b {
            BackupItem::Registry(rb) => {
                if rb.existed {
                    if let Some(v) = &rb.old_value {
                        ports
                            .registry
                            .write_value(&rb.key, &rb.value_name, v)
                            .map_err(|e| SysError::Registry(e.to_string()))?;
                    }
                } else {
                    // 原值不存在 → 恢复为"不存在"（删除失败容忍：值可能已被外部删除）
                    let _ = ports.registry.delete_value(&rb.key, &rb.value_name);
                }
            }
            BackupItem::Service { name, start_type, was_running } => {
                if let Some(svc) = ports.services {
                    svc.set_start_type(name, *start_type)
                        .map_err(|e| SysError::Apply(format!("服务 {name} 恢复失败: {e}")))?;
                    // 原运行中 → 拉起（start 内部处理 Disabled 跳过，best-effort 不阻断后续恢复）
                    if *was_running {
                        let _ = svc.start(name);
                    }
                }
            }
            BackupItem::Task { path, existed, enabled } => {
                // 不存在的任务无法恢复（跳过）；存在则恢复原启停态
                if *existed {
                    if let Some(t) = ports.tasks {
                        t.set_enabled(path, *enabled)
                            .map_err(|e| SysError::Apply(format!("任务 {path} 恢复失败: {e}")))?;
                    }
                }
            }
            // 清理动作不可逆——恢复为空操作（回滚仅还原同 tweak 的服务/注册表条目）
            BackupItem::FileClean => {}
            // Exec 不可逆——恢复为空操作
            BackupItem::Exec => {}
            // Defender 开关——恢复由对称 catalog 条目承担
            BackupItem::DefenderRealtime => {}
            // Appx 卸载不可自动恢复——提示可从 Store 重装（v1 不自动下载安装）
            BackupItem::Appx { name, was_installed, .. } => {
                if *was_installed {
                    tracing::info!(name = %name, "Appx 包已卸载；如需恢复可从 Microsoft Store 重装");
                }
            }
        }
    }
    Ok(())
}

/// BAVR 应用（docs/impl/08 W0）：
/// 1. Backup：全部 action 原值先落快照（任一备份失败即中止，未动系统）
/// 2. Apply：逐 action 写入
/// 3. Verify：逐 action 读回比较
/// 4. Rollback：verify 失败即用备份补偿（回滚也失败 → 报告携带回滚错误）
///
/// requires_admin 且非管理员 → 直接拒绝（引擎闸；UI 徽章是第一道）
pub fn apply(ports: &SysPorts, tweak: &Tweak, is_admin: bool) -> SysResult<ApplyReport> {
    if tweak.requires_admin && !is_admin {
        return Err(SysError::Apply(format!(
            "Tweak {} 需要管理员权限（提权 Helper 未启动或授权失败）",
            tweak.id
        )));
    }
    // 1. 备份先行（全量成功才进入 apply）
    let mut backup = Vec::new();
    for a in &tweak.actions {
        backup.push(backup_action(ports, &tweak.id, a)?);
    }
    // 2. 应用
    let mut applied_errors: Vec<String> = Vec::new();
    for a in &tweak.actions {
        if let Err(e) = apply_action(ports, a) {
            applied_errors.push(e.to_string());
        }
    }
    // 3. 校验（维护型不做状态回读——clear_cache 等无持久目标状态；动作失败已并入 errors）
    let verified =
        applied_errors.is_empty() && (tweak.maintenance || tweak.actions.iter().all(|a| action_applied(ports, a).unwrap_or(false)));
    if verified {
        return Ok(ApplyReport { tweak_id: tweak.id.clone(), backup, verified: true });
    }
    // 4. 失败补偿（回滚全部备份；回滚错误并入报告）
    if let Err(re) = restore_backup(ports, &backup) {
        applied_errors.push(format!("补偿回滚失败: {re}"));
    }
    Err(SysError::Apply(format!(
        "应用未通过校验（已回滚）: {}",
        applied_errors.join("; ")
    )))
}

/// 回归检测（docs/impl/08 §3.3 WUB 式防自愈，v1 边界：不做守护任务，模块 start 时比对一次）：
/// 备份里的"原值"即系统自愈后的样子——当前值回到原值 = 被系统/外部改回。
/// 返回回归的 tweak_id 列表（查询失败/端口缺失不误报）。
pub fn regression_check(ports: &SysPorts, store: &BackupStore, tweaks: &[Tweak]) -> Vec<String> {
    let mut regressed: Vec<String> = Vec::new();
    for set in store.load_all() {
        // 目录中不存在或维护型（clear_cache 无回归概念）跳过
        let Some(t) = tweaks.iter().find(|t| t.id == set.tweak_id) else { continue };
        if t.maintenance || regressed.contains(&set.tweak_id) {
            continue;
        }
        for b in &set.items {
            let hit = match b {
                BackupItem::Registry(rb) => match ports.registry.read_value(&rb.key, &rb.value_name) {
                    Ok((cur, existed)) => {
                        if rb.existed {
                            // 当前值回到备份原值，或写入的目标值消失 → 均为回归
                            (existed && Some(&cur) == rb.old_value.as_ref()) || !existed
                        } else {
                            // 我们创建的值被清掉 = 回归
                            !existed
                        }
                    }
                    Err(_) => false,
                },
                BackupItem::Service { name, start_type, .. } => match ports.services {
                    Some(svc) => svc.query(name).map(|i| i.start_type == *start_type).unwrap_or(false),
                    None => false,
                },
                BackupItem::Task { path, existed, enabled } => match ports.tasks {
                    Some(t) => match t.query_enabled(path) {
                        Ok(Some(cur)) => *existed && cur == *enabled,
                        Ok(None) => *existed, // 任务被删 = 回归
                        Err(_) => false,
                    },
                    None => false,
                },
                BackupItem::FileClean => false,
                BackupItem::Exec => false,
                BackupItem::DefenderRealtime => false,
                // 原已安装的包又被系统/Store 装回 = 回归
                BackupItem::Appx { name, was_installed, .. } => match ports.appx {
                    Some(a) => *was_installed && a.list(name).map(|p| !p.is_empty()).unwrap_or(false),
                    None => false,
                },
            };
            if hit {
                regressed.push(set.tweak_id.clone());
                break;
            }
        }
    }
    regressed
}

/// 备份落库（{appData}/winops/backup.json：每 tweak 保留最近一次 apply 的原值快照）
pub struct BackupStore {
    path: std::path::PathBuf,
}

/// 一份 apply 的完整备份（tweak_id → items 二层；Service/Task 条目归属由 set 承载）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupSet {
    pub tweak_id: String,
    pub items: Vec<BackupItem>,
}

impl BackupStore {
    pub fn open(app_data_dir: &Path) -> Self {
        Self { path: app_data_dir.join("winops").join("backup.json") }
    }

    fn load_all(&self) -> Vec<BackupSet> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// 保存 apply 备份（同 tweak 覆盖——只保留最近一次，回滚到"最近一次应用前"）
    pub fn save(&self, report: &ApplyReport) -> SysResult<()> {
        let mut all: Vec<BackupSet> = self
            .load_all()
            .into_iter()
            .filter(|s| s.tweak_id != report.tweak_id)
            .collect();
        all.push(BackupSet { tweak_id: report.tweak_id.clone(), items: report.backup.clone() });
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(SysError::Io)?;
        }
        let data = serde_json::to_vec_pretty(&all)
            .map_err(|e| SysError::Catalog(format!("备份序列化失败: {e}")))?;
        std::fs::write(&self.path, data).map_err(SysError::Io)
    }

    /// 取指定 tweak 的备份（回滚用；无备份返回空）
    pub fn take(&self, tweak_id: &str) -> Vec<BackupItem> {
        self.load_all()
            .into_iter()
            .find(|s| s.tweak_id == tweak_id)
            .map(|s| s.items)
            .unwrap_or_default()
    }

    /// 备份数据集存在与否（UI：可否回滚）
    pub fn has_backup(&self, tweak_id: &str) -> bool {
        self.load_all().iter().any(|s| s.tweak_id == tweak_id)
    }

    /// 移除指定 tweak 的备份（回滚完成后调用——已还原的状态不再参与回归检测，
    /// 否则"当前值 == 备份原值"会被误判为系统自愈）
    pub fn remove(&self, tweak_id: &str) {
        let all: Vec<BackupSet> = self.load_all().into_iter().filter(|s| s.tweak_id != tweak_id).collect();
        if let Ok(data) = serde_json::to_vec_pretty(&all) {
            let _ = std::fs::write(&self.path, data);
        }
    }
}

// ---------------------------------------------------------------------------
// 审计（docs/impl/08 §2.4/§9 W7：JSONL 追加 + 导出；helper 不落盘，审计由主进程承担）
// ---------------------------------------------------------------------------

/// 单条审计记录（audit.jsonl 每行一个 JSON 对象；只追加不修改）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    /// 毫秒时间戳
    pub ts_ms: i64,
    /// 触发者（"ui" | "profile:xxx" | "plugin:xxx"）
    pub actor: String,
    pub tweak_id: String,
    /// apply | rollback
    pub action: String,
    /// apply 时的 verify 结果（rollback 为 None）
    #[serde(default)]
    pub verified: Option<bool>,
    /// 附加信息（错误摘要/备份条目数）
    #[serde(default)]
    pub detail: String,
}

/// 审计存储（{appData}/winops/audit.jsonl 追加写；导出到 exports/ 子目录）
pub struct AuditStore {
    path: std::path::PathBuf,
}

impl AuditStore {
    pub fn open(app_data_dir: &Path) -> Self {
        Self { path: app_data_dir.join("winops").join("audit.jsonl") }
    }

    /// 追加一条审计（写失败仅告警不阻断主流程——审计不比系统修改更重要）
    pub fn record(&self, actor: &str, tweak_id: &str, action: &str, verified: Option<bool>, detail: &str) {
        let entry = AuditEntry {
            ts_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            actor: actor.to_string(),
            tweak_id: tweak_id.to_string(),
            action: action.to_string(),
            verified,
            detail: detail.to_string(),
        };
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut line) = serde_json::to_string(&entry) {
            line.push('\n');
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
                let _ = f.write_all(line.as_bytes());
            } else {
                tracing::warn!("审计写入失败（不阻断主流程）");
            }
        }
    }

    /// 全量审计条目（导出/展示用）
    pub fn entries(&self) -> Vec<AuditEntry> {
        std::fs::read_to_string(&self.path)
            .map(|raw| {
                raw.lines()
                    .filter_map(|l| serde_json::from_str::<AuditEntry>(l).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 导出审计 + 当前备份清单到 {appData}/winops/exports/，返回导出文件路径
    pub fn export(&self, app_data_dir: &Path) -> SysResult<std::path::PathBuf> {
        #[derive(Serialize)]
        struct ExportDoc {
            exported_at_ms: i64,
            audit: Vec<AuditEntry>,
            backups: Vec<BackupSet>,
        }
        let backup_store = BackupStore::open(app_data_dir);
        let doc = ExportDoc {
            exported_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            audit: self.entries(),
            backups: backup_store.load_all(),
        };
        let dir = app_data_dir.join("winops").join("exports");
        std::fs::create_dir_all(&dir).map_err(SysError::Io)?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path = dir.join(format!("winops-audit-{ts}.json"));
        let data = serde_json::to_vec_pretty(&doc).map_err(|e| SysError::Catalog(format!("导出序列化失败: {e}")))?;
        std::fs::write(&path, data).map_err(SysError::Io)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::ports::ServiceInfo;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// 内存注册表（BAVR 语义测试）
    #[derive(Default)]
    struct FakeRegistry {
        values: Mutex<HashMap<(String, String), RegValue>>,
    }
    impl FakeRegistry {
        fn set(&self, key: &str, name: &str, v: RegValue) {
            self.values
                .lock()
                .unwrap()
                .insert((key.to_string(), name.to_string()), v);
        }
    }
    impl RegistryOps for FakeRegistry {
        fn read_value(&self, key: &str, name: &str) -> std::result::Result<(RegValue, bool), host_core::error::AppError> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(&(key.to_string(), name.to_string()))
                .cloned()
                .map(|v| (v, true))
                .unwrap_or((RegValue::Dword(0), false)))
        }
        fn write_value(&self, key: &str, name: &str, value: &RegValue) -> std::result::Result<(), host_core::error::AppError> {
            self.values
                .lock()
                .unwrap()
                .insert((key.to_string(), name.to_string()), value.clone());
            Ok(())
        }
        fn delete_value(&self, key: &str, name: &str) -> std::result::Result<(), host_core::error::AppError> {
            self.values.lock().unwrap().remove(&(key.to_string(), name.to_string()));
            Ok(())
        }
    }

    /// 内存服务表（Service 动作测试：启动类型 + 运行态）
    #[derive(Default)]
    struct FakeServices {
        services: Mutex<HashMap<String, (StartType, bool)>>,
    }
    impl FakeServices {
        fn set(&self, name: &str, st: StartType, running: bool) {
            self.services.lock().unwrap().insert(name.to_string(), (st, running));
        }
    }
    impl ServiceCtlPort for FakeServices {
        fn query(&self, name: &str) -> std::result::Result<ServiceInfo, host_core::error::AppError> {
            let m = self.services.lock().unwrap();
            m.get(name)
                .map(|&(st, running)| ServiceInfo { name: name.into(), start_type: st, running })
                .ok_or_else(|| host_core::error::AppError::module("T", format!("服务 {name} 不存在"), None))
        }
        fn set_start_type(&self, name: &str, st: StartType) -> std::result::Result<(), host_core::error::AppError> {
            let mut m = self.services.lock().unwrap();
            let e = m.get_mut(name).ok_or_else(|| host_core::error::AppError::module("T", format!("服务 {name} 不存在"), None))?;
            e.0 = st;
            Ok(())
        }
        fn stop(&self, name: &str) -> std::result::Result<(), host_core::error::AppError> {
            let mut m = self.services.lock().unwrap();
            let e = m.get_mut(name).ok_or_else(|| host_core::error::AppError::module("T", format!("服务 {name} 不存在"), None))?;
            e.1 = false;
            Ok(())
        }
        fn start(&self, name: &str) -> std::result::Result<(), host_core::error::AppError> {
            let mut m = self.services.lock().unwrap();
            let e = m.get_mut(name).ok_or_else(|| host_core::error::AppError::module("T", format!("服务 {name} 不存在"), None))?;
            // Disabled 服务启动跳过（幂等语义）
            if e.0 != StartType::Disabled {
                e.1 = true;
            }
            Ok(())
        }
    }

    /// 内存任务表（Task 动作测试）
    #[derive(Default)]
    struct FakeTasks {
        tasks: Mutex<HashMap<String, bool>>,
    }
    impl TaskTogglePort for FakeTasks {
        fn query_enabled(&self, path: &str) -> std::result::Result<Option<bool>, host_core::error::AppError> {
            Ok(self.tasks.lock().unwrap().get(path).copied())
        }
        fn set_enabled(&self, path: &str, enabled: bool) -> std::result::Result<(), host_core::error::AppError> {
            let mut m = self.tasks.lock().unwrap();
            if m.contains_key(path) {
                m.insert(path.to_string(), enabled);
                Ok(())
            } else {
                Err(host_core::error::AppError::module("T", format!("任务 {path} 不存在"), None))
            }
        }
    }

    /// 内存维护表（FileClean/Exec/还原点/内存清理：记录调用参数）
    #[derive(Default)]
    struct FakeMaintenance {
        calls: Mutex<Vec<(String, bool, u32)>>,
        execs: Mutex<Vec<String>>,
        restore_points: Mutex<Vec<String>>,
        working_sets: Mutex<u32>,
    }
    impl MaintenancePort for FakeMaintenance {
        fn clean_dir(&self, path: &str, recursive: bool, skip_recent_hours: u32) -> std::result::Result<u32, host_core::error::AppError> {
            self.calls.lock().unwrap().push((path.to_string(), recursive, skip_recent_hours));
            Ok(3)
        }
        fn exec(&self, program: &str, args: &[String], timeout_ms: u32) -> std::result::Result<String, host_core::error::AppError> {
            self.execs.lock().unwrap().push(format!("{program} {args:?} {timeout_ms}"));
            Ok("ok".into())
        }
        fn restore_point(&self, description: &str) -> std::result::Result<(), host_core::error::AppError> {
            self.restore_points.lock().unwrap().push(description.to_string());
            Ok(())
        }
        fn empty_working_set(&self) -> std::result::Result<u32, host_core::error::AppError> {
            *self.working_sets.lock().unwrap() += 1;
            Ok(5)
        }
        fn repair(&self, kind: host_core::ports::RepairKind) -> std::result::Result<String, host_core::error::AppError> {
            self.execs.lock().unwrap().push(format!("repair:{kind:?}"));
            Ok("ok".into())
        }
        fn defender_realtime(&self, disable: bool) -> std::result::Result<(), host_core::error::AppError> {
            self.execs.lock().unwrap().push(format!("defender_realtime:{disable}"));
            Ok(())
        }
    }

    /// 内存 Appx 表（记录移除调用；installed = 当前"已安装"的包名集合）
    #[derive(Default)]
    struct FakeAppx {
        installed: Mutex<Vec<String>>,
        removed_current: Mutex<Vec<String>>,
        removed_provisioned: Mutex<Vec<String>>,
    }
    impl FakeAppx {
        fn install(&self, name: &str) {
            self.installed.lock().unwrap().push(name.to_string());
        }
    }
    impl AppxPort for FakeAppx {
        fn list(&self, name_filter: &str) -> std::result::Result<Vec<host_core::ports::AppxPackage>, host_core::error::AppError> {
            let m = self.installed.lock().unwrap();
            Ok(m.iter()
                .filter(|n| n.starts_with(name_filter))
                .map(|n| host_core::ports::AppxPackage { name: n.clone(), full_name: format!("{n}.1.0.0_x64__zz") })
                .collect())
        }
        fn remove_current_user(&self, name_filter: &str) -> std::result::Result<u32, host_core::error::AppError> {
            let mut m = self.installed.lock().unwrap();
            let before = m.len();
            m.retain(|n| !n.starts_with(name_filter));
            let n = (before - m.len()) as u32;
            self.removed_current.lock().unwrap().push(name_filter.to_string());
            Ok(n)
        }
        fn remove_provisioned(&self, name_filter: &str) -> std::result::Result<u32, host_core::error::AppError> {
            self.removed_provisioned.lock().unwrap().push(name_filter.to_string());
            Ok(1)
        }
    }

    /// 端口聚合构造（全部 mock）
    struct FakePorts {
        reg: FakeRegistry,
        svc: FakeServices,
        task: FakeTasks,
        maint: FakeMaintenance,
        appx: FakeAppx,
    }
    impl FakePorts {
        fn new() -> Self {
            Self {
                reg: FakeRegistry::default(),
                svc: FakeServices::default(),
                task: FakeTasks::default(),
                maint: FakeMaintenance::default(),
                appx: FakeAppx::default(),
            }
        }
        fn ports(&self) -> SysPorts<'_> {
            SysPorts {
                registry: &self.reg,
                tasks: Some(&self.task),
                services: Some(&self.svc),
                maintenance: Some(&self.maint),
                appx: Some(&self.appx),
            }
        }
    }

    const K: &str = r"HKCU\Software\Test";

    fn tweak(id: &str, name: &str, val: u32) -> Tweak {
        Tweak {
            id: id.into(),
            name: name.into(),
            category: "test".into(),
            description: String::new(),
            requires_admin: false,
            maintenance: false,
            actions: vec![TweakAction::Registry {
                key: K.into(),
                value_name: format!("{id}_v"),
                value_type: "dword".into(),
                data: RegValue::Dword(val),
            }],
        }
    }

    #[test]
    fn catalog_embedded_loads() {
        let tweaks = load_catalog(None).unwrap();
        assert!(tweaks.len() >= 8);
        // HKCU registry 动作不要求提权；HKLM registry 动作必须标 requires_admin（W3 提权 Helper 解锁）
        // Service/Task 动作一律 requires_admin（系统级资源）
        assert!(tweaks.iter().all(|t| t.actions.iter().all(|a| match a {
            TweakAction::Registry { key, .. } => {
                if key.starts_with("HKLM") { t.requires_admin } else { true }
            }
            TweakAction::Service { .. } | TweakAction::Task { .. } => t.requires_admin,
            // 清理动作仅出现于维护型 tweak（requires_admin 由 helper 路径约束兜底）
            TweakAction::FileClean { .. } => t.requires_admin && t.maintenance,
            // Appx 移除：provisioned（all_users）需提权；当前用户移除不要求但 catalog 统一标注
            TweakAction::AppxRemove { all_users, .. } => {
                if *all_users { t.requires_admin } else { true }
            }
            // Exec/还原点/内存清理：系统级动作一律 requires_admin
            TweakAction::Exec { .. } | TweakAction::RestorePoint { .. } | TweakAction::EmptyWorkingSet {} => t.requires_admin,
            TweakAction::DefenderRealtime { .. } => t.requires_admin,
            TweakAction::Unsupported { .. } => false,
        })), "HKLM registry 动作与 Service/Task/FileClean 动作必须标 requires_admin");
    }

    #[test]
    fn catalog_external_overrides_by_id() {
        let dir = std::env::temp_dir().join(format!("nf_winops_cat_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("extra.json"),
            r#"[{"id":"taskbar_hide_widgets","name":"覆盖版","category":"taskbar","actions":[]},
                {"id":"brand_new","name":"新增","category":"x","actions":[]}]"#,
        )
        .unwrap();
        let tweaks = load_catalog(Some(&dir)).unwrap();
        let w = tweaks.iter().find(|t| t.id == "taskbar_hide_widgets").unwrap();
        assert_eq!(w.name, "覆盖版");
        assert!(tweaks.iter().any(|t| t.id == "brand_new"));
        // 损坏文件跳过不阻断
        std::fs::write(dir.join("bad.json"), "not json").unwrap();
        assert!(load_catalog(Some(&dir)).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_reports_applied_state() {
        let fp = FakePorts::new();
        let ports = fp.ports();
        let t = tweak("t1", "测试", 7);
        let st = scan(&ports, &[t.clone()], false);
        assert_eq!(st[0].1, ScanState::NotApplied);
        fp.reg.set(K, "t1_v", RegValue::Dword(7));
        let st = scan(&ports, &[t], false);
        assert_eq!(st[0].1, ScanState::Applied);
    }

    /// 三态：requires_admin 且非管理员 → needs_admin（不做状态查询）；管理员 → 正常判定
    #[test]
    fn scan_needs_admin_gate() {
        let fp = FakePorts::new();
        let ports = fp.ports();
        let mut t = tweak("admin1", "需管理员", 7);
        t.requires_admin = true;
        let st = scan(&ports, &[t.clone()], false);
        assert_eq!(st[0].1, ScanState::NeedsAdmin);
        let st = scan(&ports, &[t], true);
        assert_eq!(st[0].1, ScanState::NotApplied);
    }

    #[test]
    fn apply_bavr_success_and_backup_payload() {
        let fp = FakePorts::new();
        fp.reg.set(K, "t1_v", RegValue::Dword(1)); // 原值存在
        let ports = fp.ports();
        let t = tweak("t1", "测试", 7);
        let report = apply(&ports, &t, false).unwrap();
        assert!(report.verified);
        assert_eq!(report.backup.len(), 1);
        match &report.backup[0] {
            BackupItem::Registry(rb) => {
                assert!(rb.existed);
                assert_eq!(rb.old_value, Some(RegValue::Dword(1)));
            }
            _ => panic!("应为 Registry 备份"),
        }
        assert_eq!(scan(&ports, &[t], false)[0].1, ScanState::Applied);
    }

    #[test]
    fn apply_restores_when_verify_fails() {
        // 构造写后读不匹配：FakeRegistry 正常写读必匹配 → 用"备份阶段失败"触发补偿路径
        // 改用可拒绝写入的包装：写入后值被外部改回 → verify 失败 → 回滚原值
        // Flaky 拥有内部 FakeRegistry（Port trait 要求 'static，不能借用局部）
        struct Flaky {
            inner: FakeRegistry,
        }
        impl RegistryOps for Flaky {
            fn read_value(&self, k: &str, n: &str) -> std::result::Result<(RegValue, bool), host_core::error::AppError> {
                self.inner.read_value(k, n)
            }
            fn write_value(&self, k: &str, n: &str, v: &RegValue) -> std::result::Result<(), host_core::error::AppError> {
                self.inner.write_value(k, n, v)?;
                // 模拟"目标写入被外部还原"：仅当写入值 == 应用目标(7) 时被改写；
                // 补偿回滚写原值(1) 不受影响 → 原值最终恢复
                if *v == RegValue::Dword(7) {
                    self.inner.set(k, n, RegValue::Dword(0xFFFF));
                }
                Ok(())
            }
            fn delete_value(&self, k: &str, n: &str) -> std::result::Result<(), host_core::error::AppError> {
                self.inner.delete_value(k, n)
            }
        }
        let fp = FakePorts::new();
        let mut flaky = Flaky { inner: FakeRegistry::default() };
        flaky.inner.set(K, "t1_v", RegValue::Dword(1));
        let ports = SysPorts {
            registry: &flaky,
            tasks: Some(&fp.task),
            services: Some(&fp.svc),
            maintenance: Some(&fp.maint),
            appx: Some(&fp.appx),
        };
        let t = tweak("t1", "测试", 7);
        let err = apply(&ports, &t, false).unwrap_err();
        assert!(err.to_string().contains("已回滚"));
        // 原值被补偿恢复
        let (v, existed) = flaky.inner.read_value(K, "t1_v").unwrap();
        assert!(existed && v == RegValue::Dword(1));
    }

    #[test]
    fn restore_deletes_when_originally_absent() {
        let fp = FakePorts::new();
        let ports = fp.ports();
        let t = tweak("t2", "测试", 3);
        let report = apply(&ports, &t, false).unwrap();
        match &report.backup[0] {
            BackupItem::Registry(rb) => assert!(!rb.existed),
            _ => panic!("应为 Registry 备份"),
        }
        // 回滚 → 值应被删除（恢复"不存在"）
        restore_backup(&ports, &report.backup).unwrap();
        let (_, existed) = fp.reg.read_value(K, "t2_v").unwrap();
        assert!(!existed);
    }

    /// Service 动作 BAVR：备份原 start_type → 改 disabled → 恢复
    #[test]
    fn service_action_bavr_roundtrip() {
        let fp = FakePorts::new();
        fp.svc.set("DiagTrack", StartType::Auto, false);
        let ports = fp.ports();
        let t = Tweak {
            id: "svc_off".into(),
            name: "禁服务".into(),
            category: "test".into(),
            description: String::new(),
            requires_admin: true,
            maintenance: false,
            actions: vec![TweakAction::Service {
                name: "DiagTrack".into(),
                start_type: Some(StartType::Disabled),
                action: None,
            }],
        };
        // 引擎闸：非管理员 → 拒绝
        assert!(apply(&ports, &t, false).is_err());
        // 管理员 → 成功
        let report = apply(&ports, &t, true).unwrap();
        assert!(report.verified);
        match &report.backup[0] {
            BackupItem::Service { name, start_type, was_running } => {
                assert_eq!(name, "DiagTrack");
                assert_eq!(*start_type, StartType::Auto);
                assert!(!was_running);
            }
            _ => panic!("应为 Service 备份"),
        }
        // 回滚 → 原 Auto 恢复
        restore_backup(&ports, &report.backup).unwrap();
        assert_eq!(fp.svc.query("DiagTrack").unwrap().start_type, StartType::Auto);
    }

    /// Task 动作 BAVR：备份原启停 → 禁用 → 恢复；任务不存在 → 备份失败中止
    #[test]
    fn task_action_bavr_roundtrip_and_missing() {
        let fp = FakePorts::new();
        let path = r"\Microsoft\Windows\Test\Sample";
        fp.task.tasks.lock().unwrap().insert(path.into(), true);
        let ports = fp.ports();
        let t = Tweak {
            id: "task_off".into(),
            name: "禁任务".into(),
            category: "test".into(),
            description: String::new(),
            requires_admin: true,
            maintenance: false,
            actions: vec![TweakAction::Task { path: path.into(), enabled: false }],
        };
        let report = apply(&ports, &t, true).unwrap();
        assert_eq!(fp.task.query_enabled(path).unwrap(), Some(false));
        restore_backup(&ports, &report.backup).unwrap();
        assert_eq!(fp.task.query_enabled(path).unwrap(), Some(true));
        // 任务不存在 → 备份阶段失败（未动系统）
        let t2 = Tweak {
            id: "task_missing".into(),
            name: "任务缺失".into(),
            category: "test".into(),
            description: String::new(),
            requires_admin: true,
            maintenance: false,
            actions: vec![TweakAction::Task { path: r"\Nope\Nope".into(), enabled: false }],
        };
        assert!(apply(&ports, &t2, true).is_err());
    }

    #[test]
    fn backup_store_roundtrip_and_overwrite() {
        let dir = std::env::temp_dir().join(format!("nf_winops_bs_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = BackupStore::open(&dir);
        let fp = FakePorts::new();
        fp.reg.set(K, "t1_v", RegValue::Dword(1));
        let ports = fp.ports();
        let t = tweak("t1", "测试", 7);
        let report = apply(&ports, &t, false).unwrap();
        store.save(&report).unwrap();
        assert!(store.has_backup("t1"));
        assert_eq!(store.take("t1").len(), 1);
        // 二次 apply 覆盖同 tweak 备份
        fp.reg.set(K, "t1_v", RegValue::Dword(5));
        let report2 = apply(&ports, &t, false).unwrap();
        store.save(&report2).unwrap();
        let items = store.take("t1");
        assert_eq!(items.len(), 1);
        match &items[0] {
            BackupItem::Registry(rb) => assert_eq!(rb.old_value, Some(RegValue::Dword(5))),
            _ => panic!("应为 Registry 备份"),
        }
        assert!(!store.has_backup("nope"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_failure_aborts_before_touching_system() {
        struct ReadOnly;
        impl RegistryOps for ReadOnly {
            fn read_value(&self, _k: &str, _n: &str) -> std::result::Result<(RegValue, bool), host_core::error::AppError> {
                Err(host_core::error::AppError::module("T", "只读", None))
            }
            fn write_value(&self, _k: &str, _n: &str, _v: &RegValue) -> std::result::Result<(), host_core::error::AppError> {
                panic!("备份失败后不应写入")
            }
            fn delete_value(&self, _k: &str, _n: &str) -> std::result::Result<(), host_core::error::AppError> {
                Ok(())
            }
        }
        let fp = FakePorts::new();
        let ports = SysPorts {
            registry: &ReadOnly,
            tasks: Some(&fp.task),
            services: Some(&fp.svc),
            maintenance: Some(&fp.maint),
            appx: Some(&fp.appx),
        };
        let err = apply(&ports, &tweak("t3", "测试", 1), false).unwrap_err();
        assert!(err.to_string().contains("只读"));
    }

    // ---------------- W4：clear_cache / maintenance / 回归检测 ----------------

    /// 维护型 tweak：scan 恒 NotApplied（不做状态查询）；clear_cache 端到端
    #[test]
    fn maintenance_scan_and_clear_cache_e2e() {
        let fp = FakePorts::new();
        fp.svc.set("wuauserv", StartType::Auto, true);
        fp.svc.set("bits", StartType::Manual, true);
        let ports = fp.ports();
        let t = Tweak {
            id: "update_clear_cache".into(),
            name: "清理更新缓存".into(),
            category: "update".into(),
            description: String::new(),
            requires_admin: true,
            maintenance: true,
            actions: vec![
                TweakAction::Service { name: "wuauserv".into(), start_type: None, action: Some(SvcAction::Stop) },
                TweakAction::Service { name: "bits".into(), start_type: None, action: Some(SvcAction::Stop) },
                TweakAction::FileClean {
                    path: r"C:\Windows\SoftwareDistribution\Download".into(),
                    recursive: true,
                    skip_recent_hours: 0,
                },
                TweakAction::Service { name: "wuauserv".into(), start_type: None, action: Some(SvcAction::Start) },
                TweakAction::Service { name: "bits".into(), start_type: None, action: Some(SvcAction::Start) },
            ],
        };
        // scan：恒 NotApplied（即便服务运行中）
        let states = scan(&ports, &[t.clone()], true);
        assert_eq!(states[0].1, ScanState::NotApplied);
        // apply：停止 → 清理 → 拉起；verify 短路通过
        let report = apply(&ports, &t, true).unwrap();
        assert!(report.verified);
        assert!(fp.svc.query("wuauserv").unwrap().running, "结束后服务应已拉起");
        assert!(fp.svc.query("bits").unwrap().running);
        assert_eq!(fp.maint.calls.lock().unwrap().len(), 1, "clean_dir 调用一次");
        // 备份：Service 条目带 was_running + FileClean 占位
        let svc_items = report.backup.iter().filter(|b| matches!(b, BackupItem::Service { .. })).count();
        assert_eq!(svc_items, 4);
        assert!(report.backup.iter().any(|b| matches!(b, BackupItem::FileClean)));
        // 回滚：写回原 start_type + was_running 拉起（无崩溃即语义正确）
        restore_backup(&ports, &report.backup).unwrap();
    }

    /// Disabled 服务 start 跳过（disable_auto 禁用 wuauserv 后 clear_cache 仍可执行）
    #[test]
    fn clear_cache_with_disabled_service() {
        let fp = FakePorts::new();
        fp.svc.set("wuauserv", StartType::Disabled, false);
        fp.svc.set("bits", StartType::Manual, false);
        let ports = fp.ports();
        let t = Tweak {
            id: "update_clear_cache".into(),
            name: "清理更新缓存".into(),
            category: "update".into(),
            description: String::new(),
            requires_admin: true,
            maintenance: true,
            actions: vec![
                TweakAction::Service { name: "wuauserv".into(), start_type: None, action: Some(SvcAction::Stop) },
                TweakAction::FileClean { path: "X".into(), recursive: true, skip_recent_hours: 0 },
                TweakAction::Service { name: "wuauserv".into(), start_type: None, action: Some(SvcAction::Start) },
            ],
        };
        assert!(apply(&ports, &t, true).is_ok(), "Disabled 服务 start 应跳过而非报错");
        assert!(!fp.svc.query("wuauserv").unwrap().running, "Disabled 服务不应被拉起");
    }

    /// 回归检测：注册表值被改回原值 → 检出；仍为目标值 → 不报
    #[test]
    fn regression_check_detects_self_heal() {
        let fp = FakePorts::new();
        let dir = std::env::temp_dir().join(format!("nf_winops_reg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = BackupStore::open(&dir);
        // 原值 0 预先存在 → apply 备份 {existed:true, old:0} 并写入目标 1
        fp.reg.set(K, "taskbar_x_v", RegValue::Dword(0));
        let ports = fp.ports();
        let t = tweak("taskbar_x", "测试", 1);
        let report = apply(&ports, &t, false).unwrap();
        store.save(&report).unwrap();
        // 当前 = 目标值(1) → 无回归
        assert!(regression_check(&ports, &store, &[t.clone()]).is_empty());
        // 系统自愈：改回原值(0) → 检出
        fp.reg.set(K, "taskbar_x_v", RegValue::Dword(0));
        assert_eq!(regression_check(&ports, &store, &[t.clone()]), vec!["taskbar_x".to_string()]);
        // 回滚后 remove 备份 → 不再误报
        store.remove("taskbar_x");
        assert!(regression_check(&ports, &store, &[t]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归检测：我们创建的值（原不存在）被删 → 检出
    #[test]
    fn regression_check_detects_deleted_value() {
        let fp = FakePorts::new();
        let dir = std::env::temp_dir().join(format!("nf_winops_reg2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = BackupStore::open(&dir);
        let ports = fp.ports();
        let t = tweak("created_val", "测试", 1);
        let report = apply(&ports, &t, false).unwrap();
        store.save(&report).unwrap();
        // 外部删除 → 回归
        fp.reg.delete_value(K, "created_val_v").unwrap();
        assert_eq!(regression_check(&ports, &store, &[t]), vec!["created_val".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- W5：AppxRemove ----------------

    /// Appx 移除端到端：当前用户 + provisioned 双动作；备份记录 was_installed
    #[test]
    fn appx_remove_e2e() {
        let fp = FakePorts::new();
        fp.appx.install("Microsoft.BingWeather");
        let ports = fp.ports();
        let t = Tweak {
            id: "apps_remove_weather".into(),
            name: "移除天气".into(),
            category: "apps".into(),
            description: String::new(),
            requires_admin: true,
            maintenance: false,
            actions: vec![
                TweakAction::AppxRemove { name: "Microsoft.BingWeather".into(), all_users: false },
                TweakAction::AppxRemove { name: "Microsoft.BingWeather".into(), all_users: true },
            ],
        };
        // scan：当前用户已装 → 未应用；管理员视角可判定
        let states = scan(&ports, &[t.clone()], true);
        assert_eq!(states[0].1, ScanState::NotApplied);
        let report = apply(&ports, &t, true).unwrap();
        assert!(report.verified, "移除后包不在 → action_applied=true");
        assert_eq!(fp.appx.removed_current.lock().unwrap().len(), 1);
        assert_eq!(fp.appx.removed_provisioned.lock().unwrap().len(), 1);
        // 备份：两条 Appx 条目 was_installed=true；restore 不报错（Store 重装提示）
        assert!(report.backup.iter().all(|b| matches!(b, BackupItem::Appx { was_installed: true, .. })));
        restore_backup(&ports, &report.backup).unwrap();
        // scan：包已不在 → 已应用
        let states = scan(&ports, &[t], true);
        assert_eq!(states[0].1, ScanState::Applied);
    }

    /// Appx 回归检测：被 Store/系统装回 → 检出
    #[test]
    fn regression_check_appx_reinstall() {
        let fp = FakePorts::new();
        let dir = std::env::temp_dir().join(format!("nf_winops_appx_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = BackupStore::open(&dir);
        fp.appx.install("Microsoft.Tips");
        let ports = fp.ports();
        let t = Tweak {
            id: "apps_remove_tips".into(),
            name: "移除提示".into(),
            category: "apps".into(),
            description: String::new(),
            requires_admin: true,
            maintenance: false,
            actions: vec![TweakAction::AppxRemove { name: "Microsoft.Tips".into(), all_users: false }],
        };
        let report = apply(&ports, &t, true).unwrap();
        store.save(&report).unwrap();
        // 包已卸载 → 无回归
        assert!(regression_check(&ports, &store, &[t.clone()]).is_empty());
        // 系统装回 → 回归
        fp.appx.install("Microsoft.Tips");
        assert_eq!(regression_check(&ports, &store, &[t]), vec!["apps_remove_tips".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- W6：Exec / 还原点 / 内存清理 ----------------

    /// Exec/RestorePoint/EmptyWorkingSet 动作端到端：维护型 + 不可逆备份占位
    #[test]
    fn exec_and_maintenance_actions_e2e() {
        let fp = FakePorts::new();
        let ports = fp.ports();
        let t = Tweak {
            id: "maint_ops".into(),
            name: "维护操作".into(),
            category: "maintenance".into(),
            description: String::new(),
            requires_admin: true,
            maintenance: true,
            actions: vec![
                TweakAction::Exec {
                    program: "powercfg".into(),
                    args: vec!["-duplicatescheme".into(), "e9a42b02-d546-448a-9c71-0b2ca57cbcb7".into()],
                    timeout_ms: 60_000,
                },
                TweakAction::RestorePoint { description: "NexusForge 应用前".into() },
                TweakAction::EmptyWorkingSet {},
            ],
        };
        let report = apply(&ports, &t, true).unwrap();
        assert!(report.verified);
        assert_eq!(fp.maint.execs.lock().unwrap().len(), 1, "Exec 调用一次");
        assert_eq!(fp.maint.restore_points.lock().unwrap().len(), 1);
        assert_eq!(*fp.maint.working_sets.lock().unwrap(), 1);
        // 备份：3 条 Exec 占位；restore 空操作不报错
        assert_eq!(report.backup.iter().filter(|b| matches!(b, BackupItem::Exec)).count(), 3);
        restore_backup(&ports, &report.backup).unwrap();
    }

    /// Exec 白名单参数经 catalog 序列化回读（serde default timeout）
    #[test]
    fn exec_action_serde_roundtrip() {
        let raw = r#"[{"id":"x","name":"x","category":"maintenance","requires_admin":true,"maintenance":true,
            "actions":[{"type":"exec","program":"dism","args":["/Online","/ScanHealth"]}]}]"#;
        let tweaks: Vec<Tweak> = serde_json::from_str(raw).unwrap();
        match &tweaks[0].actions[0] {
            TweakAction::Exec { program, args, timeout_ms } => {
                assert_eq!(program, "dism");
                assert_eq!(args, &vec!["/Online".to_string(), "/ScanHealth".to_string()]);
                assert_eq!(*timeout_ms, 30_000, "默认超时 30s");
            }
            other => panic!("应为 Exec 动作: {other:?}"),
        }
    }
}
