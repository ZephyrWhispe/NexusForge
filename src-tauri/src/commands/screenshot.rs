use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

// ---------------- 截图命令（docs/impl/03 P8）----------------
// GDI 捕获 / PNG 编解码为阻塞调用，统一 spawn_blocking

/// 启动截图任务：抓帧并返回覆盖层定位信息。
/// `hwnd` 为可选参（D-29 B4 T-B4-4）：缺省 = 全屏轨，给出句柄 = 只截该窗。
/// 加可选参而非新命令——两条轨的产物是同一个任务状态机，分开只会多一条要维护的 ACL 面。
#[tauri::command]
pub async fn screenshot_start(
    mode: String,
    hwnd: Option<i64>,
    state: State<'_, HostState>,
) -> Result<screenshot_core::types::TaskStartDto, AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.start_capture(&mode, hwnd))
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))?
}

/// 可截取窗口表（"截取窗口"下拉）。EnumWindows 是阻塞调用，同样进 spawn_blocking。
#[tauri::command]
pub async fn screenshot_windows(
    state: State<'_, HostState>,
) -> Result<Vec<screenshot_core::types::WindowTargetDto>, AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.window_targets())
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))
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
    let mut dto =
        tauri::async_runtime::spawn_blocking(move || screenshot.finish(&task_id, &request))
            .await
            .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))??;
    drain_upload(&state.screenshot, &mut dto).await;
    Ok(dto)
}

/// 动作链里 `upload` 那一跳的落点（D-29 B4 T-B4-9）。
///
/// 同步核发不出网络请求（reqwest 是 async，且本模块不引 blocking feature），所以
/// `run_actions` 只把"要传什么"攒成 `FinishDto::upload_ticket`（`#[serde(skip)]`，
/// 这个字段因此**永远不会**跨 IPC），由这里在同一任务上 await 唯一一次上传。
///
/// 失败不把本次完成判红：文件和历史行都已经落地，重放一次 finish 会多出孤儿文件与
/// 重复记录——那比"没传上去"糟得多。真因经 `screenshot.upload_failed` 事件回面板。
async fn drain_upload(
    screenshot: &screenshot_core::ScreenshotModule,
    dto: &mut screenshot_core::types::FinishDto,
) {
    let Some(ticket) = dto.upload_ticket.take() else {
        return;
    };
    // 链条上没有传凭据的通道（`FinishRequest` 里刻意没有任何凭据字段）：需要鉴权的
    // 端点只能走面板那一行的上传钮，这条链只服务匿名/内网自建档
    match screenshot.fulfill_upload(&ticket, None).await {
        Ok(link) => dto.link = link,
        Err(e) => {
            tracing::warn!(error = %e, source = %ticket.source, "链式上传失败");
            screenshot.publish_upload_failed(&ticket.source, &e);
        }
    }
}

// ---------------- 滚动截图（D-29 B4 T-B4-5，手动步进）----------------
// 步进由用户点击驱动：全通路不含任何输入注入（不模拟滚轮/键盘），"滚不动"永远不是这里的故障

/// 开一次滚动会话：同矩形抓首帧，返回会话 id
#[tauri::command]
pub async fn screenshot_scroll_begin(
    rect: screenshot_core::types::ConfirmRect,
    state: State<'_, HostState>,
) -> Result<String, AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.scroll_begin(rect))
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))?
}

/// 用户滚过一段后追加：同矩形重取一帧并对上（对不上则原样另起一段，不丢帧）
#[tauri::command]
pub async fn screenshot_scroll_append(
    id: String,
    state: State<'_, HostState>,
) -> Result<screenshot_core::types::ScrollStepDto, AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.scroll_append(&id))
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))?
}

