use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

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
pub async fn editor_content(id: String, state: State<'_, HostState>) -> Result<String, AppError> {
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

/// 切换回写编码/统一行尾（T-B7-18）：转码前算不可映射字符数如实返回；
/// 只改内存档位不落中间盘，落盘发生在下一次 editor_save（参数名 session_id 逐任务书签名）
#[tauri::command]
pub async fn editor_set_encoding(
    session_id: String,
    encoding: editor_core::EncodingKindDto,
    eol: editor_core::EolChoice,
    state: State<'_, HostState>,
) -> Result<editor_core::EncodingPreview, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || {
        m.sessions().set_encoding(&session_id, encoding, eol)
    })
    .await
    .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
    .map_err(editor_err)
}

/// 恢复 autosave 草稿（T-B7-18 回读口）：仅当草稿比盘上文件新；恢复后置脏待用户保存
#[tauri::command]
pub async fn editor_recover_draft(
    id: String,
    state: State<'_, HostState>,
) -> Result<editor_core::SessionInfo, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || m.sessions().recover_draft(&id))
        .await
        .map_err(|e| AppError::module("EDITOR_IPC_001", e.to_string(), None))?
        .map_err(editor_err)
}

/// PDF 基本信息
#[tauri::command]
pub async fn pdf_info(
    path: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<editor_core::PdfInfo, AppError> {
    let _m = state.editor.clone();
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

/// PDF 拆分为单页（T-B7-19：pages=None 全拆，Some(list) 只拆列出页，越界 pdf 层点名拒）
#[tauri::command]
pub async fn pdf_split(
    path: std::path::PathBuf,
    out_dir: std::path::PathBuf,
    pages: Option<Vec<u32>>,
    state: State<'_, HostState>,
) -> Result<Vec<editor_core::PdfOpResult>, AppError> {
    let m = state.editor.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ = m;
        editor_core::pdf::split(&path, &out_dir, pages.as_deref())
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
