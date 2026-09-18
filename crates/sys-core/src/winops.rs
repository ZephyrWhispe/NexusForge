//! WinOps Tweak 引擎（docs/impl/08 W0–W3）：catalog 数据驱动 + BAVR 执行语义。
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

use host_core::ports::{RegValue, RegistryOps, ServiceCtlPort, StartType, TaskTogglePort};

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
    pub actions: Vec<TweakAction>,
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
    /// 服务启动类型（需管理员；sc.exe 封装）
    Service {
        name: String,
        start_type: StartType,
    },
    /// 计划任务启停（系统内置任务需管理员；schtasks /Change 封装）
    Task {
        path: String,
        enabled: bool,
    },
    /// 预留：Appx/FileClean/Exec 等 W3+ 动作类型（catalog 出现即报"需更高版本"）
    Unsupported { kind: String },
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
    /// 服务启动类型（恢复 = 写回原 start_type）
    Service { name: String, start_type: StartType },
    /// 计划任务（existed=false → 无法恢复，跳过）
    Task { path: String, existed: bool, enabled: bool },
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

/// Port 聚合（registry 必备；tasks/services 随动作类型可选）
pub struct SysPorts<'a> {
    pub registry: &'a dyn RegistryOps,
    pub tasks: Option<&'a dyn TaskTogglePort>,
    pub services: Option<&'a dyn ServiceCtlPort>,
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
        TweakAction::Service { name, start_type } => match ports.services {
            Some(svc) => match svc.query(name) {
                Ok(info) => Ok(info.start_type == *start_type),
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
        TweakAction::Unsupported { kind } => Err(SysError::Catalog(format!("动作类型 {kind} 需更高版本支持"))),
    }
}

/// 扫描状态（三态：requires_admin 且非管理员 → needs_admin，不做状态查询）
pub fn scan(ports: &SysPorts, tweaks: &[Tweak], is_admin: bool) -> Vec<(Tweak, ScanState)> {
    tweaks
        .iter()
        .map(|t| {
            let state = if t.requires_admin && !is_admin {
                ScanState::NeedsAdmin
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
            Ok(BackupItem::Service { name: name.clone(), start_type: info.start_type })
        }
        TweakAction::Task { path, .. } => {
            let t = ports.tasks.ok_or_else(|| SysError::Apply("计划任务端口未注册（无法备份任务状态）".into()))?;
            let enabled = t
                .query_enabled(path)
                .map_err(|e| SysError::Apply(format!("任务 {path} 备份失败: {e}")))?
                .ok_or_else(|| SysError::Apply(format!("任务 {path} 不存在")))?;
            Ok(BackupItem::Task { path: path.clone(), existed: true, enabled })
        }
        TweakAction::Unsupported { kind } => Err(SysError::Catalog(format!("动作类型 {kind} 需更高版本支持"))),
    }
}

/// 应用单个 action（写目标值）
fn apply_action(ports: &SysPorts, action: &TweakAction) -> SysResult<()> {
    match action {
        TweakAction::Registry { key, value_name, data, .. } => ports
            .registry
            .write_value(key, value_name, data)
            .map_err(|e| SysError::Registry(e.to_string())),
        TweakAction::Service { name, start_type } => {
            let svc = ports.services.ok_or_else(|| SysError::Apply("服务端口未注册".into()))?;
            svc.set_start_type(name, *start_type).map_err(|e| SysError::Apply(format!("服务 {name} 配置失败: {e}")))
        }
        TweakAction::Task { path, enabled } => {
            let t = ports.tasks.ok_or_else(|| SysError::Apply("计划任务端口未注册".into()))?;
            t.set_enabled(path, *enabled).map_err(|e| SysError::Apply(format!("任务 {path} 启停失败: {e}")))
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
            BackupItem::Service { name, start_type } => {
                if let Some(svc) = ports.services {
                    svc.set_start_type(name, *start_type)
                        .map_err(|e| SysError::Apply(format!("服务 {name} 恢复失败: {e}")))?;
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
    // 3. 校验
    let verified = applied_errors.is_empty()
        && tweak.actions.iter().all(|a| action_applied(ports, a).unwrap_or(false));
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

    /// 内存服务表（Service 动作测试）
    #[derive(Default)]
    struct FakeServices {
        services: Mutex<HashMap<String, StartType>>,
    }
    impl ServiceCtlPort for FakeServices {
        fn query(&self, name: &str) -> std::result::Result<ServiceInfo, host_core::error::AppError> {
            let m = self.services.lock().unwrap();
            m.get(name)
                .map(|&st| ServiceInfo { name: name.into(), start_type: st, running: false })
                .ok_or_else(|| host_core::error::AppError::module("T", format!("服务 {name} 不存在"), None))
        }
        fn set_start_type(&self, name: &str, st: StartType) -> std::result::Result<(), host_core::error::AppError> {
            self.services.lock().unwrap().insert(name.to_string(), st);
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

    /// 端口聚合构造（全部 mock）
    struct FakePorts {
        reg: FakeRegistry,
        svc: FakeServices,
        task: FakeTasks,
    }
    impl FakePorts {
        fn new() -> Self {
            Self { reg: FakeRegistry::default(), svc: FakeServices::default(), task: FakeTasks::default() }
        }
        fn ports(&self) -> SysPorts<'_> {
            SysPorts { registry: &self.reg, tasks: Some(&self.task), services: Some(&self.svc) }
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
            TweakAction::Unsupported { .. } => false,
        })), "HKLM registry 动作与 Service/Task 动作必须标 requires_admin");
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
        let ports = SysPorts { registry: &flaky, tasks: Some(&fp.task), services: Some(&fp.svc) };
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
        fp.svc.services.lock().unwrap().insert("DiagTrack".into(), StartType::Auto);
        let ports = fp.ports();
        let t = Tweak {
            id: "svc_off".into(),
            name: "禁服务".into(),
            category: "test".into(),
            description: String::new(),
            requires_admin: true,
            actions: vec![TweakAction::Service {
                name: "DiagTrack".into(),
                start_type: StartType::Disabled,
            }],
        };
        // 引擎闸：非管理员 → 拒绝
        assert!(apply(&ports, &t, false).is_err());
        // 管理员 → 成功
        let report = apply(&ports, &t, true).unwrap();
        assert!(report.verified);
        match &report.backup[0] {
            BackupItem::Service { name, start_type } => {
                assert_eq!(name, "DiagTrack");
                assert_eq!(*start_type, StartType::Auto);
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
        let ports = SysPorts { registry: &ReadOnly, tasks: Some(&fp.task), services: Some(&fp.svc) };
        let err = apply(&ports, &tweak("t3", "测试", 1), false).unwrap_err();
        assert!(err.to_string().contains("只读"));
    }
}
