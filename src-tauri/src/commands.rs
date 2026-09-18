//! 宿主级 IPC 命令（docs/impl/01 S7；模块命令在各自模块实现）
//!
//! 命名规范：`{module}_{action}` snake_case；
//! 错误统一 `Result<T, AppError>`（S2 序列化契约）。

use host_core::error::AppError;
use serde::Serialize;
use tauri::State;

use crate::state::{HostState, ModuleStatusDto};

/// 读取 Windows 系统强调色（docs/UI-PLAN.md U1-3）。
/// 失败时前端回退默认 Windows 蓝渐变。
#[tauri::command]
pub fn host_system_accent() -> Result<String, AppError> {
    win_integration::accent::system_accent_color().map(|c| c.to_hex())
}

/// 全部模块元信息与状态（docs/impl/01 S7）
#[tauri::command]
pub fn host_modules_status(state: State<'_, HostState>) -> Vec<ModuleStatusDto> {
    let infos = state.registry.infos();
    let status: std::collections::HashMap<String, host_core::module::ModuleState> =
        state.registry.status_all().into_iter().collect();
    infos
        .into_iter()
        .map(|info| ModuleStatusDto {
            state: status.get(info.id).copied().unwrap_or(host_core::module::ModuleState::Uninitialized),
            id: info.id.to_owned(),
            name: info.name.to_owned(),
            version: info.version.to_owned(),
            priority: info.priority,
        })
        .collect()
}

/// 重启模块（stop → init → start；panic 后恢复入口）
#[tauri::command]
pub async fn host_module_restart(id: String, state: State<'_, HostState>) -> Result<(), AppError> {
    state.registry.restart(&id).await
}

/// 读模块配置
#[tauri::command]
pub fn host_config_get(module: String, state: State<'_, HostState>) -> Result<serde_json::Value, AppError> {
    state.config.get_module(&module)
}

/// 读模块配置 schema（设置中心自动渲染）
#[tauri::command]
pub fn host_config_schema(module: String, state: State<'_, HostState>) -> Result<serde_json::Value, AppError> {
    state
        .config
        .schema_of(&module)
        .ok_or_else(|| AppError::module("HOST_CONFIG_003", "模块未注册 schema", None))
}

/// 写模块配置（schema 校验 → 备份 → 原子写 → host.config_changed 事件）
#[tauri::command]
pub fn host_config_set(
    module: String,
    values: serde_json::Value,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state.config.set_module(&module, values)
}

// ---------------- 剪切板命令（docs/impl/02 C7）----------------
// DB 为 Mutex<Connection>（阻塞），统一 spawn_blocking 执行

/// FTS 搜索 / 分页
#[tauri::command]
pub async fn clipboard_search(
    query: clipboard_core::types::SearchQuery,
    state: State<'_, HostState>,
) -> Result<clipboard_core::types::Page<clipboard_core::types::ClipEntry>, AppError> {
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || clipboard.search(&query))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))?
}

/// 解密读取条目内容（secret 条目经 DPAPI 还原）
#[tauri::command]
pub async fn clipboard_get(id: String, state: State<'_, HostState>) -> Result<String, AppError> {
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || clipboard.get_content(&id))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))?
        .map(|c| c.unwrap_or_default())
}

/// 写回系统剪贴板（先置回写窗口，防自捕获循环；文本/图片/文件多格式）
#[tauri::command]
pub async fn clipboard_paste(id: String, state: State<'_, HostState>) -> Result<(), AppError> {
    use clipboard_core::store::Payload;
    use host_core::ports::ClipContent;
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let payload = clipboard
            .get_payload(&id)?
            .ok_or_else(|| AppError::module("CLIPBOARD_PASTE_002", "条目不存在", None))?;
        let content = match payload {
            Payload::Text(text) => ClipContent::Text { text, html: None },
            Payload::Files(paths) => ClipContent::Files { paths },
            Payload::Image { format, bytes } => ClipContent::Image {
                format,
                width: 0,
                height: 0,
                bytes: std::sync::Arc::from(bytes.into_boxed_slice()),
            },
            Payload::SecretB64(_) => {
                return Err(AppError::module("CLIPBOARD_PASTE_004", "加密条目状态异常", None))
            }
        };
        clipboard.write_back(&content)
    })
    .await
    .map_err(|e| AppError::module("CLIPBOARD_PASTE_003", e.to_string(), None))?
}

/// 图片条目字节（Base64 DIB），前端 canvas 解码预览用
#[tauri::command]
pub async fn clipboard_get_image(id: String, state: State<'_, HostState>) -> Result<String, AppError> {
    use clipboard_core::store::Payload;
    use base64::Engine;
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || {
        match clipboard.get_payload(&id)? {
            Some(Payload::Image { bytes, .. }) => Ok(base64::engine::general_purpose::STANDARD.encode(bytes)),
            _ => Err(AppError::module("CLIPBOARD_QUERY_004", "条目不是图片", None)),
        }
    })
    .await
    .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))?
}

#[tauri::command]
pub async fn clipboard_pin(id: String, pinned: bool, state: State<'_, HostState>) -> Result<(), AppError> {
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || clipboard.pin(&id, pinned))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))?
}

#[tauri::command]
pub async fn clipboard_delete(id: String, state: State<'_, HostState>) -> Result<(), AppError> {
    let clipboard = state.clipboard.clone();
    let id2 = id.clone();
    tauri::async_runtime::spawn_blocking(move || clipboard.delete(&id2))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))??;
    state
        .bus
        .publish(host_core::events::Event::new(
            "clipboard.deleted",
            "clipboard",
            serde_json::json!({ "id": id }),
        ))
        .ok();
    Ok(())
}

#[tauri::command]
pub async fn clipboard_clear(keep_pinned: bool, state: State<'_, HostState>) -> Result<u32, AppError> {
    let clipboard = state.clipboard.clone();
    let n = tauri::async_runtime::spawn_blocking(move || clipboard.clear(keep_pinned))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))??;
    state
        .bus
        .publish(host_core::events::Event::new(
            "clipboard.cleared",
            "clipboard",
            serde_json::json!({ "removed": n }),
        ))
        .ok();
    Ok(n)
}

/// 分组计数（SubNav 角标）
#[tauri::command]
pub async fn clipboard_group_counts(state: State<'_, HostState>) -> Result<serde_json::Value, AppError> {
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || clipboard.group_counts())
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))?
}

/// 前端日志上报（webview console 外部不可见；关键异步失败经此进入宿主日志）
#[tauri::command]
pub fn host_log(level: String, message: String) {
    match level.as_str() {
        "error" => tracing::error!(target: "webview", "{message}"),
        "warn" => tracing::warn!(target: "webview", "{message}"),
        _ => tracing::info!(target: "webview", "{message}"),
    }
}

// ---------------- 截图命令（docs/impl/03 P8）----------------
// GDI 捕获 / PNG 编解码为阻塞调用，统一 spawn_blocking

/// 启动截图任务：抓全屏帧并返回覆盖层定位信息
#[tauri::command]
pub async fn screenshot_start(
    mode: String,
    state: State<'_, HostState>,
) -> Result<screenshot_core::types::TaskStartDto, AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.start_capture(&mode))
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))?
}

/// 覆盖层取背景帧（PNG Base64，编码一次后缓存）
#[tauri::command]
pub async fn screenshot_task(
    task_id: String,
    state: State<'_, HostState>,
) -> Result<screenshot_core::types::TaskInfoDto, AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.task_info(&task_id))
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))?
}

/// 选区确认：裁剪 + 编码
#[tauri::command]
pub async fn screenshot_confirm(
    task_id: String,
    rect: screenshot_core::types::ConfirmRect,
    state: State<'_, HostState>,
) -> Result<screenshot_core::types::CropDto, AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.confirm(&task_id, rect))
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))?
}

/// 取消 / 隐藏时丢弃任务帧
#[tauri::command]
pub fn screenshot_discard(task_id: String, state: State<'_, HostState>) {
    state.screenshot.discard(&task_id);
}

/// 完成：解码前端合成图 → 动作（copy/save/pin）→ 历史 → 事件
#[tauri::command]
pub async fn screenshot_finish(
    task_id: String,
    request: screenshot_core::types::FinishRequest,
    state: State<'_, HostState>,
) -> Result<screenshot_core::types::FinishDto, AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.finish(&task_id, &request))
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))?
}

