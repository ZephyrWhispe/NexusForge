use host_core::error::AppError;
use serde::Serialize;
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

/// 捕获暂停态（§8-④）：读运行期原子值与跳过计数，不读盘——盘上是它的持久化投影
#[derive(Serialize)]
pub struct CaptureStateDto {
    pub paused: bool,
    pub skipped: u32,
}

#[tauri::command]
pub async fn clipboard_capture_get(
    state: State<'_, HostState>,
) -> Result<CaptureStateDto, AppError> {
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || CaptureStateDto {
        paused: clipboard.capture_paused(),
        skipped: clipboard.capture_skipped(),
    })
    .await
    .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))
}

#[tauri::command]
pub async fn clipboard_capture_set(
    paused: bool,
    state: State<'_, HostState>,
) -> Result<CaptureStateDto, AppError> {
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || {
        clipboard.set_capture_paused(paused)?;
        Ok::<CaptureStateDto, AppError>(CaptureStateDto {
            paused: clipboard.capture_paused(),
            skipped: clipboard.capture_skipped(),
        })
    })
    .await
    .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))?
}

// ---------------- 粘贴堆栈（docs/impl/09 §8.2 T-B3-3）----------------
// 投递两段式：write_back 写系统剪贴板 → InputInjectPort 注入 Ctrl+V。
// 敏感条目一律不出栈（内容根本没写），写失败同样不出栈；注入失败已消费
// （明文确实进了剪贴板），按 09 判据出栈并计入 failed，剩余留栈。

/// 粘贴组合键：Ctrl↓ V↓ V↑ C↑（vk/scan 为 Windows 虚拟键码与扫描码集 1）
pub const PASTE_KEY_SEQ: &[host_core::ports::RawInput] = &[
    host_core::ports::RawInput::KeyDown {
        vk: 0x11,
        scan: 0x1D,
    },
    host_core::ports::RawInput::KeyDown {
        vk: 0x56,
        scan: 0x2F,
    },
    host_core::ports::RawInput::KeyUp {
        vk: 0x56,
        scan: 0x2F,
    },
    host_core::ports::RawInput::KeyUp {
        vk: 0x11,
        scan: 0x1D,
    },
];

#[derive(Serialize)]
pub struct StackPasteDto {
    pub id: String,
    pub delivered: bool,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct StackPasteReportDto {
    pub delivered: u32,
    pub failed: u32,
    pub remaining: u32,
}

/// 投递编排在 clipboard-core（FakeClipboardPort + FakeInputInjectPort 可端到端验证），
/// 命令层只做 wire 形状映射与事件广播。
impl From<clipboard_core::module::StackDelivery> for StackPasteDto {
    fn from(d: clipboard_core::module::StackDelivery) -> Self {
        StackPasteDto {
            id: d.id,
            delivered: d.delivered,
            error: d.error,
        }
    }
}

fn publish_stack_depth(
    bus: &host_core::events::EventBus,
    clipboard: &clipboard_core::module::ClipboardModule,
) {
    let depth = clipboard.stack_list().map(|v| v.len()).unwrap_or(0);
    bus.publish(host_core::events::Event::new(
        "clipboard.stack_changed",
        "clipboard",
        serde_json::json!({ "depth": depth }),
    ))
    .ok();
}

#[tauri::command]
pub async fn clipboard_stack_push(
    id: String,
    state: State<'_, HostState>,
) -> Result<u32, AppError> {
    let clipboard = state.clipboard.clone();
    let depth = tauri::async_runtime::spawn_blocking(move || clipboard.stack_push(&id))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_STACK_003", e.to_string(), None))??;
    publish_stack_depth(&state.bus, &state.clipboard);
    Ok(depth)
}

#[tauri::command]
pub async fn clipboard_stack_list(
    state: State<'_, HostState>,
) -> Result<Vec<clipboard_core::types::ClipEntry>, AppError> {
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || clipboard.stack_entries())
        .await
        .map_err(|e| AppError::module("CLIPBOARD_STACK_003", e.to_string(), None))?
}

#[tauri::command]
pub async fn clipboard_stack_move(
    id: String,
    to: u32,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || clipboard.stack_move(&id, to as usize))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_STACK_003", e.to_string(), None))??;
    publish_stack_depth(&state.bus, &state.clipboard);
    Ok(())
}

#[tauri::command]
pub async fn clipboard_stack_remove(
    id: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let clipboard = state.clipboard.clone();
    let removed = tauri::async_runtime::spawn_blocking(move || clipboard.stack_remove(&id))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_STACK_003", e.to_string(), None))??;
    publish_stack_depth(&state.bus, &state.clipboard);
    Ok(removed)
}

#[tauri::command]
pub async fn clipboard_stack_clear(state: State<'_, HostState>) -> Result<u32, AppError> {
    let clipboard = state.clipboard.clone();
    let n = tauri::async_runtime::spawn_blocking(move || clipboard.stack_clear())
        .await
        .map_err(|e| AppError::module("CLIPBOARD_STACK_003", e.to_string(), None))??;
    publish_stack_depth(&state.bus, &state.clipboard);
    Ok(n)
}

