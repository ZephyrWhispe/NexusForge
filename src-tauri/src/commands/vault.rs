use host_core::error::AppError;
use serde::Serialize;
use tauri::State;

use crate::state::HostState;

// ---------------- 密码库命令（docs/impl/05 V7）----------------
// Argon2（数百 ms～秒级）与 SQLite 均为阻塞调用，统一 spawn_blocking

/// 密码库状态（前端三态渲染：uninitialized / locked / unlocked）
#[derive(Serialize)]
pub struct VaultStatusDto {
    pub state: String,
    pub lockout_remaining_secs: u64,
    /// 头部快照（KDF 参数 / vault_id，无机密）
    pub kdf: Option<vault_core::VaultHeader>,
    /// V4：头部含 Hello 信封（免密路径已启用）
    pub hello_enabled: bool,
    /// V4：连续校验失败后的进程级熔断（仅允许主密码解锁）
    pub hello_forced: bool,
    /// 本机 Windows Hello 可用性（false = 前端隐藏免密开关）
    pub hello_available: bool,
}

fn vault_service(state: &HostState) -> Result<std::sync::Arc<vault_core::VaultService>, AppError> {
    let svc = state
        .vault
        .service()
        .ok_or_else(|| AppError::module("VAULT_IPC_001", "密码库模块未就绪", None))?;
    // D-24 V5：任何触及服务的命令都算一次用户活动（空闲线计时基准）
    svc.touch();
    Ok(svc)
}

fn vault_state_str(s: vault_core::VaultState) -> &'static str {
    match s {
        vault_core::VaultState::Uninitialized => "uninitialized",
        vault_core::VaultState::Locked => "locked",
        vault_core::VaultState::Unlocked => "unlocked",
    }
}

/// 状态迁移事件（create/unlock/lock/改密码后发布）
fn publish_vault_state(state: &HostState, svc: &vault_core::VaultService) {
    state
        .bus
        .publish(host_core::events::Event::new(
            "vault.state_changed",
            "vault",
            serde_json::json!({ "state": vault_state_str(svc.state()) }),
        ))
        .ok();
}

/// 数据变更事件（条目/文件夹 CRUD 后发布）
fn publish_vault_entries(state: &HostState, action: &str, id: Option<&str>) {
    state
        .bus
        .publish(host_core::events::Event::new(
            "vault.entries_changed",
            "vault",
            serde_json::json!({ "action": action, "id": id }),
        ))
        .ok();
}

#[tauri::command]
pub async fn vault_status(state: State<'_, HostState>) -> Result<VaultStatusDto, AppError> {
    let svc = vault_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        Ok(VaultStatusDto {
            state: vault_state_str(svc.state()).into(),
            lockout_remaining_secs: svc.lockout_remaining_secs(),
            kdf: svc.header(),
            hello_enabled: svc.hello_enabled(),
            hello_forced: svc.hello_forced_off(),
            hello_available: win_integration::hello::WindowsHello::available(),
        })
    })
    .await
    .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
}

/// 新建保险库（默认 Argon2id 64MiB/t3/p4；秒级耗时 → spawn_blocking）
#[tauri::command]
pub async fn vault_create(
    master_password: String,
    state: State<'_, HostState>,
) -> Result<vault_core::VaultHeader, AppError> {
    let svc = vault_service(&state)?;
    let svc2 = svc.clone();
    tauri::async_runtime::spawn_blocking(move || svc2.create(&master_password, None))
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
        .inspect(|_h| {
            publish_vault_state(&state, &svc);
        })
}

/// 主密码解锁（每次全量 Argon2 → spawn_blocking）
#[tauri::command]
pub async fn vault_unlock(
    master_password: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let svc = vault_service(&state)?;
    let svc2 = svc.clone();
    tauri::async_runtime::spawn_blocking(move || svc2.unlock(&master_password))
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
        .map(|_| publish_vault_state(&state, &svc))
}

/// 锁定（DEK 立即 wipe）
#[tauri::command]
pub fn vault_lock(state: State<'_, HostState>) -> Result<(), AppError> {
    let svc = vault_service(&state)?;
    svc.lock()?;
    publish_vault_state(&state, &svc);
    Ok(())
}