/// 截图历史分页
#[tauri::command]
pub async fn screenshot_history_list(
    query: screenshot_core::types::HistoryQuery,
    state: State<'_, HostState>,
) -> Result<screenshot_core::types::Page<screenshot_core::types::ShotItem>, AppError> {
    let screenshot = state.screenshot.clone();
    let store = screenshot.history_store();
    tauri::async_runtime::spawn_blocking(move || {
        let store = store
            .ok_or_else(|| AppError::module("SCREENSHOT_STATE_001", "模块未就绪", None))?;
        store.list(&query)
    })
    .await
    .map_err(|e| AppError::module("SCREENSHOT_QUERY_002", e.to_string(), None))?
}

/// 全部 Pin 贴图（主窗口启动恢复用）
#[tauri::command]
pub fn screenshot_pins(state: State<'_, HostState>) -> Vec<screenshot_core::types::PinDto> {
    state.screenshot.pins()
}

/// 单个贴图数据（Pin 窗口加载用）
#[tauri::command]
pub async fn screenshot_pin_get(
    id: String,
    state: State<'_, HostState>,
) -> Result<screenshot_core::types::PinDataDto, AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.pin_get(&id))
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))?
}

/// 贴图缩放 / 透明度持久化
#[tauri::command]
pub async fn screenshot_pin_update(
    id: String,
    zoom: f32,
    opacity: f32,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.pin_update(&id, zoom, opacity))
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))?
}

/// 关闭贴图（删除记录 + 图片文件）
#[tauri::command]
pub async fn screenshot_pin_close(id: String, state: State<'_, HostState>) -> Result<(), AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.pin_close(&id))
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))?
}

// ---------------- OCR 命令（docs/impl/04 O7）----------------

/// 识别（PNG Base64 输入；spawn_blocking + 30s 超时）
#[tauri::command]
pub async fn ocr_recognize(
    request: ocr_core::types::OcrRequest,
    state: State<'_, HostState>,
) -> Result<ocr_core::types::OcrResultDto, AppError> {
    let ocr = state.ocr.clone();
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = ocr.recognize(&request)?;
        // 关联截图任务的 OCR 文本回填历史
        if let Some(task_id) = &request.source_task_id {
            screenshot.record_ocr_text(task_id, &result.text);
        }
        Ok(result)
    })
    .await
    .map_err(|e| AppError::module("OCR_RUN_004", e.to_string(), None))?
}

/// 引擎可用性与语言列表（docs/impl/04 O8）
#[tauri::command]
pub fn ocr_engine_status(state: State<'_, HostState>) -> ocr_core::types::EngineStatusDto {
    state.ocr.status()
}

/// OCR 结果"复制全部"（走 ClipboardPort，写入剪贴板并进入剪贴板历史）
#[tauri::command]
pub async fn ocr_copy_text(text: String, state: State<'_, HostState>) -> Result<(), AppError> {
    use host_core::ports::ClipContent;
    let screenshot = state.screenshot.clone();
    let bus = state.bus.clone();
    tauri::async_runtime::spawn_blocking(move || {
        screenshot.copy_text(&ClipContent::Text { text, html: None })
    })
    .await
    .map_err(|e| AppError::module("OCR_RUN_004", e.to_string(), None))??;
    bus.publish(host_core::events::Event::new(
        "ocr.completed",
        "ocr",
        serde_json::json!({ "action": "copied" }),
    ))
    .ok();
    Ok(())
}

// ---------------- KVM 键鼠共享命令（docs/impl/05 K8）----------------

/// ModuleError → AppError（IPC 层统一错误壳）
fn kvm_err(e: host_core::error::ModuleError) -> AppError {
    AppError::module("KVM_IPC_001", e.to_string(), None)
}

/// 签发一次性配对码（6 位，2 分钟有效）
#[tauri::command]
pub fn kvm_issue_pair_code(state: State<'_, HostState>) -> Result<(String, u64), AppError> {
    state.kvm.issue_pair_code().map_err(kvm_err)
}

/// 向已发现设备发起配对（阻塞握手 ≤10s → spawn_blocking）
#[tauri::command]
pub async fn kvm_pair_with(
    addr: String,
    code: String,
    state: State<'_, HostState>,
) -> Result<kvm_core::PairedPeer, AppError> {
    let addr: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| AppError::module("KVM_IPC_002", format!("地址非法: {e}"), None))?;
    let kvm = state.kvm.clone();
    tauri::async_runtime::spawn_blocking(move || kvm.pair_with(addr, &code))
        .await
        .map_err(|e| AppError::module("KVM_IPC_003", e.to_string(), None))?
        .map_err(kvm_err)
}

/// 解除配对
#[tauri::command]
pub fn kvm_unpair(device_id: String, state: State<'_, HostState>) -> Result<bool, AppError> {
    state.kvm.unpair(&device_id).map_err(kvm_err)
}

/// 已配对设备列表
#[tauri::command]
pub fn kvm_paired_peers(state: State<'_, HostState>) -> Result<Vec<kvm_core::PairedPeer>, AppError> {
    state.kvm.paired_peers().map_err(kvm_err)
}

/// 已发现邻居列表（心跳快照）
#[tauri::command]
pub fn kvm_discovered_peers(
    state: State<'_, HostState>,
) -> Result<Vec<kvm_core::PeerInfo>, AppError> {
    state.kvm.discovered_peers().map_err(kvm_err)
}

/// 向已配对设备发起会话（客户端角色；阻塞握手 ≤10s → spawn_blocking）
#[tauri::command]
pub async fn kvm_connect_to(addr: String, state: State<'_, HostState>) -> Result<String, AppError> {
    let addr: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| AppError::module("KVM_IPC_002", format!("地址非法: {e}"), None))?;
    let kvm = state.kvm.clone();
    tauri::async_runtime::spawn_blocking(move || kvm.connect_to(addr))
        .await
        .map_err(|e| AppError::module("KVM_IPC_003", e.to_string(), None))?
        .map_err(kvm_err)
}

/// 发送剪贴板内容到对端（Text/Image 单帧；Files 逐文件走 send_file）
#[tauri::command]
pub async fn kvm_send_clip(
    device_id: String,
    content: host_core::ports::ClipContent,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let kvm = state.kvm.clone();
    tauri::async_runtime::spawn_blocking(move || kvm.send_clip(&device_id, content))
        .await
        .map_err(|e| AppError::module("KVM_IPC_003", e.to_string(), None))?
        .map_err(kvm_err)
}

/// 发送本地文件到对端（后台传输；进度/回执走事件）
#[tauri::command]
pub fn kvm_send_file(
    device_id: String,
    path: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state.kvm.send_file(&device_id, path).map_err(kvm_err)
}

/// 活跃会话列表（服务端接入 + 本端发起）
#[tauri::command]
pub fn kvm_session_list(state: State<'_, HostState>) -> Vec<serde_json::Value> {
    state.kvm.session_list()
}

/// 设置 [设备→共享边] 映射（值 "left" | "right"；即时生效）
#[tauri::command]
pub fn kvm_set_edge_map(
    map: std::collections::HashMap<String, String>,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state.kvm.set_edge_map(map).map_err(kvm_err)
}

/// 当前边缘映射
#[tauri::command]
pub fn kvm_edge_map(state: State<'_, HostState>) -> std::collections::HashMap<String, String> {
    state.kvm.edge_map()
}

/// 控制状态（role: idle/controlling/controlled）
#[tauri::command]
pub fn kvm_control_state(state: State<'_, HostState>) -> serde_json::Value {
    state.kvm.control_state()
}

/// 手动释放控制权（UI 切回按钮）
#[tauri::command]
pub fn kvm_release_control(state: State<'_, HostState>) -> Result<(), AppError> {
    state.kvm.release_control().map_err(kvm_err)
}

// ---------------- 密码库命令（docs/impl/05 V7）----------------
// Argon2（数百 ms～秒级）与 SQLite 均为阻塞调用，统一 spawn_blocking