/// 收束会话：逐段走既有动作通路 + 逐段入历史（分段是降级出口，各存一图）
#[tauri::command]
pub async fn screenshot_scroll_finish(
    id: String,
    actions: Vec<String>,
    state: State<'_, HostState>,
) -> Result<screenshot_core::types::FinishDto, AppError> {
    let screenshot = state.screenshot.clone();
    let mut dto =
        tauri::async_runtime::spawn_blocking(move || screenshot.scroll_finish(&id, &actions))
            .await
            .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))??;
    drain_upload(&state.screenshot, &mut dto).await;
    Ok(dto)
}

/// 放弃会话：带子只住内存，用户点"放弃"就得立刻还内存，不等进程重启
#[tauri::command]
pub fn screenshot_scroll_discard(id: String, state: State<'_, HostState>) {
    state.screenshot.scroll_discard(&id);
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
        let store =
            store.ok_or_else(|| AppError::module("SCREENSHOT_STATE_001", "模块未就绪", None))?;
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

/// 单条历史截图的 PNG 字节（D-29 B0-2 主面板真缩略图；历史表只存路径）
#[tauri::command]
pub async fn screenshot_history_get(
    id: String,
    state: State<'_, HostState>,
) -> Result<screenshot_core::types::ShotDataDto, AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.history_get(&id))
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))?
}

/// 历史截图再复制进系统剪贴板（png→CF_DIB 由 win-integration 转换）
#[tauri::command]
pub async fn screenshot_history_copy(
    id: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let screenshot = state.screenshot.clone();
    tauri::async_runtime::spawn_blocking(move || screenshot.history_copy(&id))
        .await
        .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))?
}

/// 历史条目的美化出口（D-29 B4 T-B4-6）：读原字节 → 美化 → 走与 finish 同一条动作通路。
///
/// **`actions` 为空数组 = 只预览不落盘**：返回 `preview_b64` 供 `<img>`，零磁盘写、
/// 零剪贴板写、零历史新增（面板那颗"预览"钮传 `[]`，语义写在字面上）。
#[tauri::command]
pub async fn screenshot_beautify_apply(
    id: String,
    spec: screenshot_core::beautify::BeautifySpec,
    actions: Vec<String>,
    state: State<'_, HostState>,
) -> Result<screenshot_core::types::FinishDto, AppError> {
    let screenshot = state.screenshot.clone();
    let mut dto = tauri::async_runtime::spawn_blocking(move || {
        screenshot.beautify_apply(&id, &spec, &actions)
    })
    .await
    .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))??;
    drain_upload(&state.screenshot, &mut dto).await;
    Ok(dto)
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

// ---------------- 上传轨（D-29 B4 T-B4-9，§9.1-⑩）----------------
// 数据出机面：只有主窗口拿得到这两条命令（overlay/pin/quickpanel 等一律不授权，
// 见 src-tauri/tests/security_config.rs 的 auxWindows_neverGrantScreenshotUpload）。

/// 已注册的上传目标表（供设置面板下拉与"未启用"徽标）。纯读，不发任何请求。
#[tauri::command]
pub fn screenshot_upload_targets(
    state: State<'_, HostState>,
) -> Vec<screenshot_core::upload::UploadTargetInfo> {
    state.screenshot.upload_targets()
}

/// 上传历史里的某一行，换回一条直链。
///
/// `header_value` 是**这一次调用**的凭据值：它不进配置、不进日志、不进事件，
/// 函数返回即无处可寻。端点没配请求头名时传 `None`（前端此时根本不给输入框）。
#[tauri::command]
pub async fn screenshot_upload(
    id: String,
    header_value: Option<String>,
    state: State<'_, HostState>,
) -> Result<String, AppError> {
    let screenshot = state.screenshot.clone();
    let (filename, bytes) =
        tauri::async_runtime::spawn_blocking(move || screenshot.upload_payload(&id))
            .await
            .map_err(|e| AppError::module("SCREENSHOT_STATE_003", e.to_string(), None))??;
    state
        .screenshot
        .upload_now(&filename, &bytes, header_value)
        .await
}
