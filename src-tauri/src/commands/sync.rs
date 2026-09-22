use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

// ======================== 跨设备同步（M15 SYNC，docs/impl/07） ========================

fn sync_err(e: sync_core::SyncError) -> AppError {
    AppError::module(e.code(), e.to_string(), None)
}

/// 配对设备列表（信任根复用 KVM 配对；UI 选择同步目标）
///
/// `peers()` 走 PairStore（内存表 + 磁盘读），与其余同步读命令同规走 `spawn_blocking`
/// （09 §10.1 承重⑬ 随行修：本模块三条读命令过去全是 `async fn` 里直接做阻塞 I/O）。
#[tauri::command]
pub async fn sync_peers(
    state: State<'_, HostState>,
) -> Result<Vec<kvm_core::PairedPeer>, AppError> {
    let sync = state.sync.clone();
    tauri::async_runtime::spawn_blocking(move || sync.peers())
        .await
        .map_err(|e| sync_err(sync_core::SyncError::Db(format!("配对读任务失败：{e}"))))
}

/// 同步状态快照（T-B5-4 类型化：DTO 就是 sync-core 的 `SyncStatus`，单一真源）
///
/// 本命令的返回类型过去是无类型 JSON 值——里面有什么键全靠人记，
/// 加键/漏键两端都不报错，面板因此可以在内核根本没监听时说"监听 :49820"。
/// 类型化后编译期就是这道门（09 §10.2 T-B5-4 的机检面：本文件 grep 不到 `Value` 返回）。
pub type SyncStatusDto = sync_core::SyncStatus;

#[tauri::command]
pub async fn sync_status(state: State<'_, HostState>) -> Result<SyncStatusDto, AppError> {
    let sync = state.sync.clone();
    tauri::async_runtime::spawn_blocking(move || sync.status())
        .await
        .map_err(|e| sync_err(sync_core::SyncError::Db(format!("状态读任务失败：{e}"))))?
        .map_err(sync_err)
}

/// 立即与指定设备同步（addr 如 "192.168.1.10:49820"；端口默认 DEFAULT_SYNC_PORT）
///
/// 本命令保持 `async`：会话本体是 await 网络 I/O，整段塞进 `spawn_blocking` 不可表达。
/// ⑬ 的适用面在这里只剩"会话内的 op_log 读写"，其量级按批封顶（`BATCH_LIMIT`），
/// 与三条读命令的"面板每开一次就吃一次 worker"不是同一风险面——差异已记 09 §10.2
/// T-B5-4 落地补记，不在这里悄悄留个注释就当没说。
#[tauri::command]
pub async fn sync_now(
    device_id: String,
    addr: String,
    state: State<'_, HostState>,
) -> Result<sync_core::SyncSummary, AppError> {
    state
        .sync
        .sync_with(&device_id, &addr)
        .await
        .map_err(sync_err)
}

/// 冲突行 DTO（09 §10.2 T-B5-2）：直接透出 sync-core 结构 = 单一真源，
/// 不另立字段副本（承重⑭ 的教训：两处声明同形靠人记，一处声明靠编译器记）。
pub type SyncConflictDto = sync_core::ConflictEntry;

/// 恢复结果最小形状：只说"这条快照已作为本机新变更入流"，**不含**任何"对端已回滚"承诺
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRestoreDto {
    pub conflict_id: String,
    pub entity: String,
    pub entity_id: String,
    /// 新入流的 op（可据此在变更流里对上号）
    pub op_id: String,
    /// 新 op 的时间戳（大于当时胜者 ts ⇒ 下轮 LWW 由它胜出）
    pub ts: i64,
}

/// 冲突历史分页（读侧 LIMIT 化：表可增长，视图不跟着无界）
#[tauri::command]
pub async fn sync_conflicts_get(
    limit: u32,
    offset: u32,
    state: State<'_, HostState>,
) -> Result<Vec<SyncConflictDto>, AppError> {
    let sync = state.sync.clone();
    let limit = (limit.min(200)) as i64;
    tauri::async_runtime::spawn_blocking(move || sync.conflicts(limit, offset as i64))
        .await
        .map_err(|e| sync_err(sync_core::SyncError::Db(format!("冲突读任务失败：{e}"))))?
        .map_err(sync_err)
}

/// 以本地留存的败方快照重新生效并推送（文案红线：不称"撤销对端/强制回滚"）
#[tauri::command]
pub async fn sync_conflict_restore(
    conflict_id: String,
    state: State<'_, HostState>,
) -> Result<SyncRestoreDto, AppError> {
    let sync = state.sync.clone();
    let id = conflict_id.clone();
    let op = tauri::async_runtime::spawn_blocking(move || sync.restore_conflict(&id))
        .await
        .map_err(|e| sync_err(sync_core::SyncError::Db(format!("冲突恢复任务失败：{e}"))))?
        .map_err(sync_err)?;
    Ok(SyncRestoreDto {
        conflict_id,
        entity: op.entity,
        entity_id: op.entity_id,
        op_id: op.op_id,
        ts: op.ts,
    })
}

/// 流水行 DTO（09 §10.2 T-B5-3）：同 `SyncConflictDto` 的取舍——直接透出 sync-core 结构。
pub type SyncRunDto = sync_core::SyncRun;

/// 同步活动流水（新行在前）。失败行照实返回（`error` 非空），面板不得只渲染成功行。
#[tauri::command]
pub async fn sync_runs_get(
    limit: u32,
    state: State<'_, HostState>,
) -> Result<Vec<SyncRunDto>, AppError> {
    let sync = state.sync.clone();
    let limit = (limit.min(200)) as i64;
    tauri::async_runtime::spawn_blocking(move || sync.runs(limit))
        .await
        .map_err(|e| sync_err(sync_core::SyncError::Db(format!("同步流水读任务失败：{e}"))))?
        .map_err(sync_err)
}

/// 暂停/恢复同步（09 §10.2 T-B5-6）：**唯一写口**，面板与以后托盘两处入口共用同一实现点
/// （B3 沉淀纪律：两个入口两份实现迟早漂移，漂移就是"徽章说暂停了内核还在发"）。
///
/// 不进 `spawn_blocking`：这一枚既无磁盘也无网络——写一个原子位、掐掉在等的静默窗。
/// 恢复**不补跑**（语义是"以后照常"，要立刻出账那里有 `sync_now`）。状态读面
/// `sync_status().paused` 与本命令读同一位，因此面板回读到的就是内核此刻真的那套开关。
#[tauri::command]
pub async fn sync_set_paused(paused: bool, state: State<'_, HostState>) -> Result<(), AppError> {
    state.sync.set_paused(paused);
    Ok(())
}