/// 密码库状态（前端三态渲染：uninitialized / locked / unlocked）
#[derive(Serialize)]
pub struct VaultStatusDto {
    pub state: String,
    pub lockout_remaining_secs: u64,
    /// 头部快照（KDF 参数 / vault_id，无机密）
    pub kdf: Option<vault_core::VaultHeader>,
}

fn vault_service(state: &HostState) -> Result<std::sync::Arc<vault_core::VaultService>, AppError> {
    state
        .vault
        .service()
        .ok_or_else(|| AppError::module("VAULT_IPC_001", "密码库模块未就绪", None))
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
pub fn vault_status(state: State<'_, HostState>) -> Result<VaultStatusDto, AppError> {
    let svc = vault_service(&state)?;
    Ok(VaultStatusDto {
        state: vault_state_str(svc.state()).into(),
        lockout_remaining_secs: svc.lockout_remaining_secs(),
        kdf: svc.header(),
    })
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
        .map(|h| {
            publish_vault_state(&state, &svc);
            h
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
    .map(|h| {
        publish_vault_state(&state, &svc);
        h
    })
}

// ---- 文件夹 ----

#[tauri::command]
pub async fn vault_folders(state: State<'_, HostState>) -> Result<Vec<vault_core::Folder>, AppError> {
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
        .map(|f| {
            publish_vault_entries(&state, "folder_created", Some(&f.id));
            f
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
        .map(|ok| {
            if ok {
                publish_vault_entries(&state, "folder_renamed", Some(&id));
            }
            ok
        })
}

/// 删除文件夹（条目保留，folder_id 置空）
#[tauri::command]
pub async fn vault_folder_delete(id: String, state: State<'_, HostState>) -> Result<bool, AppError> {
    let svc = vault_service(&state)?;
    let id2 = id.clone();
    tauri::async_runtime::spawn_blocking(move || svc.delete_folder(&id2))
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
        .map(|ok| {
            if ok {
                publish_vault_entries(&state, "folder_deleted", Some(&id));
            }
            ok
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
    .map(|e| {
        publish_vault_entries(&state, "entry_added", Some(&e.id));
        e
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
        .map(|e| {
            publish_vault_entries(&state, "entry_updated", Some(&e.id));
            e
        })
}

#[tauri::command]
pub async fn vault_entry_delete(id: String, state: State<'_, HostState>) -> Result<bool, AppError> {
    let svc = vault_service(&state)?;
    let id2 = id.clone();
    tauri::async_runtime::spawn_blocking(move || svc.delete_entry(&id2))
        .await
        .map_err(|e| AppError::module("VAULT_IPC_002", e.to_string(), None))?
        .map(|ok| {
            if ok {
                publish_vault_entries(&state, "entry_deleted", Some(&id));
            }
            ok
        })
}

// ---- 工具：生成器 / TOTP ----

#[tauri::command]
pub fn vault_generate_password(
    policy: vault_core::PasswordPolicy,
) -> Result<String, AppError> {
    vault_core::generate_password(&policy)
}

/// 当前 TOTP 码 + 剩余秒数（前端 rAF 环形进度用 remaining 自算倒计时）
#[tauri::command]
pub fn vault_totp_now(secret: String) -> Result<(String, u64), AppError> {
    vault_core::totp_now(&secret)
}

// ---------------- 文件与存储命令（docs/impl/05 F，M6）----------------
// 全部为阻塞 IO，统一 spawn_blocking 执行

#[derive(Serialize)]
pub struct FileEnqueueDto {
    /// None = Ask 策略发现冲突未入队
    pub op_id: Option<String>,
    pub conflicts: Vec<file_core::ConflictItem>,
}

fn file_service(state: &HostState) -> Result<std::sync::Arc<file_core::FileService>, AppError> {
    state
        .file
        .service()
        .ok_or_else(|| AppError::module("FILE_IPC_001", "文件模块未就绪", None))
}

fn file_err(e: file_core::FileError) -> AppError {
    AppError::module(e.code(), e.to_string(), None)
}

/// 盘符列表（F1）
#[tauri::command]
pub async fn file_drives(state: State<'_, HostState>) -> Result<Vec<file_core::DriveInfo>, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.drives()))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
}

/// 列目录（F1）
#[tauri::command]
pub async fn file_list(
    path: std::path::PathBuf,
    sort: Option<file_core::SortKey>,
    asc: Option<bool>,
    state: State<'_, HostState>,
) -> Result<Vec<file_core::FileEntry>, AppError> {
    let svc = file_service(&state)?;
    let sort = sort.unwrap_or(file_core::SortKey::Name);
    let asc = asc.unwrap_or(true);
    tauri::async_runtime::spawn_blocking(move || svc.list_dir(&path, sort, asc))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 面包屑（F1）
#[tauri::command]
pub async fn file_breadcrumbs(
    path: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<Vec<(String, std::path::PathBuf)>, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.breadcrumbs(&path)))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
}

/// 新建目录（F1）
#[tauri::command]
pub async fn file_mkdir(
    path: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.mkdir(&path))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 重命名/移动单条目（F1 本地驱动）
#[tauri::command]
pub async fn file_rename_entry(
    from: std::path::PathBuf,
    to: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.rename_entry(&from, &to))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 入队文件操作（F2/F3；Ask 冲突预扫描）
#[tauri::command]
pub async fn file_enqueue(
    spec: file_core::OpSpec,
    state: State<'_, HostState>,
) -> Result<FileEnqueueDto, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.enqueue(spec).map(|(op_id, conflicts)| FileEnqueueDto { op_id, conflicts }))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 活跃/近期操作（F2）
#[tauri::command]
pub async fn file_ops_active(state: State<'_, HostState>) -> Result<Vec<file_core::OpProgress>, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.ops_active()))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
}

/// 崩溃恢复扫描：未完成操作（F2，docs/impl/01 S6.5）
#[tauri::command]
pub async fn file_ops_pending(state: State<'_, HostState>) -> Result<Vec<file_core::PendingOp>, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.ops_pending()))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
}

/// 暂停操作（块边界生效）
#[tauri::command]
pub async fn file_op_pause(op_id: String, state: State<'_, HostState>) -> Result<(), AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.op_pause(&op_id))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 恢复操作（返回新 op_id）
#[tauri::command]
pub async fn file_op_resume(op_id: String, state: State<'_, HostState>) -> Result<String, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.op_resume(&op_id))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 取消操作
#[tauri::command]
pub async fn file_op_cancel(op_id: String, state: State<'_, HostState>) -> Result<(), AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.op_cancel(&op_id))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 丢弃 pending 记录
#[tauri::command]
pub async fn file_op_drop_pending(
    op_id: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.op_drop_pending(&op_id))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 预览（F4：文本片段/图片缩略图/Shell 系统缩略图）
#[tauri::command]
pub async fn file_preview(
    path: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<file_core::Preview, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.preview(&path))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 全局搜索（F5：USN 优先，无权限自动降级目录遍历）
#[tauri::command]
pub async fn file_search(
    query: String,
    limit: Option<u32>,
    root: Option<std::path::PathBuf>,
    state: State<'_, HostState>,
) -> Result<file_core::SearchResult, AppError> {
    let svc = file_service(&state)?;
    let opts = file_core::SearchOpts {
        query,
        limit: limit.unwrap_or(50),
        root,
        max_depth: 6,
    };
    tauri::async_runtime::spawn_blocking(move || svc.search(&opts))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 存储驱动列表（F6）
#[tauri::command]
pub async fn file_drivers(state: State<'_, HostState>) -> Result<Vec<file_core::DriverInfo>, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.drivers()))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
}

/// 批量重命名预览（F7；前端先预览前 20 条再应用）
#[tauri::command]
pub async fn file_rename_plan(
    dir: std::path::PathBuf,
    names: Vec<String>,
    rule: file_core::RenameRule,
    state: State<'_, HostState>,
) -> Result<Vec<file_core::RenamePlan>, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.rename_plan(&dir, &names, &rule))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 批量重命名应用（F7；冲突条目跳过）
#[tauri::command]
pub async fn file_rename_apply(
    plans: Vec<file_core::RenamePlan>,
    state: State<'_, HostState>,
) -> Result<usize, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.rename_apply(&plans))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

// ======================== 代理（M7 PR，docs/impl/05） ========================

fn proxy_service(state: &HostState) -> Result<std::sync::Arc<proxy_core::ProxyService>, AppError> {
    state.proxy.service().ok_or_else(|| {
        AppError::module(
            "PROXY_STATE_001",
            "代理模块尚未初始化",
            Some("请等待模块启动完成后重试"),
        )
    })
}

fn proxy_err(e: proxy_core::ProxyError) -> AppError {
    AppError::from(e)
}

/// 代理总状态（模式/内核运行/安装清单/管理员/TUN 前置/备份标记）
#[tauri::command]
pub async fn proxy_status(state: State<'_, HostState>) -> Result<proxy_core::StatusDto, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.status()))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
}

/// 安装/更新 sing-box 内核（官方 Release 直链；version 空则用默认版本）
#[tauri::command]
pub async fn proxy_kernel_install(
    version: Option<String>,
    state: State<'_, HostState>,
) -> Result<proxy_core::Manifest, AppError> {
    let svc = proxy_service(&state)?;
    svc.kernel_install(version).await.map_err(proxy_err)
}

/// 安装 wintun.dll（TUN 模式前置）
#[tauri::command]
pub async fn proxy_wintun_install(state: State<'_, HostState>) -> Result<(), AppError> {
    let svc = proxy_service(&state)?;
    svc.wintun_install().await.map_err(proxy_err)
}

/// 订阅列表
#[tauri::command]
pub async fn proxy_subs(state: State<'_, HostState>) -> Result<Vec<proxy_core::Sub>, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.subs()))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
}

/// 添加订阅（不自动拉取，随后调用 proxy_sub_update）
#[tauri::command]
pub async fn proxy_sub_add(
    name: String,
    url: String,
    state: State<'_, HostState>,
) -> Result<proxy_core::Sub, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.sub_add(&name, &url))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
        .map_err(proxy_err)
}

/// 删除订阅（连其节点一并清除）
#[tauri::command]
pub async fn proxy_sub_remove(
    id: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.sub_remove(&id))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
        .map_err(proxy_err)
}

/// 拉取并解析订阅（内容持久化，不进日志）
#[tauri::command]
pub async fn proxy_sub_update(
    id: String,
    state: State<'_, HostState>,
) -> Result<proxy_core::Sub, AppError> {
    let svc = proxy_service(&state)?;
    svc.sub_update(&id).await.map_err(proxy_err)
}

/// 节点列表（全部订阅聚合）
#[tauri::command]
pub async fn proxy_nodes(state: State<'_, HostState>) -> Result<Vec<proxy_core::NodeDto>, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.nodes()))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
}

/// 直连域名规则
#[tauri::command]
pub async fn proxy_direct_rules(state: State<'_, HostState>) -> Result<Vec<String>, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.direct_rules()))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
}

/// 设置直连域名规则（trim + 去空 + 去重；模式重新切换后生效）
#[tauri::command]
pub async fn proxy_set_direct_rules(
    rules: Vec<String>,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.set_direct_rules(rules))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
        .map_err(proxy_err)
}

/// 切换模式：off（停内核+还原）/ system（内核+系统代理）/ tun（内核+TUN，需管理员）
#[tauri::command]
pub async fn proxy_set_mode(
    mode: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let svc = proxy_service(&state)?;
    let mode = proxy_core::Mode::parse(&mode).map_err(proxy_err)?;
    tauri::async_runtime::spawn_blocking(move || svc.set_mode(mode))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
        .map_err(proxy_err)
}

/// 节点 TCP 连通性测试（3s 超时，并发）
#[tauri::command]
pub async fn proxy_delay_test(
    state: State<'_, HostState>,
) -> Result<Vec<proxy_core::NodeDelayDto>, AppError> {
    let svc = proxy_service(&state)?;
    Ok(svc.delay_test().await)
}

/// 内核日志（环形缓冲快照，limit 条）
#[tauri::command]
pub async fn proxy_logs(
    limit: Option<usize>,
    state: State<'_, HostState>,
) -> Result<Vec<proxy_core::LogLine>, AppError> {
    let svc = proxy_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.logs(limit.unwrap_or(100))))
        .await
        .map_err(|e| AppError::module("PROXY_IPC_001", e.to_string(), None))?
}

// ======================== 桌面效率（M8 D，docs/impl/05） ========================

fn desktop_module(state: &HostState) -> std::sync::Arc<desktop_core::DesktopModule> {
    state.desktop.clone()
}

fn desktop_err(e: desktop_core::DesktopError) -> AppError {
    AppError::from(e)
}

/// 启动器搜索（D1+D2 打分排序）
#[tauri::command]
pub async fn desktop_launcher_search(
    query: String,
    state: State<'_, HostState>,
) -> Result<Vec<desktop_core::LauncherHit>, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || m.index().search(&query, 20))
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
        .map_err(desktop_err)
}

/// 启动条目（App → ShellExecuteW；Action → 发事件；记频次）
#[tauri::command]
pub async fn desktop_launcher_launch(id: String, state: State<'_, HostState>) -> Result<(), AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || m.launch(&id))
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
        .map_err(|e| AppError::from(e))
}

/// 索引状态（是否就绪 + 条目数）
#[tauri::command]
pub async fn desktop_launcher_status(
    state: State<'_, HostState>,
) -> Result<(bool, usize), AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || Ok(m.index().status()))
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
}

