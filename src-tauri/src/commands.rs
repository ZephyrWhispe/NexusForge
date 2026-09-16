//! 宿主级 IPC 命令（docs/impl/01 S7；模块命令在各自模块实现）
//!
//! 命名规范：`{module}_{action}` snake_case；
//! 错误统一 `Result<T, AppError>`（S2 序列化契约）。

use host_core::error::AppError;
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
