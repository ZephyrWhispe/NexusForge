//! helper 方法白名单（docs/impl/08 §4.3 v1 集：registry / service / task / file / appx 窄面）。
//!
//! 数据面 = win-integration 本地实现（helper 已是提权进程，HKLM/服务/系统任务直执行）。
//! exec/dns/hosts/maintenance 等 W6+ 方法族按文档白名单逐步扩充，未收录一律拒绝。

use std::sync::atomic::{AtomicBool, Ordering};

use host_core::ports::{
    AppxPort, MaintenancePort, RegValue, RegistryOps, ServiceCtlPort, StartType, TaskTogglePort,
};
use serde_json::{json, Value};
use win_integration::appx::AppxOps;
use win_integration::maintenance::MaintenanceWin;
use win_integration::registry::RegistryOpsWin;
use win_integration::service::ServiceOps;
use win_integration::taskschd::TaskSchdOps;

/// 长任务执行中标志（看门狗在 BUSY 期间不自退）
pub static BUSY: AtomicBool = AtomicBool::new(false);

/// SEC-19：RAII 忙碌守卫——构造置位、drop 复位，请求处理入口统一包裹，
/// 杜绝"新增耗时方法忘记置位 BUSY → 看门狗中途 exit(0)"的半清理状态。
struct BusyGuard;