/// 桌面整理预览（D3）
#[tauri::command]
pub async fn desktop_tidy_plan(state: State<'_, HostState>) -> Result<desktop_core::TidyPlan, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || m.tidy_planner().plan(m.desktop_dir()))
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
        .map_err(desktop_err)
}

/// 执行桌面整理（返回移动数/跳过数）
#[tauri::command]
pub async fn desktop_tidy_apply(
    state: State<'_, HostState>,
) -> Result<(usize, usize), AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || m.tidy_planner().apply(m.desktop_dir()))
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
        .map_err(desktop_err)
}

/// 还原上次整理
#[tauri::command]
pub async fn desktop_tidy_restore(state: State<'_, HostState>) -> Result<usize, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || m.tidy_planner().restore())
        .await
        .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
        .map_err(desktop_err)
}

/// 是否存在待还原的整理记录
#[tauri::command]
pub async fn desktop_tidy_status(state: State<'_, HostState>) -> Result<bool, AppError> {
    let m = desktop_module(&state);
    Ok(m.tidy_planner().has_manifest())
}

/// 新增随记（D4：#标签 + 提醒解析在 core 内）
#[tauri::command]
pub async fn desktop_note_add(
    content: String,
    state: State<'_, HostState>,
) -> Result<desktop_core::Note, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let store = m.note_store().ok_or_else(|| {
            desktop_core::DesktopError::BadState("随记库未初始化".into())
        })?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        store.add(&content, now)
    })
    .await
    .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
    .map_err(desktop_err)
}

/// 随记列表
#[tauri::command]
pub async fn desktop_note_list(
    include_done: bool,
    state: State<'_, HostState>,
) -> Result<Vec<desktop_core::Note>, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let store = m.note_store().ok_or_else(|| {
            desktop_core::DesktopError::BadState("随记库未初始化".into())
        })?;
        store.list(include_done)
    })
    .await
    .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
    .map_err(desktop_err)
}

/// 随记完成/未完成
#[tauri::command]
pub async fn desktop_note_done(
    id: String,
    done: bool,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let store = m.note_store().ok_or_else(|| {
            desktop_core::DesktopError::BadState("随记库未初始化".into())
        })?;
        store.set_done(&id, done)
    })
    .await
    .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
    .map_err(desktop_err)
}

/// 删除随记
#[tauri::command]
pub async fn desktop_note_remove(id: String, state: State<'_, HostState>) -> Result<bool, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let store = m.note_store().ok_or_else(|| {
            desktop_core::DesktopError::BadState("随记库未初始化".into())
        })?;
        store.remove(&id)
    })
    .await
    .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
    .map_err(desktop_err)
}

/// 手动拉取到期提醒（后台轮询之外的补充路径）
#[tauri::command]
pub async fn desktop_notes_due(state: State<'_, HostState>) -> Result<Vec<desktop_core::Note>, AppError> {
    let m = desktop_module(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let store = m.note_store().ok_or_else(|| {
            desktop_core::DesktopError::BadState("随记库未初始化".into())
        })?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        store.take_due(now)
    })
    .await
    .map_err(|e| AppError::module("DESKTOP_IPC_001", e.to_string(), None))?
    .map_err(desktop_err)
}

