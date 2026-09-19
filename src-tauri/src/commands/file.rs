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
