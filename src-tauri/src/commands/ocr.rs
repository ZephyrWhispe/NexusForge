use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

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

/// OCR 结果"复制全部"（D-10：走剪贴板回写窗口写入系统剪贴板，
/// 置 500ms 自捕获抑制，不产生新历史条目）
#[tauri::command]
pub async fn ocr_copy_text(text: String, state: State<'_, HostState>) -> Result<(), AppError> {
    use host_core::ports::ClipContent;
    let clipboard = state.clipboard.clone();
    let bus = state.bus.clone();
    tauri::async_runtime::spawn_blocking(move || {
        clipboard.write_back(&ClipContent::Text { text, html: None })
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