// ======================== 文本与 PDF（M9 E，docs/impl/06） ========================

fn editor_err(e: editor_core::EditorError) -> AppError {
    AppError::from(e)
}

/// 打开文件会话（E1：编码检测 + EOL 检测 + 大文件标志）
#[tauri::command]
pub async fn editor_open(
    path: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<editor_core::SessionInfo, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || m.sessions().open(&path))
        .await
        .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
        .map_err(editor_err)
}

/// 取会话内容（打开后拉取一次）
#[tauri::command]
pub async fn editor_content(
    id: String,
    state: State<'_, HostState>,
) -> Result<String, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || m.sessions().content(&id))
        .await
        .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
        .map_err(editor_err)
}

/// 更新内容置脏（编辑器 onChange 不调——autosave 兼职更新；此命令供显式更新）
#[tauri::command]
pub async fn editor_update(
    id: String,
    content: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || m.sessions().update(&id, &content))
        .await
        .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
        .map_err(editor_err)
}

/// 保存（保持原编码 + EOL 统一 + 清理草稿）
#[tauri::command]
pub async fn editor_save(
    id: String,
    state: State<'_, HostState>,
) -> Result<editor_core::SessionInfo, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || m.sessions().save(&id))
        .await
        .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
        .map_err(editor_err)
}

/// 另存为
#[tauri::command]
pub async fn editor_save_as(
    id: String,
    target: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<editor_core::SessionInfo, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || m.sessions().save_as(&id, &target))
        .await
        .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
        .map_err(editor_err)
}

/// 自动保存草稿（前端 3s 防抖调用；写 `<path>.nforge-autosave`）
#[tauri::command]
pub async fn editor_autosave(
    id: String,
    content: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || m.sessions().autosave(&id, &content))
        .await
        .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
        .map_err(editor_err)
}

/// 关闭会话（清理草稿；返回关闭时是否脏——供 UI 提示）
#[tauri::command]
pub async fn editor_close(id: String, state: State<'_, HostState>) -> Result<bool, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || m.sessions().close(&id))
        .await
        .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
        .map_err(editor_err)
}

/// 会话列表
#[tauri::command]
pub async fn editor_sessions(
    state: State<'_, HostState>,
) -> Result<Vec<editor_core::SessionInfo>, AppError> {
    let m = state.editor.clone();
    Ok(m.sessions().list())
}

/// PDF 基本信息
#[tauri::command]
pub async fn pdf_info(
    path: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<editor_core::PdfInfo, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || editor_core::pdf::info(&path))
        .await
        .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
        .map_err(editor_err)
}

/// PDF 合并
#[tauri::command]
pub async fn pdf_merge(
    inputs: Vec<std::path::PathBuf>,
    output: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<editor_core::PdfOpResult, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ = m;
        editor_core::pdf::merge(&inputs, &output)
    })
    .await
    .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
    .map_err(editor_err)
}

/// PDF 拆分为单页
#[tauri::command]
pub async fn pdf_split(
    path: std::path::PathBuf,
    out_dir: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<Vec<editor_core::PdfOpResult>, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ = m;
        editor_core::pdf::split(&path, &out_dir)
    })
    .await
    .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
    .map_err(editor_err)
}

/// PDF 压缩（结果更大则保留原文件）
#[tauri::command]
pub async fn pdf_compress(
    path: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<editor_core::PdfOpResult, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ = m;
        editor_core::pdf::compress(&path)
    })
    .await
    .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
    .map_err(editor_err)
}

/// PDF 文字水印
#[tauri::command]
pub async fn pdf_watermark(
    path: std::path::PathBuf,
    text: String,
    state: State<'_, HostState>,
) -> Result<editor_core::PdfOpResult, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ = m;
        editor_core::pdf::watermark(&path, &text)
    })
    .await
    .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
    .map_err(editor_err)
}

// ======================== 笔记与知识（M10 N，docs/impl/06） ========================

fn notes_err(e: notes_core::NoteError) -> AppError {
    AppError::module(e.code(), e.to_string(), None)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 变更事件（UI 事件驱动刷新；topic 见 host-core TOPIC_REGISTRY）
fn notes_notify(state: &HostState, action: &str, path: Option<&str>) {
    state
        .bus
        .publish(host_core::events::Event::new(
            "notes.changed",
            "notes",
            serde_json::json!({ "action": action, "path": path }),
        ))
        .ok();
}

/// 全库笔记列表（N1）
#[tauri::command]
pub async fn notes_list(state: State<'_, HostState>) -> Result<Vec<notes_core::model::NoteMeta>, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.list_notes())
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map_err(notes_err)
}

/// 读取笔记内容 + 元数据
#[tauri::command]
pub async fn notes_read(
    rel_path: String,
    state: State<'_, HostState>,
) -> Result<NoteReadDto, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || {
        let (content, meta) = m.read(&rel_path)?;
        Ok(NoteReadDto { content, meta })
    })
    .await
    .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
    .map_err(notes_err)
}

#[derive(serde::Serialize)]
pub struct NoteReadDto {
    pub content: String,
    pub meta: notes_core::model::NoteMeta,
}

/// 创建笔记（空内容默认给一个 H1）
#[tauri::command]
pub async fn notes_create(
    rel_path: String,
    content: Option<String>,
    state: State<'_, HostState>,
) -> Result<notes_core::model::NoteMeta, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    let body = content.unwrap_or_else(|| {
        let stem = std::path::Path::new(&rel_path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("# {stem}\n\n")
    });
    tauri::async_runtime::spawn_blocking(move || m.create(&rel_path, &body))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map(|meta| {
            notes_notify_inner(&state, "create", Some(&meta.path));
            meta
        })
        .map_err(notes_err)
}

fn notes_notify_inner(state: &State<'_, HostState>, action: &str, path: Option<&str>) {
    notes_notify(state.inner(), action, path);
}

/// 保存笔记（重索引 + 事件）
#[tauri::command]
pub async fn notes_write(
    rel_path: String,
    content: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    let rel2 = rel_path.clone();
    tauri::async_runtime::spawn_blocking(move || m.write(&rel_path, &content))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map(|_| {
            notes_notify_inner(&state, "write", Some(&rel2));
        })
        .map_err(notes_err)
}

/// 删除笔记（联动卡片解除关联）
#[tauri::command]
pub async fn notes_delete(rel_path: String, state: State<'_, HostState>) -> Result<(), AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    let rel2 = rel_path.clone();
    tauri::async_runtime::spawn_blocking(move || m.delete(&rel_path))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map(|_| {
            notes_notify_inner(&state, "delete", Some(&rel2));
        })
        .map_err(notes_err)
}

/// 重命名 + 全库引用改写（N2）
#[tauri::command]
pub async fn notes_rename(
    old_path: String,
    new_path: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    let new2 = new_path.clone();
    tauri::async_runtime::spawn_blocking(move || m.rename(&old_path, &new_path))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map(|_| {
            notes_notify_inner(&state, "rename", Some(&new2));
        })
        .map_err(notes_err)
}

/// 本文出链（dst 原文 + 解析结果）
#[tauri::command]
pub async fn notes_links(
    rel_path: String,
    state: State<'_, HostState>,
) -> Result<Vec<LinkDto>, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || {
        let rows = m.links_of(&rel_path)?;
        Ok(rows
            .into_iter()
            .map(|(dst, dst_path)| LinkDto { dst, dst_path })
            .collect::<Vec<_>>())
    })
    .await
    .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
    .map_err(notes_err)
}

#[derive(serde::Serialize)]
pub struct LinkDto {
    pub dst: String,
    pub dst_path: String,
}

/// 反链（含来源标题 + 命中行）
#[tauri::command]
pub async fn notes_backlinks(
    rel_path: String,
    state: State<'_, HostState>,
) -> Result<Vec<notes_core::model::Backlink>, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.backlinks(&rel_path))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map_err(notes_err)
}

