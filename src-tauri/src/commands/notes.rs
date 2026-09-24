use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

// ======================== 笔记与知识（M10 N，docs/impl/06） ========================

fn notes_err(e: notes_core::NoteError) -> AppError {
    AppError::module(e.code(), e.to_string(), None)
}

use host_core::util::now_ms;

/// notes.changed 载荷（单一构造点：同步层按 `action`/`path`/`old_path` 分派，
/// 形状在这里、消费在 `sync_core::change_records_of_event`，两侧不各写一份字面量）
fn notes_change_payload(
    action: &str,
    path: Option<&str>,
    old_path: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "action": action,
        "path": path,
        "old_path": old_path,
    })
}

/// 变更事件（UI 事件驱动刷新；topic 见 host-core TOPIC_REGISTRY）
///
/// `old_path` 仅 rename 用：同步层据此记"旧路径删除 + 新路径写入"两笔。
/// 不带旧路径的 rename 会被整条丢弃（宁可远端不改名，也不要远端凭空多出一份新笔记、
/// 旧笔记原地留着——那是 T-B5-5 之前 rename 的真实下场）。
fn notes_notify(state: &HostState, action: &str, path: Option<&str>, old_path: Option<&str>) {
    state
        .bus
        .publish(host_core::events::Event::new(
            "notes.changed",
            "notes",
            notes_change_payload(action, path, old_path),
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
            notes_notify_inner(&state, "create", Some(&meta.path), None);
        })
        .map_err(notes_err)
}

fn notes_notify_inner(
    state: &State<'_, HostState>,
    action: &str,
    path: Option<&str>,
    old_path: Option<&str>,
) {
    notes_notify(state.inner(), action, path, old_path);
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
            notes_notify_inner(&state, "write", Some(&rel2), None);
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
            notes_notify_inner(&state, "delete", Some(&rel2), None);
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
    let old2 = old_path.clone();
    tauri::async_runtime::spawn_blocking(move || m.rename(&old_path, &new_path))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map(|_| {
            // 旧路径一并带上：同步层据此记 delete(old)+write(new) 两笔
            notes_notify_inner(&state, "rename", Some(&new2), Some(&old2));
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
            notes_notify_inner(&state, "sync", None, None);
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
            notes_notify_inner(&state, "reindex", None, None);
        })
        .map_err(notes_err)
}

/// 正文全文搜索（T-B7-21 FTS5；只读，不发变更事件）
#[tauri::command]
pub async fn notes_search(
    query: String,
    limit: u32,
    state: State<'_, HostState>,
) -> Result<Vec<notes_core::model::SearchHit>, AppError> {
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.search(&query, limit))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
        .map_err(notes_err)
}

