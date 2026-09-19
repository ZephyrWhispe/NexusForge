use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

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

/// 解密读取条目内容（secret 条目经信封解密还原，D-04）
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
                return Err(AppError::module(
                    "CLIPBOARD_PASTE_004",
                    "加密条目状态异常",
                    None,
                ))
            }
        };
        clipboard.write_back(&content)
    })
    .await
    .map_err(|e| AppError::module("CLIPBOARD_PASTE_003", e.to_string(), None))?
}

/// 图片条目字节（Base64 DIB），前端 canvas 解码预览用
#[tauri::command]
pub async fn clipboard_get_image(
    id: String,
    state: State<'_, HostState>,
) -> Result<String, AppError> {
    use base64::Engine;
    use clipboard_core::store::Payload;
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || match clipboard.get_payload(&id)? {
        Some(Payload::Image { bytes, .. }) => {
            Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        _ => Err(AppError::module(
            "CLIPBOARD_QUERY_004",
            "条目不是图片",
            None,
        )),
    })
    .await
    .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))?
}

#[tauri::command]
pub async fn clipboard_pin(
    id: String,
    pinned: bool,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
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
pub async fn clipboard_clear(
    keep_pinned: bool,
    state: State<'_, HostState>,
) -> Result<u32, AppError> {
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
pub async fn clipboard_group_counts(
    state: State<'_, HostState>,
) -> Result<serde_json::Value, AppError> {
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || clipboard.group_counts())
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))?
}