/// 增量索引（外部编辑器改动收敛）
#[tauri::command]
pub async fn notes_sync(state: State<'_, HostState>) -> Result<notes_core::model::SyncResult, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.sync())
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map(|r| {
            notes_notify_inner(&state, "sync", None);
            r
        })
        .map_err(notes_err)
}

/// 全量重建索引
#[tauri::command]
pub async fn notes_reindex(state: State<'_, HostState>) -> Result<notes_core::model::SyncResult, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.reindex())
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map(|r| {
            notes_notify_inner(&state, "reindex", None);
            r
        })
        .map_err(notes_err)
}

// ---- N4 复习 ----

/// 全部卡片
#[tauri::command]
pub async fn notes_cards(state: State<'_, HostState>) -> Result<Vec<notes_core::model::Card>, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.cards().list())
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map_err(notes_err)
}

/// 新建卡片（due=now 立即入队）
#[tauri::command]
pub async fn notes_card_create(
    front: String,
    back: String,
    note_path: Option<String>,
    state: State<'_, HostState>,
) -> Result<notes_core::model::Card, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    let now = now_ms();
    tauri::async_runtime::spawn_blocking(move || m.cards().create(&front, &back, note_path, now))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map(|card| {
            notes_notify_inner(&state, "cards", None);
            card
        })
        .map_err(notes_err)
}

/// 删除卡片
#[tauri::command]
pub async fn notes_card_delete(id: String, state: State<'_, HostState>) -> Result<bool, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.cards().delete(&id))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map(|ok| {
            notes_notify_inner(&state, "cards", None);
            ok
        })
        .map_err(notes_err)
}

/// 今天到期队列
#[tauri::command]
pub async fn notes_review_queue(
    state: State<'_, HostState>,
) -> Result<Vec<notes_core::model::Card>, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    let now = now_ms();
    tauri::async_runtime::spawn_blocking(move || m.review_queue(now))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map_err(notes_err)
}

/// SM-2 评分（quality 0-5；UI 四档映射 1/3/4/5）
#[tauri::command]
pub async fn notes_review_grade(
    id: String,
    quality: u32,
    state: State<'_, HostState>,
) -> Result<notes_core::model::Card, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    let now = now_ms();
    tauri::async_runtime::spawn_blocking(move || m.grade_card(&id, quality, now))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map(|card| {
            notes_notify_inner(&state, "cards", None);
            card
        })
        .map_err(notes_err)
}

// ---- N3 画布 ----

/// 读取目录画布（不存在/损坏 → 空画布）
#[tauri::command]
pub async fn notes_canvas_get(
    dir: String,
    state: State<'_, HostState>,
) -> Result<notes_core::model::CanvasDoc, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.canvas_get(&dir))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map_err(notes_err)
}

/// 保存目录画布
#[tauri::command]
pub async fn notes_canvas_save(
    dir: String,
    doc: notes_core::model::CanvasDoc,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    let dir2 = dir.clone();
    tauri::async_runtime::spawn_blocking(move || m.canvas_save(&dir, &doc))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map(|_| {
            notes_notify_inner(&state, "canvas", Some(&dir2));
        })
        .map_err(notes_err)
}

/// 画布可用目录列表
#[tauri::command]
pub async fn notes_canvas_dirs(state: State<'_, HostState>) -> Result<Vec<String>, AppError> {
    let m = state.notes.library().ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.canvas_dirs())
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map_err(notes_err)
}


// ======================== 终端与运维（M11 T，docs/impl/06） ========================

use host_core::ports::DockerPipePort;

fn term_err(e: term_core::TermError) -> AppError {
    AppError::module(e.code(), e.to_string(), None)
}

/// 本地/WSL 会话参数
#[derive(serde::Deserialize)]
pub struct TermSpawnDto {
    /// "local" | "wsl"
    pub kind: String,
    /// 本地完整命令行（None = 默认 PowerShell）
    pub shell: Option<String>,
    pub cwd: Option<std::path::PathBuf>,
    pub wsl_distro: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

/// 本地会话（T1）
#[tauri::command]
pub async fn term_spawn_local(
    shell: Option<String>,
    cwd: Option<std::path::PathBuf>,
    cols: u16,
    rows: u16,
    state: State<'_, HostState>,
) -> Result<term_core::SessionInfo, AppError> {
    state
        .term
        .sessions()
        .spawn_local(shell, cwd, cols, rows)
        .await
        .map_err(term_err)
}

/// WSL 会话（T5）
#[tauri::command]
pub async fn term_spawn_wsl(
    distro: String,
    cols: u16,
    rows: u16,
    state: State<'_, HostState>,
) -> Result<term_core::SessionInfo, AppError> {
    state
        .term
        .sessions()
        .spawn_wsl(&distro, cols, rows)
        .await
        .map_err(term_err)
}

/// WSL 分发列表（T5）
#[tauri::command]
pub async fn term_wsl_list(state: State<'_, HostState>) -> Result<Vec<String>, AppError> {
    let distros = tokio::task::spawn_blocking(term_core::wsl::list_distros)
        .await
        .map_err(|e| AppError::module("TERM_IPC_001", e.to_string(), None))?
        .map_err(term_err)?;
    Ok(distros)
}

/// 终端输入（UTF-8；含控制序列）
#[tauri::command]
pub async fn term_write(
    session_id: String,
    data: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state
        .term
        .sessions()
        .get(&session_id)
        .map_err(term_err)?
        .write(data.into_bytes())
        .await
        .map_err(term_err)
}

/// 调整尺寸
#[tauri::command]
pub async fn term_resize(
    session_id: String,
    cols: u16,
    rows: u16,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state
        .term
        .sessions()
        .get(&session_id)
        .map_err(term_err)?
        .resize(cols, rows)
        .await
        .map_err(term_err)
}

/// 背压 ack（T2：前端回传累计已收字节数）
#[tauri::command]
pub async fn term_ack(
    session_id: String,
    received_total: i64,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state
        .term
        .sessions()
        .ack(&session_id, received_total)
        .map_err(term_err)
}

/// 终止会话
#[tauri::command]
pub async fn term_kill(session_id: String, state: State<'_, HostState>) -> Result<(), AppError> {
    state.term.sessions().kill_session(&session_id).map_err(term_err)
}

/// 会话列表
#[tauri::command]
pub async fn term_sessions(
    state: State<'_, HostState>,
) -> Result<Vec<term_core::SessionInfo>, AppError> {
    Ok(state.term.sessions().list())
}

// ---- T3 SSH/SFTP ----

/// SSH 参数（auth 内联）
#[derive(serde::Deserialize)]
pub struct SshConnectDto {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: term_core::SshAuth,
    pub cols: u16,
    pub rows: u16,
}

/// SSH 终端会话（T3）
#[tauri::command]
pub async fn term_ssh_connect(
    conn: SshConnectDto,
    state: State<'_, HostState>,
) -> Result<term_core::SessionInfo, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    let target = term_core::SshTarget {
        host: conn.host,
        port: conn.port,
        user: conn.user,
        auth: conn.auth,
    };
    ssh.open_shell(target, conn.cols, conn.rows, state.term.sessions())
        .await
        .map_err(term_err)
}

/// 已记录主机指纹列表（TOFU 管理）
#[tauri::command]
pub async fn term_ssh_known_hosts(
    state: State<'_, HostState>,
) -> Result<Vec<SshKnownHostDto>, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    Ok(ssh
        .known_hosts()
        .entries()
        .into_iter()
        .map(|(host, fingerprint)| SshKnownHostDto { host, fingerprint })
        .collect())
}

#[derive(serde::Serialize)]
pub struct SshKnownHostDto {
    pub host: String,
    pub fingerprint: String,
}

/// 删除主机指纹（用户确认主机重建后）
#[tauri::command]
pub async fn term_ssh_forget_host(
    host: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    let (h, port) = parse_host_port(&host);
    ssh.known_hosts().remove(&h, port).map_err(term_err)
}

fn parse_host_port(host: &str) -> (String, u16) {
    // "[h]:port" / "h"（缺省 22）
    if let Some(rest) = host.strip_prefix('[') {
        if let Some((h, p)) = rest.split_once("]:") {
            return (h.to_string(), p.parse().unwrap_or(22));
        }
    }
    (host.to_string(), 22)
}

