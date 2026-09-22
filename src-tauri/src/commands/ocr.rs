use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

// ---------------- OCR 命令（docs/impl/04 O7）----------------

/// 识别（PNG Base64 输入；spawn_blocking + 30s 超时）
///
/// `request.langs` 是本次的显式覆盖：传空数组 **不是**"无偏好"，而是"跟随设置里的
/// 偏好语言"（由 `EngineRegistry::resolve_langs` 单点解析），覆盖层取字即走此臂。
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

/// 运行态 OCR 配置快照（T-B4-10：面板占位文案显示"设置里的默认"，读的是模块内存态；
/// 写侧仍只有 host_config_set 一个口）
#[tauri::command]
pub fn ocr_config_get(state: State<'_, HostState>) -> ocr_core::types::OcrConfigDto {
    state.ocr.config()
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

/// 批量识别的合并文本导出（T-B4-12）：格式白名单 `txt | md`，落 `{app_data}/export/`。
/// 命令**不收目录入参**——路径由宿主自己拼，所以"只能写导出目录"是结构性的而非约定性的
/// （同 B3 `clipboard_export`；导入侧才有路径 Input，且只走读侧）。
#[tauri::command]
pub async fn ocr_export(
    text: String,
    format: String,
    state: State<'_, HostState>,
) -> Result<String, AppError> {
    let app_data = state.app_data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        ocr_core::export::write_export(&app_data, &text, &format).map(|p| p.display().to_string())
    })
    .await
    .map_err(|e| AppError::module("OCR_EXPORT_004", e.to_string(), None))?
}