impl BusyGuard {
    fn new() -> Self {
        BUSY.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

/// 数据面（构造一次，进程内复用）
pub struct Ops {
    registry: RegistryOpsWin,
    services: ServiceOps,
    tasks: TaskSchdOps,
    maintenance: MaintenanceWin,
    appx: AppxOps,
}

impl Ops {
    pub fn new() -> Self {
        Self {
            registry: RegistryOpsWin::new(),
            services: ServiceOps::new(),
            tasks: TaskSchdOps::new(),
            maintenance: MaintenanceWin::new(),
            appx: AppxOps::new(),
        }
    }
}

impl Default for Ops {
    fn default() -> Self {
        Self::new()
    }
}

/// file.clean_dir 路径白名单（提权进程纵深防御：只允许精确清理下列目录，防 ..\ 遍历/任意删除）
const CLEAN_DIR_ALLOWLIST: &[&str] = &["C:\\Windows\\SoftwareDistribution\\Download"];

/// SEC-02：注册表写/删键白名单（前缀匹配，与内置 catalog 触及的键一一对应；
/// 外置 catalog 只能新增 tweak，触及白名单外键的动作在 helper 层拒绝）
const REG_ALLOWED_PREFIXES: &[&str] = &[
    r"HKCU\Software\Microsoft\Windows\CurrentVersion\AdvertisingInfo",
    r"HKCU\Software\Microsoft\Windows\CurrentVersion\ContentDeliveryManager",
    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Search",
    r"HKCU\Software\Policies\Microsoft\Windows\System",
    r"HKCU\Software\Policies\Microsoft\Windows\WindowsCopilot",
    r"HKLM\SOFTWARE\Policies\Microsoft\EdgeUpdate",
    r"HKLM\SOFTWARE\Policies\Microsoft\Windows\WindowsAI",
    r"HKLM\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate",
];

/// SEC-02：服务启停/启动类型白名单（只允许本产品预置维护动作触及的服务）
const SVC_ALLOWLIST: &[&str] = &["wuauserv", "bits", "diagtrack", "w32time"];

/// SEC-02：计划任务启停白名单（与内置 catalog 触及的任务一一对应）
const TASK_ALLOWLIST: &[&str] = &[
    r"\Microsoft\Windows\Application Experience\Microsoft Compatibility Appraiser",
    r"\Microsoft\Windows\Application Experience\ProgramDataUpdater",
    r"\Microsoft\Windows\Customer Experience Improvement Program\Consolidator",
    r"\Microsoft\Windows\Customer Experience Improvement Program\UsbCeip",
];

fn clean_dir_allowed(path: &str) -> bool {
    // Windows 路径大小写不敏感；统一 / → \ 后精确比较
    let norm = |s: &str| {
        s.trim_start_matches(r"\\?\")
            .replace('/', "\\")
            .to_ascii_lowercase()
    };
    let p = norm(path);
    CLEAN_DIR_ALLOWLIST.iter().any(|a| p == norm(a))
}

/// 注册表键白名单判定：键等于或位于允许前缀之下（大小写/斜杠方向不敏感）
fn reg_key_allowed(key: &str) -> bool {
    let norm = |s: &str| {
        s.trim_start_matches(r"\\?\")
            .replace('/', "\\")
            .to_ascii_lowercase()
    };
    let k = norm(key);
    let in_prefixes = REG_ALLOWED_PREFIXES.iter().any(|p| {
        let p = norm(p);
        k == p || k.starts_with(&format!("{p}\\"))
    });
    #[cfg(test)]
    let test_hook = k.starts_with("hkcu\\software\\nexusforgehelper");
    #[cfg(not(test))]
    let test_hook = false;
    in_prefixes || test_hook
}

/// 服务名白名单判定（大小写不敏感）
fn svc_allowed(name: &str) -> bool {
    SVC_ALLOWLIST.contains(&name.to_ascii_lowercase().as_str())
}

/// 计划任务路径白名单判定（大小写不敏感）
fn task_allowed(path: &str) -> bool {
    TASK_ALLOWLIST
        .iter()
        .any(|a| a.eq_ignore_ascii_case(&path.replace('/', "\\")))
}

fn req_str<'a>(p: &'a Value, k: &str) -> Result<&'a str, String> {
    p[k].as_str().ok_or_else(|| format!("缺少参数 {k}"))
}

/// 白名单分发（未知方法一律拒绝；错误信息回传 error.message）。
/// SEC-19：入口统一 RAII BusyGuard——任何方法（含 file.clean_dir 等耗时操作）
/// 执行期间看门狗不得自退，无需逐方法手工配对。
pub fn handle_request(ops: &Ops, method: &str, p: Value) -> Result<Value, String> {
    let _busy = BusyGuard::new();
    match method {
        "helper.ping" => Ok(json!({ "version": env!("CARGO_PKG_VERSION") })),
        "registry.read" => {
            let (v, existed) = ops
                .registry
                .read_value(req_str(&p, "key")?, req_str(&p, "value_name")?)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "existed": existed, "value": if existed { Some(v) } else { None } }))
        }
        "registry.write" => {
            let key = req_str(&p, "key")?;
            if !reg_key_allowed(key) {
                return Err(format!("键不在白名单（SEC-02）: {key}"));
            }
            let value: RegValue = serde_json::from_value(p["value"].clone())
                .map_err(|e| format!("value 解析失败: {e}"))?;
            ops.registry
                .write_value(key, req_str(&p, "value_name")?, &value)
                .map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "registry.delete" => {
            let key = req_str(&p, "key")?;
            if !reg_key_allowed(key) {
                return Err(format!("键不在白名单（SEC-02）: {key}"));
            }
            ops.registry
                .delete_value(key, req_str(&p, "value_name")?)
                .map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "service.query" => {
            let info = ops
                .services
                .query(req_str(&p, "name")?)
                .map_err(|e| e.to_string())?;
            serde_json::to_value(info).map_err(|e| e.to_string())
        }
        "service.set_start" => {
            let name = req_str(&p, "name")?;
            if !svc_allowed(name) {
                return Err(format!("服务不在白名单（SEC-02）: {name}"));
            }
            let st: StartType = serde_json::from_value(p["start_type"].clone())
                .map_err(|e| format!("start_type 解析失败: {e}"))?;
            ops.services
                .set_start_type(name, st)
                .map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "service.stop" => {
            let name = req_str(&p, "name")?;
            if !svc_allowed(name) {
                return Err(format!("服务不在白名单（SEC-02）: {name}"));
            }
            ops.services.stop(name).map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "service.start" => {
            let name = req_str(&p, "name")?;
            if !svc_allowed(name) {
                return Err(format!("服务不在白名单（SEC-02）: {name}"));
            }
            ops.services.start(name).map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "file.clean_dir" => {
            let path = req_str(&p, "path")?;
            if !clean_dir_allowed(path) {
                return Err(format!("路径不在清理白名单: {path}"));
            }
            let recursive = p["recursive"].as_bool().unwrap_or(true);
            let skip = p["skip_recent_hours"].as_u64().unwrap_or(24) as u32;
            let removed = ops
                .maintenance
                .clean_dir(path, recursive, skip)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "removed": removed }))
        }
        "task.query_enabled" => {
            let enabled = ops
                .tasks
                .query_enabled(req_str(&p, "path")?)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "enabled": enabled }))
        }
        "task.set_enabled" => {
            let path = req_str(&p, "path")?;
            if !task_allowed(path) {
                return Err(format!("计划任务不在白名单（SEC-02）: {path}"));
            }
            let enabled = p["enabled"].as_bool().ok_or("缺少参数 enabled")?;
            ops.tasks
                .set_enabled(path, enabled)
                .map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        // Appx：名称校验在 win-integration 层（[A-Za-z0-9._] 防注入）；当前用户移除偏离 §4.3
        // 白名单补充收录（helper 与主进程同用户，WinRT 当前用户移除等价）
        "appx.remove_current_user" => {
            let n = ops
                .appx
                .remove_current_user(req_str(&p, "name")?)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "removed": n }))
        }
        "appx.remove_provisioned" => {
            let n = ops
                .appx
                .remove_provisioned(req_str(&p, "name")?)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "removed": n }))
        }
        // Exec 白名单执行（spec §4.3 单一 exec 方法 + 程序白名单的偏离设计——
        // 程序名在 win-integration 层精确匹配 powercfg/dism/sfc/netsh/onedrive_uninstall）；
        // DISM/SFC 可达 10–30 分钟 → BUSY 置位防看门狗自退
        "exec" => {
            let program = req_str(&p, "program")?.to_string();
            let args: Vec<String> = p["args"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let timeout = p["timeout_ms"].as_u64().unwrap_or(30_000) as u32;
            let out = ops
                .maintenance
                .exec(&program, &args, timeout)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "output": out }))
        }
        "maintenance.restore_point" => {
            let desc = req_str(&p, "description")?;
            ops.maintenance
                .restore_point(desc)
                .map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "maintenance.empty_working_set" => {
            let n = ops
                .maintenance
                .empty_working_set()
                .map_err(|e| e.to_string())?;
            Ok(json!({ "processed": n }))
        }
        // Defender 实时保护开关（W7 高风险族；固定 PS 模板无用户输入）
        "defender.set_realtime" => {
            let disable = p["disable"].as_bool().ok_or("缺少参数 disable")?;
            ops.maintenance
                .defender_realtime(disable)
                .map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        other => Err(format!("未知方法（白名单外）: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const K: &str = "HKCU\\Software\\NexusForgeHelperTest";

    #[test]
    fn unknown_method_rejected() {
        let ops = Ops::new();
        assert!(handle_request(&ops, "exec.dism", json!({})).is_err());
        assert!(handle_request(&ops, "registry.drop_table", json!({})).is_err());
        assert!(handle_request(&ops, "", json!({})).is_err());
    }

    #[test]
    fn ping_whitelisted() {
        let ops = Ops::new();
        let r = handle_request(&ops, "helper.ping", json!({})).unwrap();
        assert!(r["version"].is_string());
    }

    /// HKCU 测试键读写删回环（helper 数据面 = win-integration 本地实现）
    #[test]
    fn registry_whitelist_roundtrip() {
        let ops = Ops::new();
        handle_request(
            &ops,
            "registry.write",
            json!({"key": K, "value_name": "v", "value": {"dword": 7}}),
        )
        .unwrap();
        let r =
            handle_request(&ops, "registry.read", json!({"key": K, "value_name": "v"})).unwrap();
        assert_eq!(r["existed"], true);
        assert_eq!(r["value"]["dword"], 7);
        handle_request(
            &ops,
            "registry.delete",
            json!({"key": K, "value_name": "v"}),
        )
        .unwrap();
        let r =
            handle_request(&ops, "registry.read", json!({"key": K, "value_name": "v"})).unwrap();
        assert_eq!(r["existed"], false);
    }

    /// service.set_start 参数解析（snake_case 三态）
    #[test]
    fn service_start_type_parse() {
        let st: StartType = serde_json::from_value(json!("disabled")).unwrap();
        assert_eq!(st, StartType::Disabled);
        assert!(serde_json::from_value::<StartType>(json!("always")).is_err());
    }

    /// file.clean_dir 路径白名单：白名单内放行、任意路径/遍历/大小写变体按语义判定
    #[test]
    fn clean_dir_allowlist() {
        assert!(clean_dir_allowed(
            r"C:\Windows\SoftwareDistribution\Download"
        ));
        assert!(
            clean_dir_allowed(r"c:/windows/softwaredistribution/download"),
            "大小写与斜杠方向不敏感"
        );
        assert!(
            !clean_dir_allowed(r"C:\Users\86151\Documents"),
            "任意路径拒绝"
        );
        assert!(!clean_dir_allowed(r"C:\Windows\System32"), "系统目录拒绝");
        assert!(
            !clean_dir_allowed(r"C:\Windows\SoftwareDistribution\Download\..\.."),
            "遍历拒绝（精确匹配）"
        );
        assert!(
            !clean_dir_allowed(r"C:\Windows\SoftwareDistribution"),
            "前缀目录拒绝（只允许精确路径）"
        );
        // 白名单外路径经 handle_request 一律拒绝（不触达数据面）
        let ops = Ops::new();
        assert!(handle_request(
            &ops,
            "file.clean_dir",
            json!({"path": r"C:\Users\86151\Documents"})
        )
        .is_err());
    }

    /// appx 方法：非法包名（注入载荷）在数据面校验层拒绝；未知方法仍拒绝
    #[test]
    fn appx_name_validation() {
        let ops = Ops::new();
        assert!(
            handle_request(&ops, "appx.remove_current_user", json!({"name": "a'; rm"})).is_err()
        );
        assert!(handle_request(&ops, "appx.remove_provisioned", json!({"name": ""})).is_err());
        assert!(
            handle_request(&ops, "appx.drop_all", json!({})).is_err(),
            "白名单外方法拒绝"
        );
    }

    // ---- SEC-02：提权输入面白名单（负例必须） ----

    /// 注册表写/删：白名单外键（含 HKLM Run、无前缀、路径遍历变体）一律拒绝
    #[test]
    fn registry_write_rejects_out_of_whitelist_key() {
        let ops = Ops::new();
        let evil = json!({"key": r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
                          "value_name": "x", "value": {"sz": "calc.exe"}});
        assert!(handle_request(&ops, "registry.write", evil).is_err());
        for key in [
            r"HKLM\SYSTEM\CurrentControlSet\Services\evil\Start",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            r"HKCU\Software\AnythingElse",
            r"HKLM\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\..\..\Run",
            "",
        ] {
            let req = json!({"key": key, "value_name": "v", "value": {"dword": 1}});
            assert!(
                handle_request(&ops, "registry.write", req).is_err(),
                "键 {key:?} 必须被拒"
            );
        }
        // 删除同样受限
        assert!(handle_request(
            &ops,
            "registry.delete",
            json!({"key": r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "value_name": "x"})
        )
        .is_err());
        // 白名单判定语义
        assert!(reg_key_allowed(
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced"
        ));
        assert!(
            reg_key_allowed(
                r"hkcu/software/microsoft/windows/currentversion/explorer/advanced/sub"
            ),
            "大小写与斜杠方向不敏感"
        );
        assert!(!reg_key_allowed(
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run"
        ));
    }

    /// 服务启停/启动类型：白名单外服务拒绝（防任意服务停摆 = 提权 DoS）
    #[test]
    fn service_mutations_reject_out_of_whitelist() {
        let ops = Ops::new();
        for name in ["LanmanServer", "eventlog", "SamSs", ""] {
            assert!(
                handle_request(&ops, "service.stop", json!({"name": name})).is_err(),
                "服务 {name:?} 停止必须被拒"
            );
            assert!(handle_request(&ops, "service.start", json!({"name": name})).is_err());
            assert!(handle_request(
                &ops,
                "service.set_start",
                json!({"name": name, "start_type": "disabled"})
            )
            .is_err());
        }
        // 白名单判定大小写不敏感
        assert!(svc_allowed("DiagTrack"));
        assert!(svc_allowed("wuauserv"));
        assert!(!svc_allowed("LanmanWorkstation"));
    }

    /// 计划任务启停：白名单外任务拒绝
    #[test]
    fn task_set_enabled_rejects_out_of_whitelist() {
        let ops = Ops::new();
        assert!(handle_request(
            &ops,
            "task.set_enabled",
            json!({"path": r"\Microsoft\Windows\PushToInstall\LoginCheck", "enabled": false})
        )
        .is_err());
        assert!(handle_request(
            &ops,
            "task.set_enabled",
            json!({"path": r"\Evil\Task", "enabled": false})
        )
        .is_err());
        // 白名单内任务（大小写/斜杠变体）
        assert!(task_allowed(
            r"\microsoft\windows\application experience\microsoft compatibility appraiser"
        ));
        assert!(task_allowed(
            r"/Microsoft/Windows/Application Experience/ProgramDataUpdater"
        ));
        assert!(!task_allowed(
            r"\Microsoft\Windows\PushToInstall\LoginCheck"
        ));
    }

    /// exec：白名单程序 + 任意参数组合端到端被拒（模板校验在数据面双保险）
    #[test]
    fn exec_rejects_arbitrary_args() {
        let ops = Ops::new();
        assert!(handle_request(
            &ops,
            "exec",
            json!({"program": "netsh",
                   "args": ["advfirewall", "set", "allprofiles", "state", "off"],
                   "timeout_ms": 1000})
        )
        .is_err());
        assert!(handle_request(
            &ops,
            "exec",
            json!({"program": "dism",
                   "args": ["/Online", "/Add-Package", "/PackagePath:\\\\evil\\x.cab"],
                   "timeout_ms": 1000})
        )
        .is_err());
    }

    /// SEC-19：BusyGuard RAII 置位/复位
    #[test]
    fn busy_guard_sets_and_resets() {
        assert!(!BUSY.load(Ordering::SeqCst));
        {
            let _g = BusyGuard::new();
            assert!(BUSY.load(Ordering::SeqCst));
        }
        assert!(!BUSY.load(Ordering::SeqCst), "drop 必须复位 BUSY");
    }
}