/// SFTP 目录列表
#[tauri::command]
pub async fn term_sftp_list(
    host: String,
    port: u16,
    user: String,
    auth: term_core::SshAuth,
    path: String,
    state: State<'_, HostState>,
) -> Result<Vec<term_core::SftpEntry>, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    let target = term_core::SshTarget { host, port, user, auth };
    ssh.sftp_list(&target, &path).await.map_err(term_err)
}

/// SFTP 下载
#[tauri::command]
pub async fn term_sftp_download(
    host: String,
    port: u16,
    user: String,
    auth: term_core::SshAuth,
    remote_path: String,
    local_path: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<u64, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    let target = term_core::SshTarget { host, port, user, auth };
    ssh.sftp_download(&target, &remote_path, &local_path)
        .await
        .map_err(term_err)
}

/// SFTP 上传
#[tauri::command]
pub async fn term_sftp_upload(
    host: String,
    port: u16,
    user: String,
    auth: term_core::SshAuth,
    local_path: std::path::PathBuf,
    remote_path: String,
    state: State<'_, HostState>,
) -> Result<u64, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    let target = term_core::SshTarget { host, port, user, auth };
    ssh.sftp_upload(&target, &local_path, &remote_path)
        .await
        .map_err(term_err)
}

// ---- T6 Docker ----

/// 容器列表
#[tauri::command]
pub async fn term_docker_containers(
    state: State<'_, HostState>,
) -> Result<Vec<term_core::docker::DockerContainer>, AppError> {
    let docker = state
        .ports
        .get::<dyn DockerPipePort>()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "Docker 管道未注册", None))?;
    tokio::task::spawn_blocking(move || term_core::docker::containers_list(docker.as_ref()))
        .await
        .map_err(|e| AppError::module("TERM_IPC_001", e.to_string(), None))?
        .map_err(term_err)
}

/// 启动/停止容器
#[tauri::command]
pub async fn term_docker_lifecycle(
    id: String,
    start: bool,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let docker = state
        .ports
        .get::<dyn DockerPipePort>()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "Docker 管道未注册", None))?;
    tokio::task::spawn_blocking(move || term_core::docker::container_lifecycle(docker.as_ref(), &id, start))
        .await
        .map_err(|e| AppError::module("TERM_IPC_001", e.to_string(), None))?
        .map_err(term_err)
}

/// 容器日志（tail 最近 N 行）
#[tauri::command]
pub async fn term_docker_logs(
    id: String,
    tail: u32,
    state: State<'_, HostState>,
) -> Result<String, AppError> {
    let docker = state
        .ports
        .get::<dyn DockerPipePort>()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "Docker 管道未注册", None))?;
    tokio::task::spawn_blocking(move || term_core::docker::container_logs(docker.as_ref(), &id, tail))
        .await
        .map_err(|e| AppError::module("TERM_IPC_001", e.to_string(), None))?
        .map_err(term_err)
}

// ======================== 系统管理（M12 SY，docs/impl/06） ========================

fn sys_err(e: sys_core::SysError) -> AppError {
    AppError::module(e.code(), e.to_string(), None)
}

/// 包管理器源信息（SY1 探测）
#[derive(serde::Serialize)]
pub struct PkgSourceDto {
    pub id: String,
    pub label: String,
    pub available: bool,
}

/// 可用包管理器列表
#[tauri::command]
pub async fn sys_pkg_sources(state: State<'_, HostState>) -> Result<Vec<PkgSourceDto>, AppError> {
    Ok(state
        .sys
        .managers()
        .iter()
        .map(|m| PkgSourceDto {
            id: m.id().to_string(),
            label: m.label().to_string(),
            available: m.available(),
        })
        .collect())
}

/// 已装清单合并视图（SY2：多源去重，winget 优先）
#[tauri::command]
pub async fn sys_pkg_list(state: State<'_, HostState>) -> Result<Vec<sys_core::PkgEntry>, AppError> {
    let m = state.sys.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut sources = Vec::new();
        for mgr in m.managers() {
            if mgr.available() {
                match mgr.list() {
                    Ok(pkgs) => sources.push((mgr.id(), pkgs)),
                    Err(e) => tracing::warn!(source = mgr.id(), error = %e, "包清单获取失败"),
                }
            }
        }
        Ok(sys_core::pkg::merge_installed(sources))
    })
    .await
    .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))?
    .map_err(sys_err)
}

/// 变更命令行预览（UI 确认展示——docs/impl/06 SY1：列出将执行的确切命令行）
#[tauri::command]
pub async fn sys_pkg_cmd_preview(
    source: String,
    action: String,
    package_id: String,
    state: State<'_, HostState>,
) -> Result<String, AppError> {
    let mgr = state
        .sys
        .manager(&source)
        .ok_or_else(|| AppError::module("SYS_IPC_002", format!("未知包管理器: {source}"), None))?;
    mgr.cmd_preview(&action, &package_id).map_err(sys_err)
}

/// 执行包变更（install/uninstall/upgrade_all；输出逐行发 sys.pkg_line 事件）
#[tauri::command]
pub async fn sys_pkg_action(
    source: String,
    action: String,
    package_id: String,
    state: State<'_, HostState>,
) -> Result<Vec<String>, AppError> {
    // 预检（在 move 之前校验管理器存在）
    state
        .sys
        .manager(&source)
        .ok_or_else(|| AppError::module("SYS_IPC_002", format!("未知包管理器: {source}"), None))?;
    let m = state.sys.clone();
    let bus = state.bus.clone();
    let src_for_emit = source.clone();
    let action_for_emit = action.clone();
    let lines = tauri::async_runtime::spawn_blocking(move || {
        let mgr = m
            .manager(&source)
            .ok_or_else(|| AppError::module("SYS_IPC_002", format!("未知包管理器: {source}"), None))?;
        let mut emit = |line: String| {
            bus.publish(host_core::events::Event::new(
                "sys.pkg_line",
                "sys",
                serde_json::json!({ "source": src_for_emit, "action": action_for_emit, "line": line }),
            ))
            .ok();
        };
        mgr.run_action(&action, &package_id, &mut emit)
            .map_err(|e| AppError::module(e.code(), e.to_string(), None))
    })
    .await
    .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))??;
    Ok(lines)
}

/// 清理目标清单（SY3）
#[tauri::command]
pub async fn sys_clean_targets(
    state: State<'_, HostState>,
) -> Result<Vec<sys_core::clean::CleanTarget>, AppError> {
    Ok(state.sys.targets().to_vec())
}

/// 清理扫描（SY3：汇总可回收量）
#[tauri::command]
pub async fn sys_clean_scan(
    state: State<'_, HostState>,
) -> Result<Vec<sys_core::clean::CleanScanItem>, AppError> {
    let m = state.sys.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let mut out = Vec::new();
        for t in m.targets() {
            match sys_core::clean::scan_target(t, now) {
                Ok(item) => out.push(item),
                Err(e) => tracing::warn!(target = t.id, error = %e, "清理扫描失败"),
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))?
}

/// 执行清理（selected_ids 勾选目标；recycle=true 走回收站可恢复）
#[tauri::command]
pub async fn sys_clean_execute(
    selected_ids: Vec<String>,
    recycle: bool,
    state: State<'_, HostState>,
) -> Result<u64, AppError> {
    let m = state.sys.clone();
    let recycle_port = state.sys.recycle();
    tauri::async_runtime::spawn_blocking(move || {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let mut total_files = 0u64;
        for id in &selected_ids {
            let Some(t) = m.target(id) else { continue };
            let recycle_fn = |paths: &[std::path::PathBuf]| -> sys_core::Result<u32> {
                let Some(rp) = recycle_port.as_ref() else {
                    return Err(sys_core::SysError::CleanTarget("回收站端口未注册".into()));
                };
                rp.delete(paths)
                    .map_err(|e| sys_core::SysError::CleanTarget(e.to_string()))
            };
            match sys_core::clean::execute_target(t, now, recycle, &recycle_fn) {
                Ok((files, _)) => total_files += files,
                Err(e) => tracing::warn!(target = id, error = %e, "清理执行失败"),
            }
        }
        Ok(total_files)
    })
    .await
    .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))?
}