#[tauri::command]
pub async fn clipboard_stack_paste_next(
    state: State<'_, HostState>,
) -> Result<Option<StackPasteDto>, AppError> {
    let clipboard = state.clipboard.clone();
    let dto = tauri::async_runtime::spawn_blocking(move || {
        clipboard
            .stack_deliver_head(PASTE_KEY_SEQ)
            .map(|o| o.map(StackPasteDto::from))
    })
    .await
    .map_err(|e| AppError::module("CLIPBOARD_STACK_003", e.to_string(), None))??;
    publish_stack_depth(&state.bus, &state.clipboard);
    Ok(dto)
}

/// 全部粘贴：逐条投递，条间 sleep interval_ms（上限 5s 防误填挂机）；
/// 任一失败立即停，剩余留栈，remaining 如实。
#[tauri::command]
pub async fn clipboard_stack_paste_all(
    interval_ms: u32,
    state: State<'_, HostState>,
) -> Result<StackPasteReportDto, AppError> {
    let clipboard = state.clipboard.clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        let r = clipboard.stack_paste_all(
            std::time::Duration::from_millis((interval_ms as u64).min(5000)),
            PASTE_KEY_SEQ,
        )?;
        Ok::<StackPasteReportDto, AppError>(StackPasteReportDto {
            delivered: r.delivered,
            failed: r.failed,
            remaining: r.remaining,
        })
    })
    .await
    .map_err(|e| AppError::module("CLIPBOARD_STACK_003", e.to_string(), None))??;
    publish_stack_depth(&state.bus, &state.clipboard);
    Ok(report)
}

// ---------------- 分组数据面 + 智能建议 + 统计（docs/impl/09 §8.2 T-B3-4）----------------
// 建议制：分类器只写 suggested_*，落库分组要么 auto_group 开、要么用户在建议卡上点采纳。

#[tauri::command]
pub async fn clipboard_entry_set_group(
    id: String,
    group: Option<String>,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || clipboard.set_entry_group(&id, group.as_deref()))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))??;
    publish_group_counts(&state.bus, &state.clipboard);
    Ok(())
}

#[tauri::command]
pub async fn clipboard_group_rename(
    from: String,
    to: String,
    state: State<'_, HostState>,
) -> Result<u32, AppError> {
    let clipboard = state.clipboard.clone();
    let n = tauri::async_runtime::spawn_blocking(move || clipboard.rename_group(&from, &to))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))??;
    publish_group_counts(&state.bus, &state.clipboard);
    Ok(n)
}

#[tauri::command]
pub async fn clipboard_group_delete(
    name: String,
    state: State<'_, HostState>,
) -> Result<u32, AppError> {
    let clipboard = state.clipboard.clone();
    let n = tauri::async_runtime::spawn_blocking(move || clipboard.delete_group(&name))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))??;
    publish_group_counts(&state.bus, &state.clipboard);
    Ok(n)
}

#[tauri::command]
pub async fn clipboard_suggestions(
    limit: Option<u32>,
    state: State<'_, HostState>,
) -> Result<Vec<clipboard_core::types::SuggestionDto>, AppError> {
    let clipboard = state.clipboard.clone();
    let limit = limit.unwrap_or(100);
    tauri::async_runtime::spawn_blocking(move || clipboard.suggestions(limit))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))?
}

/// 采纳/忽略建议：两路都只动建议位，忽略不改分组（01§5-1 用户点才算数）
#[tauri::command]
pub async fn clipboard_suggestion_apply(
    ids: Vec<String>,
    accept: bool,
    state: State<'_, HostState>,
) -> Result<u32, AppError> {
    let clipboard = state.clipboard.clone();
    let n = tauri::async_runtime::spawn_blocking(move || clipboard.apply_suggestion(&ids, accept))
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))??;
    if accept {
        publish_group_counts(&state.bus, &state.clipboard);
    }
    Ok(n)
}

#[tauri::command]
pub async fn clipboard_stats(
    state: State<'_, HostState>,
) -> Result<clipboard_core::types::StatsDto, AppError> {
    let clipboard = state.clipboard.clone();
    tauri::async_runtime::spawn_blocking(move || clipboard.stats())
        .await
        .map_err(|e| AppError::module("CLIPBOARD_QUERY_002", e.to_string(), None))?
}

/// 分组写口成功后广播一次计数（SubNav 角标与左树同源，前端不必各自重拉）
fn publish_group_counts(
    bus: &host_core::events::EventBus,
    clipboard: &clipboard_core::module::ClipboardModule,
) {
    if let Ok(counts) = clipboard.group_counts() {
        bus.publish(host_core::events::Event::new(
            "clipboard.groups_changed",
            "clipboard",
            counts,
        ))
        .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::ports::RawInput;

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-3）字面测试名优先于 rustc 命名惯例
    fn pasteKeySeq_fourElementsCtrlVInOrder() {
        let shape: Vec<String> = PASTE_KEY_SEQ
            .iter()
            .map(|e| match e {
                RawInput::KeyDown { vk, scan } => format!("down {vk} {scan}"),
                RawInput::KeyUp { vk, scan } => format!("up {vk} {scan}"),
                other => format!("other {other:?}"),
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                "down 17 29".to_string(),
                "down 86 47".to_string(),
                "up 86 47".to_string(),
                "up 17 29".to_string(),
            ],
            "Ctrl 按住 → V 按下 → V 抬起 → Ctrl 抬起：vk/scan 任一错位都会注入成别的键",
        );
    }
}
