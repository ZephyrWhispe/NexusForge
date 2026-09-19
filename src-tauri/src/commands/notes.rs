use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

// ======================== 笔记与知识（M10 N，docs/impl/06） ========================

fn notes_err(e: notes_core::NoteError) -> AppError {
    AppError::module(e.code(), e.to_string(), None)
}

use host_core::util::now_ms;

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
pub async fn notes_list(
    state: State<'_, HostState>,
) -> Result<Vec<notes_core::model::NoteMeta>, AppError> {
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
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
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
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
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
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
        .inspect(|meta| {
            notes_notify_inner(&state, "create", Some(&meta.path));
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
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
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
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
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
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
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
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
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
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.backlinks(&rel_path))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map_err(notes_err)
}

/// 增量索引（外部编辑器改动收敛）
#[tauri::command]
pub async fn notes_sync(
    state: State<'_, HostState>,
) -> Result<notes_core::model::SyncResult, AppError> {
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.sync())
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .inspect(|_r| {
            notes_notify_inner(&state, "sync", None);
        })
        .map_err(notes_err)
}

/// 全量重建索引
#[tauri::command]
pub async fn notes_reindex(
    state: State<'_, HostState>,
) -> Result<notes_core::model::SyncResult, AppError> {
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.reindex())
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .inspect(|_r| {
            notes_notify_inner(&state, "reindex", None);
        })
        .map_err(notes_err)
}

// ---- N4 复习 ----

/// 全部卡片
#[tauri::command]
pub async fn notes_cards(
    state: State<'_, HostState>,
) -> Result<Vec<notes_core::model::Card>, AppError> {
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
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
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    let now = now_ms();
    tauri::async_runtime::spawn_blocking(move || m.cards().create(&front, &back, note_path, now))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .inspect(|_card| {
            notes_notify_inner(&state, "cards", None);
        })
        .map_err(notes_err)
}

/// 删除卡片
#[tauri::command]
pub async fn notes_card_delete(id: String, state: State<'_, HostState>) -> Result<bool, AppError> {
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.cards().delete(&id))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .inspect(|_ok| {
            notes_notify_inner(&state, "cards", None);
        })
        .map_err(notes_err)
}

/// 今天到期队列
#[tauri::command]
pub async fn notes_review_queue(
    state: State<'_, HostState>,
) -> Result<Vec<notes_core::model::Card>, AppError> {
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
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
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    let now = now_ms();
    tauri::async_runtime::spawn_blocking(move || m.grade_card(&id, quality, now))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .inspect(|_card| {
            notes_notify_inner(&state, "cards", None);
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
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
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
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
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
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.canvas_dirs())
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map_err(notes_err)
}