/// 按标签精确查询（T-B7-22；只读）
#[tauri::command]
pub async fn notes_by_tag(
    tag: String,
    state: State<'_, HostState>,
) -> Result<Vec<notes_core::model::NoteMeta>, AppError> {
    let m = state
        .notes
        .library()
        .ok_or_else(|| AppError::module("NOTE_IPC_001", "笔记模块未就绪", None))?;
    tauri::async_runtime::spawn_blocking(move || m.by_tag(&tag))
        .await
        .map_err(|e| AppError::module("NOTE_IPC_001", e.to_string(), None))?
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
            notes_notify_inner(&state, "cards", None, None);
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
            notes_notify_inner(&state, "cards", None, None);
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
            notes_notify_inner(&state, "cards", None, None);
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
            notes_notify_inner(&state, "canvas", Some(&dir2), None);
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

#[cfg(test)]
mod tests {
    use super::notes_change_payload;
    use host_core::device::{DeviceIdentity, PairStore};
    use host_core::events::{Event, EventBus};
    use std::collections::HashMap;
    use std::sync::Arc;
    use sync_core::{ChangeApplier, OpLog, SyncCtx, ENTITY_NOTE};

    /// 内存笔记库（只需回答快照；本测试关心的是入流的两笔 op 形状）
    #[derive(Default)]
    struct MemNotes {
        data: parking_lot::Mutex<HashMap<String, serde_json::Value>>,
    }
    impl ChangeApplier for MemNotes {
        fn snapshot(
            &self,
            _entity: &str,
            id: &str,
        ) -> sync_core::Result<Option<serde_json::Value>> {
            Ok(self.data.lock().get(id).cloned())
        }
        fn apply_upsert(
            &self,
            _entity: &str,
            id: &str,
            value: &serde_json::Value,
        ) -> sync_core::Result<()> {
            self.data.lock().insert(id.to_string(), value.clone());
            Ok(())
        }
        fn apply_delete(&self, _entity: &str, id: &str) -> sync_core::Result<()> {
            self.data.lock().remove(id);
            Ok(())
        }
    }

    fn ctx(tag: &str) -> (Arc<SyncCtx>, Arc<MemNotes>, std::path::PathBuf) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("nf_notes_sync_{tag}_{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let identity = Arc::new(DeviceIdentity::load_or_create(&dir, None).unwrap());
        let store = Arc::new(PairStore::load_or_default(&dir).unwrap());
        let log = Arc::new(OpLog::open(&dir.join("sync.db")).unwrap());
        let notes = Arc::new(MemNotes::default());
        let mut appliers: HashMap<String, Arc<dyn ChangeApplier>> = HashMap::new();
        appliers.insert(ENTITY_NOTE.to_string(), notes.clone());
        let ctx = Arc::new(SyncCtx {
            identity,
            store,
            log,
            appliers: Arc::new(appliers),
            bus: Some(Arc::new(EventBus::new())),
        });
        (ctx, notes, dir)
    }

    /// 任务书（09 §10.2 T-B5-5）字面测试名：改名在变更流里是"旧删 + 新写"两笔
    ///
    /// 放在 src-tauri 层是因为这条链的断点在两侧交界处：载荷由 `notes_change_payload`
    /// 造、由 sync-core 解析消费。只测 sync-core 会漏掉"发布方根本没带 old_path"，
    /// 只测发布方会漏掉"带了也没人拆成两笔"。
    #[test]
    #[allow(non_snake_case)]
    fn syncEntity_renameRecordsDeleteOldAndUpsertNew() {
        let (ctx, notes, dir) = ctx("rename");
        // 改名后磁盘上只有新路径（旧路径读不到——正因如此 old_path 必须来自事件本身）
        notes.data.lock().insert(
            "new.md".into(),
            serde_json::json!({ "content": "# 改名后" }),
        );

        let payload = notes_change_payload("rename", Some("new.md"), Some("old.md"));
        assert_eq!(payload["action"], "rename");
        assert_eq!(payload["old_path"], "old.md");
        let event = Event::new("notes.changed", "notes", payload);
        let dev = ctx.identity.device_id.clone();

        assert_eq!(sync_core::record_change_event(&ctx, &event).unwrap(), 2);
        let ops = ctx.log.ops_of_device(&dev, 0, 10).unwrap();
        assert_eq!(ops.len(), 2, "一次改名两笔：旧删 + 新写");
        assert_eq!(ops[0].entity_id, "old.md", "顺序必须是 old → new");
        assert!(ops[0].is_delete(), "旧路径那笔是删除");
        assert_eq!(ops[1].entity_id, "new.md");
        assert!(!ops[1].is_delete());
        assert_eq!(ops[1].value["content"], "# 改名后", "新路径带的是真快照");

        // 负对照：缺 old_path（半截升级的发布方）整条丢弃，绝不留下"只写新不删旧"的半改名
        let half = Event::new(
            "notes.changed",
            "notes",
            notes_change_payload("rename", Some("x.md"), None),
        );
        assert_eq!(sync_core::record_change_event(&ctx, &half).unwrap(), 0);
        assert_eq!(ctx.log.ops_of_device(&dev, 0, 10).unwrap().len(), 2);
        // 索引/画布类事件连 path 都没有，同样零入流
        let reindex = Event::new(
            "notes.changed",
            "notes",
            notes_change_payload("reindex", None, None),
        );
        assert_eq!(sync_core::record_change_event(&ctx, &reindex).unwrap(), 0);
        assert_eq!(ctx.log.ops_of_device(&dev, 0, 10).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