/// 监控历史（SY4：环形缓冲快照）
#[tauri::command]
pub async fn sys_metrics_history(
    state: State<'_, HostState>,
) -> Result<Vec<sys_core::MetricsPoint>, AppError> {
    Ok(state.sys.metrics().history())
}

// ======================== 自动化与拓展（M14 A1–A3，docs/impl/07） ========================

fn auto_err(e: automation_core::AutomationError) -> AppError {
    AppError::module(e.code(), e.to_string(), None)
}

/// 规则清单
#[tauri::command]
pub async fn automation_rules_list(
    state: State<'_, HostState>,
) -> Result<Vec<automation_core::rule::Rule>, AppError> {
    Ok(state.automation.rules())
}

/// 保存规则（新增/覆盖；校验 + rules.json 持久化）
#[tauri::command]
pub async fn automation_save_rule(
    rule: automation_core::rule::Rule,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state.automation.save_rule(rule).map_err(auto_err)
}

/// 删除规则
#[tauri::command]
pub async fn automation_delete_rule(
    id: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    state.automation.delete_rule(&id).map_err(auto_err)
}

/// 启停规则
#[tauri::command]
pub async fn automation_toggle_rule(
    id: String,
    enabled: bool,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    state.automation.toggle_rule(&id, enabled).map_err(auto_err)
}

/// 死信队列（UI 死信面板）
#[tauri::command]
pub async fn automation_dead_letters(
    state: State<'_, HostState>,
) -> Result<Vec<automation_core::engine::DeadLetter>, AppError> {
    Ok(state
        .automation
        .engine()
        .map(|e| e.dead_letters())
        .unwrap_or_default())
}

/// 重放死信（动作成功移除；仍失败以新 id 重新入队）
#[tauri::command]
pub async fn automation_replay(
    dead_id: String,
    rule_id: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let engine = state
        .automation
        .engine()
        .ok_or_else(|| AppError::module("AUTO_EXEC_001", "规则引擎未初始化", None))?;
    // 规则已删除的死信无法重放（动作语义随规则上下文）
    let rule = state
        .automation
        .rules()
        .into_iter()
        .find(|r| r.id == rule_id)
        .ok_or_else(|| AppError::module("AUTO_RULE_001", format!("规则 {rule_id} 已删除，无法重放"), None))?;
    engine.replay(&dead_id, &rule).map_err(auto_err)
}

/// 插件清单（A6：扫描插件库）
#[tauri::command]
pub async fn automation_plugins_list(
    state: State<'_, HostState>,
) -> Result<Vec<automation_core::PluginInfo>, AppError> {
    Ok(state
        .automation
        .plugin_store()
        .map(|s| s.list())
        .unwrap_or_default())
}

/// 从本地目录安装插件（读 {src}/manifest.json + entry → 校验 → 入库；A6 v1 市场客户端 = 本地导入）
#[tauri::command]
pub async fn automation_plugin_install(
    src_dir: String,
    state: State<'_, HostState>,
) -> Result<automation_core::PluginManifest, AppError> {
    let store = state
        .automation
        .plugin_store()
        .ok_or_else(|| AppError::module("AUTO_EXEC_001", "插件库未初始化", None))?;
    tauri::async_runtime::spawn_blocking(move || {
        store.install_from_dir(std::path::Path::new(&src_dir))
    })
    .await
    .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))?
    .map_err(auto_err)
}

/// 删除插件
#[tauri::command]
pub async fn automation_plugin_remove(id: String, state: State<'_, HostState>) -> Result<bool, AppError> {
    let store = state
        .automation
        .plugin_store()
        .ok_or_else(|| AppError::module("AUTO_EXEC_001", "插件库未初始化", None))?;
    tauri::async_runtime::spawn_blocking(move || store.remove(&id))
        .await
        .map_err(|e| AppError::module("SYS_IPC_001", e.to_string(), None))?
        .map_err(auto_err)
}

// ======================== 跨设备同步（M15 SYNC，docs/impl/07） ========================

fn sync_err(e: sync_core::SyncError) -> AppError {
    AppError::module(e.code(), e.to_string(), None)
}

/// 配对设备列表（信任根复用 KVM 配对；UI 选择同步目标）
#[tauri::command]
pub async fn sync_peers(state: State<'_, HostState>) -> Result<Vec<kvm_core::PairedPeer>, AppError> {
    Ok(state.sync.peers())
}

/// op_log 状态（计数/监听端口）
#[tauri::command]
pub async fn sync_status(state: State<'_, HostState>) -> Result<serde_json::Value, AppError> {
    Ok(state.sync.status())
}

/// 立即与指定设备同步（addr 如 "192.168.1.10:49820"；端口默认 DEFAULT_SYNC_PORT）
#[tauri::command]
pub async fn sync_now(
    device_id: String,
    addr: String,
    state: State<'_, HostState>,
) -> Result<sync_core::SyncSummary, AppError> {
    state.sync.sync_with(&device_id, &addr).await.map_err(sync_err)
}

// ======================== WinOps Tweak 引擎（M16 W0–W4，docs/impl/08） ========================

/// 组装 WinOps 端口聚合（registry 必备；tasks/services/maintenance/appx 注册缺失容忍为 None）
fn winops_ports(state: &HostState) -> Result<
    (
        std::sync::Arc<dyn host_core::ports::RegistryOps>,
        Option<std::sync::Arc<dyn host_core::ports::TaskTogglePort>>,
        Option<std::sync::Arc<dyn host_core::ports::ServiceCtlPort>>,
        Option<std::sync::Arc<dyn host_core::ports::MaintenancePort>>,
        Option<std::sync::Arc<dyn host_core::ports::AppxPort>>,
    ),
    AppError,
> {
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
pub async fn winops_catalog(state: State<'_, HostState>) -> Result<Vec<sys_core::winops::Tweak>, AppError> {
    let external = state.app_data_dir.join("winops").join("catalog");
    tauri::async_runtime::spawn_blocking(move || {
        sys_core::winops::load_catalog(Some(&external))
    })
    .await
    .map_err(|e| AppError::module("SYS_WINOPS_001", e.to_string(), None))?
    .map_err(sys_err)
}

/// 扫描应用状态（三态：已应用/未应用/需管理员——requires_admin 且非提权进程）
#[tauri::command]
pub async fn winops_scan(state: State<'_, HostState>) -> Result<Vec<(sys_core::winops::Tweak, sys_core::winops::ScanState)>, AppError> {
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
        || t.actions
            .iter()
            .any(|a| match a {
                TweakAction::Registry { key, .. } => key.starts_with("HKLM"),
                TweakAction::AppxRemove { all_users: true, .. } => true,
                TweakAction::Exec { .. } | TweakAction::RestorePoint { .. } | TweakAction::EmptyWorkingSet {} => true,
                TweakAction::DefenderRealtime { .. } => true,
                _ => false,
            })
}

/// BAVR 应用（备份 → 写入 → 校验 → 失败补偿；成功后备份落 {appData}/winops/backup.json）
/// 非提权进程应用需管理员条目：拉起提权 Helper（UAC）→ helper-backed 数据面执行
#[tauri::command]
pub async fn winops_apply(id: String, state: State<'_, HostState>) -> Result<sys_core::winops::ApplyReport, AppError> {
    let external = state.app_data_dir.join("winops").join("catalog");
    let app_dir = state.app_data_dir.clone();
    let (reg, tasks, services, maintenance, appx) = winops_ports(&state)?;
    let helper_spawn = state.ports.get::<dyn host_core::ports::HelperSpawnPort>();
    let is_admin = state
        .ports
        .get::<dyn host_core::ports::SysProxyPort>()
        .map(|p| p.is_admin())
        .unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
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
        sys_core::winops::BackupStore::open(&app_dir).save(&report).map_err(sys_err)?;
        // W7 审计：apply 落 JSONL（写失败不阻断）
        sys_core::winops::AuditStore::open(&app_dir).record("ui", &id, "apply", Some(report.verified), "");
        Ok(report)
    })
    .await
    .map_err(|e| AppError::module("SYS_WINOPS_001", e.to_string(), None))?
    .map_err(sys_err)
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
            return Err(sys_core::SysError::Catalog(format!("Tweak {id} 无备份可回滚")));
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
