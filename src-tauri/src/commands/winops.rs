use host_core::error::AppError;
use tauri::State;

use super::sys::sys_err;
use crate::state::HostState;

// ======================== WinOps Tweak 引擎（M16 W0–W4，docs/impl/08） ========================

/// 组装 WinOps 端口聚合（registry 必备；tasks/services/maintenance/appx 注册缺失容忍为 None）
type WinOpsPorts = (
    std::sync::Arc<dyn host_core::ports::RegistryOps>,
    Option<std::sync::Arc<dyn host_core::ports::TaskTogglePort>>,
    Option<std::sync::Arc<dyn host_core::ports::ServiceCtlPort>>,
    Option<std::sync::Arc<dyn host_core::ports::MaintenancePort>>,
    Option<std::sync::Arc<dyn host_core::ports::AppxPort>>,
);

fn winops_ports(state: &HostState) -> Result<WinOpsPorts, AppError> {
    let registry = state
        .ports
        .get::<dyn host_core::ports::RegistryOps>()
        .ok_or_else(|| AppError::module("SYS_WINOPS_002", "注册表端口未注册", None))?;
    let tasks = state.ports.get::<dyn host_core::ports::TaskTogglePort>();
    let services = state.ports.get::<dyn host_core::ports::ServiceCtlPort>();
    let maintenance = state.ports.get::<dyn host_core::ports::MaintenancePort>();
    let appx = state.ports.get::<dyn host_core::ports::AppxPort>();
    Ok((registry, tasks, services, maintenance, appx))
}

/// 目录清单（内置 + 外置 {appData}/winops/catalog/*.json 覆盖）
#[tauri::command]
pub async fn winops_catalog(
    state: State<'_, HostState>,
) -> Result<Vec<sys_core::winops::Tweak>, AppError> {
    let external = state.app_data_dir.join("winops").join("catalog");
    tauri::async_runtime::spawn_blocking(move || sys_core::winops::load_catalog(Some(&external)))
        .await
        .map_err(|e| AppError::module("SYS_WINOPS_001", e.to_string(), None))?
        .map_err(sys_err)
}

/// 扫描应用状态（三态：已应用/未应用/需管理员——requires_admin 且非提权进程）
#[tauri::command]
pub async fn winops_scan(
    state: State<'_, HostState>,
) -> Result<Vec<(sys_core::winops::Tweak, sys_core::winops::ScanState)>, AppError> {
    let external = state.app_data_dir.join("winops").join("catalog");
    let (reg, tasks, services, maintenance, appx) = winops_ports(&state)?;
    let is_admin = state
        .ports
        .get::<dyn host_core::ports::SysProxyPort>()
        .map(|p| p.is_admin())
        .unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        let tweaks = sys_core::winops::load_catalog(Some(&external))?;
        let ports = sys_core::winops::SysPorts {
            registry: reg.as_ref(),
            tasks: tasks.as_deref(),
            services: services.as_deref(),
            maintenance: maintenance.as_deref(),
            appx: appx.as_deref(),
        };
        Ok(sys_core::winops::scan(&ports, &tweaks, is_admin))
    })
    .await
    .map_err(|e| AppError::module("SYS_WINOPS_001", e.to_string(), None))?
    .map_err(sys_err)
}

/// 判定 tweak 是否需要提权数据面（requires_admin、HKLM registry、provisioned Appx、
/// Exec/还原点/内存清理——系统级动作全部经 helper）
fn winops_needs_elevation(t: &sys_core::winops::Tweak) -> bool {
    use sys_core::winops::TweakAction;
    t.requires_admin
        || t.actions.iter().any(|a| match a {
            TweakAction::Registry { key, .. } => key.starts_with("HKLM"),
            TweakAction::AppxRemove {
                all_users: true, ..
            } => true,
            TweakAction::Exec { .. }
            | TweakAction::RestorePoint { .. }
            | TweakAction::EmptyWorkingSet {} => true,
            TweakAction::DefenderRealtime { .. } => true,
            _ => false,
        })
}