/// 改主密码（须解锁态；DEK 重包，数据零改动）
#[tauri::command]
pub async fn vault_change_master_password(
    old_password: String,
    new_password: String,
    state: State<'_, HostState>,
) -> Result<vault_core::VaultHeader, AppError> {
    let svc = vault_service(&state)?;
    let svc2 = svc.clone();
    tauri::async_runtime::spawn_blocking(move || {
        svc2.change_master_password(&old_password, &new_password)
    })
    .await
    .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
    .inspect(|_h| {
        publish_vault_state(&state, &svc);
    })
}

// ---- V4 Windows Hello 免密（D-24）----

/// 启用免密路径（解锁态；弹一次 Hello 校验）。校验弹窗可长达数十秒 → spawn_blocking
#[tauri::command]
pub async fn vault_hello_enable(
    state: State<'_, HostState>,
) -> Result<vault_core::VaultHeader, AppError> {
    let svc = vault_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.hello_enable())
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
}

/// 关闭免密路径（头部 hello 信封移除）
#[tauri::command]
pub async fn vault_hello_disable(
    state: State<'_, HostState>,
) -> Result<vault_core::VaultHeader, AppError> {
    let svc = vault_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.hello_disable())
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
}

/// 免密解锁（Cooling / 熔断期同样拒绝；成功后前端 refresh 拉取条目）
#[tauri::command]
pub async fn vault_hello_unlock(state: State<'_, HostState>) -> Result<(), AppError> {
    let svc = vault_service(&state)?;
    let svc2 = svc.clone();
    tauri::async_runtime::spawn_blocking(move || svc2.hello_unlock())
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
        .map(|_| publish_vault_state(&state, &svc))
}

/// V5 失焦线信号：主窗口 blur/focus 上报（无窗口可见性问题：纯时间戳记录）
#[tauri::command]
pub fn vault_notify_blur(blurred: bool, state: State<'_, HostState>) -> Result<(), AppError> {
    vault_service(&state)?.set_blur(blurred);
    Ok(())
}

/// 复制密码字段到剪贴板（D-24：后端 write_back 走 D-10 回写窗口防自捕获，
/// 并按 clear_clipboard_secs 延时清除——仅当剪贴板仍是这条密码时才动它）
#[tauri::command]
pub async fn vault_copy_password(
    entry_id: String,
    field_key: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    use host_core::ports::{ClipContent, ClipboardPort};
    let svc = vault_service(&state)?;
    let clipboard = state.clipboard.clone();
    let secret = tauri::async_runtime::spawn_blocking(move || {
        let entry = svc
            .get_entry(&entry_id)?
            .ok_or_else(|| AppError::module("VAULT_IPC_003", "条目不存在", None))?;
        let field = entry
            .fields
            .iter()
            .find(|f| f.key == field_key)
            .ok_or_else(|| AppError::module("VAULT_IPC_004", "字段不存在", None))?;
        if field.value.is_empty() {
            return Err(AppError::module("VAULT_IPC_005", "字段为空", None));
        }
        let value = field.value.clone();
        clipboard.write_back(&ClipContent::Text {
            text: value.clone(),
            html: None,
        })?;
        Ok::<String, AppError>(value)
    })
    .await
    .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))??;

    // 清除延时（0/缺省=90s）；到期任务只在这条密码仍是剪贴板内容时清除
    let clear_secs = state
        .config
        .get_module("vault")
        .ok()
        .and_then(|v| {
            v.get("clear_clipboard_secs")
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(90)
        .min(600);
    if clear_secs > 0 {
        let clipboard = state.clipboard.clone();
        let ports = state.ports.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(clear_secs)).await;
            let _ = tauri::async_runtime::spawn_blocking(move || {
                // 无法判定（read_text=None）或已被替换 → 一律不动剪贴板（D-05 保守纪律）
                let still_same = vault_core::clipboard_clear_due(
                    ports
                        .get::<dyn ClipboardPort>()
                        .and_then(|p| p.read_text())
                        .as_deref(),
                    &secret,
                );
                if still_same {
                    let _ = clipboard.write_back(&ClipContent::Text {
                        text: String::new(),
                        html: None,
                    });
                }
            })
            .await;
        });
    }
    Ok(())
}

// ---- 文件夹 ----

