use host_core::error::AppError;
use serde::Serialize;
use tauri::State;

use crate::state::HostState;

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
pub async fn file_drives(
    state: State<'_, HostState>,
) -> Result<Vec<file_core::DriveInfo>, AppError> {
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
}

/// 入队文件操作（F2/F3；Ask 冲突预扫描）
#[tauri::command]
pub async fn file_enqueue(
    spec: file_core::OpSpec,
    state: State<'_, HostState>,
) -> Result<FileEnqueueDto, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        svc.enqueue(spec)
            .map(|(op_id, conflicts)| FileEnqueueDto { op_id, conflicts })
    })
    .await
    .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
    .map_err(file_err)
}

/// 活跃/近期操作（F2）
#[tauri::command]
pub async fn file_ops_active(
    state: State<'_, HostState>,
) -> Result<Vec<file_core::OpProgress>, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.ops_active()))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
}

/// 崩溃恢复扫描：未完成操作（F2，docs/impl/01 S6.5）
#[tauri::command]
pub async fn file_ops_pending(
    state: State<'_, HostState>,
) -> Result<Vec<file_core::PendingOp>, AppError> {
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
pub async fn file_op_resume(
    op_id: String,
    state: State<'_, HostState>,
) -> Result<String, AppError> {
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
pub async fn file_drivers(
    state: State<'_, HostState>,
) -> Result<Vec<file_core::DriverInfo>, AppError> {
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

// ---------------- B6 远程连接档案（09 §6.2 T-B6-1）----------------
// 档案 = 站点清单（零凭据字段，AuthKind 只有指针）；口令面自 T-B6-3 起
// 走 file_remote_connect 的逐次入参，永不入本命令面。

/// 前端 DTO 与 file-core 档案同形（结构体已带 snake_case serde 契约）
pub type RemoteProfileDto = file_core::RemoteProfile;

/// 档案列表（按 last_used_ms 降序）
#[tauri::command]
pub async fn file_remote_profiles(
    state: State<'_, HostState>,
) -> Result<Vec<RemoteProfileDto>, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.profiles().list()))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
}

/// 存/改档案（写侧校验：保留 id、`remote:` 前缀、host/port/base_path 形状）
#[tauri::command]
pub async fn file_remote_profile_save(
    profile: RemoteProfileDto,
    state: State<'_, HostState>,
) -> Result<RemoteProfileDto, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.profiles().save(profile))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 幂等删除：档案不存在回 false（非 Err）
#[tauri::command]
pub async fn file_remote_profile_delete(
    id: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.profiles().delete(&id))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

// ---------------- B6 远端连接（09 §6.2 T-B6-3）----------------
// 口令走本命令面的 `secret` 逐次入参（AuthSecret 只进不出：类型层面没有
// Serialize，返回值不可能带出凭据；连接态进程内不落盘，重启即"未连接"）。
// CSP 复核登记：tauri.conf.json connect-src 只管 WebView 发起的请求，
// Rust 侧 reqwest 不经 CSP——本行只记事实，不放松 CSP。

/// 凭据 DTO：手写 Deserialize（serde deny_unknown_fields），永不 Serialize
pub type AuthSecretDto = file_core::AuthSecret;
/// 已连接驱动的对外描述（键集恒等 RemoteDriverInfo 字段集，无凭据位）
pub type RemoteDriverDto = file_core::RemoteDriverInfo;

/// 连接档案（本行只认 webdav；其余协议 FILE_REMOTE_004 点名后续行，禁假就绪）
#[tauri::command]
pub async fn file_remote_connect(
    profile_id: String,
    secret: Option<AuthSecretDto>,
    state: State<'_, HostState>,
) -> Result<RemoteDriverDto, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.connect(&profile_id, secret))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 远端列目录（独立子视图：不走 file_list/本地浏览链，见 09 §6.2 T-B6-3 落地补记）
#[tauri::command]
pub async fn file_remote_browse(
    driver_id: String,
    path: String,
    state: State<'_, HostState>,
) -> Result<Vec<file_core::RemoteEntry>, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.remote_list(&driver_id, &path))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 已连接远端列表（进程内事实源；未连接 = 空表，与档案列表 file_remote_profiles 分面）
#[tauri::command]
pub async fn file_remote_drivers(
    state: State<'_, HostState>,
) -> Result<Vec<RemoteDriverDto>, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.remote_drivers()))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
}

// ---------------- B6 远程预设（09 §6.2 T-B6-6）----------------
// 预设 = 数据文件非代码（`file-core/presets/default.json` + 用户目录），
// 形状校验 fail-closed 在 FileService::open 已完成（坏一份 ⇒ 服务开不起来），
// 本命令面只读列举；auth_kind 只有匿名/逐次输入两档——预设永不含凭据。

/// 前端 DTO 与 file-core 预设同形（结构体已带 snake_case + deny_unknown_fields 契约）
pub type RemotePresetDto = file_core::RemotePreset;

/// 预设列表（内置 + 用户目录合并快照，按 id 排序前的装载序）
#[tauri::command]
pub async fn file_remote_presets(
    state: State<'_, HostState>,
) -> Result<Vec<RemotePresetDto>, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.presets()))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
}

// ---------------- B6 SFTP TOFU（09 §6.2 T-B6-5）----------------
// 信任决定命令比 connect 更敏感（写的是主机键表）——capability 面 main-only
// 负例与 auxWindows 扫描同谱。文案红线：接受的是**这一枚指纹**，不是"这台主机"。

/// TOFU 首见的唯一出路：用户逐字核对后确认**这一枚**主机键描述符
/// （"算法名 SHA256:base64"整串，取自 FILE_REMOTE_001 错误消息）
#[tauri::command]
pub async fn file_remote_fingerprint_ack(
    profile_id: String,
    fingerprint: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || svc.fingerprint_ack(&profile_id, &fingerprint))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
        .map_err(file_err)
}

/// 主动断开（幂等：未连接回 false 而非 Err——退役不是错误）
#[tauri::command]
pub async fn file_remote_disconnect(
    driver_id: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let svc = file_service(&state)?;
    tauri::async_runtime::spawn_blocking(move || Ok(svc.detach(&driver_id)))
        .await
        .map_err(|e| AppError::module("FILE_IPC_002", e.to_string(), None))?
}