/// BAVR 应用（备份 → 写入 → 校验 → 失败补偿；成功后备份落 {appData}/winops/backup.json）
/// 非提权进程应用需管理员条目：拉起提权 Helper（UAC）→ helper-backed 数据面执行
#[tauri::command]
pub async fn winops_apply(
    id: String,
    state: State<'_, HostState>,
) -> Result<sys_core::winops::ApplyReport, AppError> {
    let external = state.app_data_dir.join("winops").join("catalog");
    let app_dir = state.app_data_dir.clone();
    let (reg, tasks, services, maintenance, appx) = winops_ports(&state)?;
    let helper_spawn = state.ports.get::<dyn host_core::ports::HelperSpawnPort>();
    let is_admin = state
        .ports
        .get::<dyn host_core::ports::SysProxyPort>()
        .map(|p| p.is_admin())
        .unwrap_or(false);
    // 闭包错误类型显式标注 AppError：内部 ensure_up/spawner 都是 AppError，
    // 靠尾部 map_err(sys_err) 反推会把闭包错误类型定成 SysError 而冲突（E0277）
    tauri::async_runtime::spawn_blocking(
        move || -> Result<sys_core::winops::ApplyReport, AppError> {
            let tweaks = sys_core::winops::load_catalog(Some(&external)).map_err(sys_err)?;
            let tweak = tweaks
                .into_iter()
                .find(|t| t.id == id)
                .ok_or_else(|| sys_core::SysError::Catalog(format!("Tweak {id} 不存在")))
                .map_err(sys_err)?;
            let report = if winops_needs_elevation(&tweak) && !is_admin {
                // 提权数据面：HKLM registry → helper；HKCU → 本地；Service/Task/FileClean/Appx → helper
                let spawner = helper_spawn.ok_or_else(|| {
                    AppError::module("SYS_HELPER_006", "HelperSpawnPort 未注册", None)
                })?;
                crate::winops_helper::ensure_up(spawner.as_ref())?;
                let routing = crate::winops_helper::RoutingRegistry::new(reg.clone());
                let ports = sys_core::winops::SysPorts {
                    registry: &routing,
                    tasks: Some(&crate::winops_helper::HelperTasks),
                    services: Some(&crate::winops_helper::HelperServices),
                    maintenance: Some(&crate::winops_helper::HelperMaintenance),
                    appx: Some(&crate::winops_helper::HelperAppx),
                };
                sys_core::winops::apply(&ports, &tweak, true).map_err(sys_err)?
            } else {
                let ports = sys_core::winops::SysPorts {
                    registry: reg.as_ref(),
                    tasks: tasks.as_deref(),
                    services: services.as_deref(),
                    maintenance: maintenance.as_deref(),
                    appx: appx.as_deref(),
                };
                sys_core::winops::apply(&ports, &tweak, is_admin).map_err(sys_err)?
            };
            sys_core::winops::BackupStore::open(&app_dir)
                .save(&report)
                .map_err(sys_err)?;
            // W7 审计：apply 落 JSONL（写失败不阻断）
            sys_core::winops::AuditStore::open(&app_dir).record(
                "ui",
                &id,
                "apply",
                Some(report.verified),
                "",
            );
            Ok(report)
        },
    )
    .await
    .map_err(|e| AppError::module("SYS_WINOPS_001", e.to_string(), None))?
}

/// 导出 WinOps 审计（审计记录 + 当前备份清单）到 {appData}/winops/exports/，返回文件路径
#[tauri::command]
pub async fn winops_audit_export(state: State<'_, HostState>) -> Result<String, AppError> {
    let app_dir = state.app_data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        sys_core::winops::AuditStore::open(&app_dir).export(&app_dir)
    })
    .await
    .map_err(|e| AppError::module("SYS_WINOPS_001", e.to_string(), None))?
    .map(|p| p.to_string_lossy().into_owned())
    .map_err(sys_err)
}