#[tauri::command]
pub async fn vault_folders(
    state: State<'_, HostState>,
) -> Result<Vec<vault_core::Folder>, AppError> {
    let svc = vault_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.list_folders())
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
}

#[tauri::command]
pub async fn vault_folder_create(
    name: String,
    state: State<'_, HostState>,
) -> Result<vault_core::Folder, AppError> {
    let svc = vault_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.create_folder(&name))
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
        .inspect(|f| {
            publish_vault_entries(&state, "folder_created", Some(&f.id));
        })
}

#[tauri::command]
pub async fn vault_folder_rename(
    id: String,
    name: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let svc = vault_service(&state)?;
    let id2 = id.clone();
    tauri::async_runtime::spawn_blocking(move || svc.rename_folder(&id2, &name))
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
        .inspect(|&ok| {
            if ok {
                publish_vault_entries(&state, "folder_renamed", Some(&id));
            }
        })
}

/// 删除文件夹（条目保留，folder_id 置空）
#[tauri::command]
pub async fn vault_folder_delete(
    id: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let svc = vault_service(&state)?;
    let id2 = id.clone();
    tauri::async_runtime::spawn_blocking(move || svc.delete_folder(&id2))
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
        .inspect(|&ok| {
            if ok {
                publish_vault_entries(&state, "folder_deleted", Some(&id));
            }
        })
}

// ---- 条目 ----

/// 条目列表（folder_id None = 全部；search 按 title LIKE）
#[tauri::command]
pub async fn vault_entries(
    folder_id: Option<String>,
    search: Option<String>,
    state: State<'_, HostState>,
) -> Result<Vec<vault_core::Entry>, AppError> {
    let svc = vault_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        svc.list_entries(folder_id.as_deref(), search.as_deref())
    })
    .await
    .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
}

#[tauri::command]
pub async fn vault_entry_get(
    id: String,
    state: State<'_, HostState>,
) -> Result<Option<vault_core::Entry>, AppError> {
    let svc = vault_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.get_entry(&id))
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
}

#[tauri::command]
pub async fn vault_entry_add(
    title: String,
    folder_id: Option<String>,
    favorite: bool,
    fields: Vec<vault_core::EntryField>,
    totp_secret: Option<String>,
    state: State<'_, HostState>,
) -> Result<vault_core::Entry, AppError> {
    let svc = vault_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        svc.add_entry(folder_id, &title, favorite, fields, totp_secret)
    })
    .await
    .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
    .inspect(|e| {
        publish_vault_entries(&state, "entry_added", Some(&e.id));
    })
}

/// 更新条目（按 entry.id 整体覆盖）
#[tauri::command]
pub async fn vault_entry_update(
    entry: vault_core::Entry,
    state: State<'_, HostState>,
) -> Result<vault_core::Entry, AppError> {
    let svc = vault_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.update_entry(entry))
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
        .inspect(|e| {
            publish_vault_entries(&state, "entry_updated", Some(&e.id));
        })
}

#[tauri::command]
pub async fn vault_entry_delete(id: String, state: State<'_, HostState>) -> Result<bool, AppError> {
    let svc = vault_service(&state)?;
    let id2 = id.clone();
    tauri::async_runtime::spawn_blocking(move || svc.delete_entry(&id2))
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
        .inspect(|&ok| {
            if ok {
                publish_vault_entries(&state, "entry_deleted", Some(&id));
            }
        })
}

// ---- 工具：生成器 / TOTP ----

#[tauri::command]
pub fn vault_generate_password(policy: vault_core::PasswordPolicy) -> Result<String, AppError> {
    vault_core::generate_password(&policy)
}

/// 当前 TOTP 码 + 剩余秒数（前端 rAF 环形进度用 remaining 自算倒计时）
#[tauri::command]
pub fn vault_totp_now(secret: String) -> Result<(String, u64), AppError> {
    vault_core::totp_now(&secret)
}

#[cfg(test)]
mod tests {
    use super::vault_state_str;
    use vault_core::VaultState;

    #[test]
    fn state_maps_to_stable_wire_strings() {
        assert_eq!(vault_state_str(VaultState::Uninitialized), "uninitialized");
        assert_eq!(vault_state_str(VaultState::Locked), "locked");
        assert_eq!(vault_state_str(VaultState::Unlocked), "unlocked");
    }
}