/// 回滚到最近一次 apply 前的状态（备份含 HKLM/服务/任务且非提权 → helper-backed 数据面；
/// 成功后移除备份——已还原状态不参与回归检测，也避免误判"可回滚"）
#[tauri::command]
pub async fn winops_rollback(id: String, state: State<'_, HostState>) -> Result<(), AppError> {
    let app_dir = state.app_data_dir.clone();
    let (reg, tasks, services, maintenance, appx) = winops_ports(&state)?;
    let helper_spawn = state.ports.get::<dyn host_core::ports::HelperSpawnPort>();
    let is_admin = state
        .ports
        .get::<dyn host_core::ports::SysProxyPort>()
        .map(|p| p.is_admin())
        .unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || -> sys_core::Result<()> {
        let store = sys_core::winops::BackupStore::open(&app_dir);
        let backup = store.take(&id);
        if backup.is_empty() {
            return Err(sys_core::SysError::Catalog(format!(
                "Tweak {id} 无备份可回滚"
            )));
        }
        let result = if crate::winops_helper::backup_needs_elevation(&backup) && !is_admin {
            let spawner = helper_spawn
                .ok_or_else(|| sys_core::SysError::Apply("HelperSpawnPort 未注册".into()))?;
            // 闭包错误类型为 SysError：AppError 转入 Apply 文案
            crate::winops_helper::ensure_up(spawner.as_ref())
                .map_err(|e| sys_core::SysError::Apply(e.to_string()))?;
            let routing = crate::winops_helper::RoutingRegistry::new(reg.clone());
            let ports = sys_core::winops::SysPorts {
                registry: &routing,
                tasks: Some(&crate::winops_helper::HelperTasks),
                services: Some(&crate::winops_helper::HelperServices),
                maintenance: Some(&crate::winops_helper::HelperMaintenance),
                appx: Some(&crate::winops_helper::HelperAppx),
            };
            sys_core::winops::restore_backup(&ports, &backup)
        } else {
            let ports = sys_core::winops::SysPorts {
                registry: reg.as_ref(),
                tasks: tasks.as_deref(),
                services: services.as_deref(),
                maintenance: maintenance.as_deref(),
                appx: appx.as_deref(),
            };
            sys_core::winops::restore_backup(&ports, &backup)
        };
        // 回滚成功 → 移除备份（回归检测不再比对该 tweak）
        if result.is_ok() {
            store.remove(&id);
            sys_core::winops::AuditStore::open(&app_dir).record("ui", &id, "rollback", None, "");
        }
        result
    })
    .await
    .map_err(|e| AppError::module("SYS_WINOPS_001", e.to_string(), None))?
    .map_err(sys_err)
}

#[cfg(test)]
mod tests {
    use super::winops_needs_elevation;
    use host_core::ports::RegValue;
    use sys_core::winops::{Tweak, TweakAction};

    fn tweak(requires_admin: bool, actions: Vec<TweakAction>) -> Tweak {
        Tweak {
            id: "t".into(),
            name: "n".into(),
            category: "c".into(),
            description: String::new(),
            requires_admin,
            maintenance: false,
            actions,
        }
    }

    fn registry(key: &str) -> TweakAction {
        TweakAction::Registry {
            key: key.into(),
            value_name: "v".into(),
            value_type: "dword".into(),
            data: RegValue::Dword(1),
        }
    }

    #[test]
    fn system_scoped_routes_to_elevation() {
        assert!(winops_needs_elevation(&tweak(true, vec![])));
        assert!(winops_needs_elevation(&tweak(
            false,
            vec![registry(r"HKLM\SOFTWARE\X")]
        )));
        assert!(winops_needs_elevation(&tweak(
            false,
            vec![TweakAction::EmptyWorkingSet {}]
        )));
        assert!(winops_needs_elevation(&tweak(
            false,
            vec![TweakAction::Exec {
                program: "powercfg".into(),
                args: vec![],
                timeout_ms: 5000,
            }]
        )));
        assert!(winops_needs_elevation(&tweak(
            false,
            vec![TweakAction::RestorePoint {
                description: String::new()
            }]
        )));
        assert!(winops_needs_elevation(&tweak(
            false,
            vec![TweakAction::DefenderRealtime { disable: true }]
        )));
        assert!(winops_needs_elevation(&tweak(
            false,
            vec![TweakAction::AppxRemove {
                name: "App".into(),
                all_users: true,
            }]
        )));
    }

    #[test]
    fn user_scoped_stays_in_process() {
        assert!(!winops_needs_elevation(&tweak(
            false,
            vec![registry(r"HKCU\SOFTWARE\X")]
        )));
        assert!(!winops_needs_elevation(&tweak(
            false,
            vec![TweakAction::Task {
                path: r"\NexusForge\T".into(),
                enabled: false,
            }]
        )));
        assert!(!winops_needs_elevation(&tweak(
            false,
            vec![TweakAction::AppxRemove {
                name: "App".into(),
                all_users: false,
            }]
        )));
        assert!(!winops_needs_elevation(&tweak(false, vec![])));
    }
}
