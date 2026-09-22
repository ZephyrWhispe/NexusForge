//! SYNC3 冲突解决（docs/impl/07 SYNC3）：默认 LWW(ts, device_id 字典序破平)。
//!
//! v1 裁剪：不做 diff-match-patch 3-way 文本合并（依赖重、价值密度低）；
//! LWW 输掉的本地改动以 sync.conflict 事件通知（UI 可查），数据本身不丢
//! （op_log 仍保留本地条目，下一次本地修改 ts 更新即可胜出）。
//!
//! T-B5-2 起，判负一侧的**内容快照**另落 `conflict_log`（见 oplog.rs）：
//! 事件是提示不是事实源，只发事件等于"阅后即焚"。

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{Result, SyncError};
use crate::oplog::{ConflictEntry, OpEntry, OpLog};

/// 变更应用回调（宿主注入：notes → NoteLibrary；v1 数据集 = note）
pub trait ChangeApplier: Send + Sync {
    /// 实体当前快照（record_local 入库前读取；None = 实体不存在）
    fn snapshot(&self, entity: &str, entity_id: &str) -> Result<Option<serde_json::Value>>;
    /// 应用远端 upsert（写穿数据集）
    fn apply_upsert(&self, entity: &str, entity_id: &str, value: &serde_json::Value) -> Result<()>;
    /// 应用远端删除
    fn apply_delete(&self, entity: &str, entity_id: &str) -> Result<()>;
}

/// 本地应用远端变更的结果
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// 已应用（写入数据集 + op_log）
    Applied,
    /// 重复/过期丢弃（LWW 输或同 op 重放；数据集不动）
    LostLww,
    /// 本地与新值内容一致（无操作但游标推进）
    Noop,
}

/// 推送批次切分结果（纯函数产物，调用方据此写帧并驱动出站游标）
#[derive(Clone, Debug)]
pub struct PushBatch {
    pub ops: Vec<OpEntry>,
    /// 本批恰满 limit ⇒ 可能还有下一批（对端回执后继续按游标取）
    pub more: bool,
}

pub struct SyncEngine;

impl SyncEngine {
    /// 切分推送批次：raw 须是 `ops_of_device(device, push_cursor, limit)` 的升序结果，
    /// 切分本身不触 IO（§9.1-④ 两半制：语义可直测，游标读写留在会话里）。
    pub fn plan_push(raw: Vec<OpEntry>, limit: usize) -> PushBatch {
        let more = limit > 0 && raw.len() == limit;
        PushBatch { ops: raw, more }
    }

    /// 冲突行的**确定性**主键：sha256(entity ‖ \0 ‖ entity_id ‖ \0 ‖ lost_ts ‖ \0 ‖ lost_device) 前 32 hex。
    ///
    /// 与 op_id 的 UUIDv7 刻意分野：op 是"一次事实"（每次产生都得新 id），冲突行是
    /// "同一个败方快照被再次观测到"——对端没收到 Ack 而重推同一批时，UUID 会把一次
    /// 网络抖动灌成一堆重复历史，确定性 id 让 `INSERT OR IGNORE` 天然吸收。
    pub fn conflict_id(entity: &str, entity_id: &str, lost_ts: i64, lost_device: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(entity.as_bytes());
        hasher.update([0u8]);
        hasher.update(entity_id.as_bytes());
        hasher.update([0u8]);
        hasher.update(lost_ts.to_le_bytes());
        hasher.update([0u8]);
        hasher.update(lost_device.as_bytes());
        // 取前 16 字节 = 32 个 hex 字符（冲突行非信任根素材，截断只为可读可贴）
        hasher
            .finalize()
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// 由败方条目 + 当时压住它的本地条目组装冲突行（纯函数，落盘时机留在会话里）。
    ///
    /// `winner` 正常必为 `Some`（判负的前提就是本地有更新条目）；`None` 只可能是
    /// 查表 IO 失败，此时以空胜者如实留痕——快照本身才是用户要的东西，不因审计列缺失而丢行。
    pub fn conflict_of(
        lost: &OpEntry,
        winner: Option<&OpEntry>,
        recorded_ms: i64,
    ) -> ConflictEntry {
        ConflictEntry {
            conflict_id: Self::conflict_id(&lost.entity, &lost.entity_id, lost.ts, &lost.device),
            entity: lost.entity.clone(),
            entity_id: lost.entity_id.clone(),
            lost_ts: lost.ts,
            lost_device: lost.device.clone(),
            winner_device: winner.map(|w| w.device.clone()).unwrap_or_default(),
            winner_ts: winner.map(|w| w.ts).unwrap_or(0),
            lost_value: lost.value.clone(),
            recorded_ms,
        }
    }

    /// 记录本地变更：snapshot 由调用方提供（避免 engine 依赖 applier 时机）
    pub fn record_local(
        log: &OpLog,
        entity: &str,
        entity_id: &str,
        value: serde_json::Value,
        device: &str,
        now_ms: i64,
    ) -> Result<OpEntry> {
        let op = OpEntry {
            op_id: Uuid::now_v7().to_string(),
            entity: entity.to_string(),
            entity_id: entity_id.to_string(),
            ts: now_ms,
            device: device.to_string(),
            value,
        };
        log.append(&op)?;
        Ok(op)
    }

    /// 应用远端变更（LWW）；返回结果供游标推进与统计
    pub fn apply_remote(
        log: &OpLog,
        applier: &dyn ChangeApplier,
        incoming: &OpEntry,
    ) -> Result<ApplyOutcome> {
        let latest = log.latest_for(&incoming.entity, &incoming.entity_id)?;
        match latest {
            Some(local) if local.op_id == incoming.op_id => return Ok(ApplyOutcome::Noop),
            Some(local) if local.ts > incoming.ts => return Ok(ApplyOutcome::LostLww),
            Some(local) if local.ts == incoming.ts && local.device >= incoming.device => {
                // ts 破平：device_id 字典序大者胜；平局（同 device 同 ts，理论不可能）本地保守
                return Ok(ApplyOutcome::LostLww);
            }
            _ => {}
        }
        // LWW 胜出 → 写穿数据集
        if incoming.is_delete() {
            applier
                .apply_delete(&incoming.entity, &incoming.entity_id)
                .map_err(|e| SyncError::Apply(e.to_string()))?;
        } else {
            applier
                .apply_upsert(&incoming.entity, &incoming.entity_id, &incoming.value)
                .map_err(|e| SyncError::Apply(e.to_string()))?;
        }
        log.append(incoming)?;
        Ok(ApplyOutcome::Applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::BATCH_LIMIT;
    use parking_lot::Mutex;
    use serde_json::json;
    use std::sync::Arc;

    /// 内存数据集（HashMap 投影）
    #[derive(Default)]
    struct FakeStore {
        data: Mutex<std::collections::HashMap<String, serde_json::Value>>,
    }
    impl ChangeApplier for FakeStore {
        fn snapshot(&self, _entity: &str, id: &str) -> Result<Option<serde_json::Value>> {
            Ok(self.data.lock().get(id).cloned())
        }
        fn apply_upsert(&self, _entity: &str, id: &str, value: &serde_json::Value) -> Result<()> {
            self.data.lock().insert(id.to_string(), value.clone());
            Ok(())
        }
        fn apply_delete(&self, _entity: &str, id: &str) -> Result<()> {
            self.data.lock().remove(id);
            Ok(())
        }
    }

    fn log(tag: &str) -> OpLog {
        OpLog::open(
            &std::env::temp_dir().join(format!("nf_sync_eng_{tag}_{}.db", uuid::Uuid::now_v7())),
        )
        .unwrap()
    }

    fn op(id: &str, eid: &str, ts: i64, device: &str, content: &str) -> OpEntry {
        OpEntry {
            op_id: id.into(),
            entity: "note".into(),
            entity_id: eid.into(),
            ts,
            device: device.into(),
            value: json!({ "content": content }),
        }
    }

    #[test]
    fn new_entity_applies_and_lww_wins_by_ts() {
        let l = log("lww");
        let store = FakeStore::default();
        // 新实体直接应用
        let o1 = op("r1", "a.md", 100, "devB", "v1");
        assert_eq!(
            SyncEngine::apply_remote(&l, &store, &o1).unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(store.data.lock()["a.md"]["content"], "v1");
        // 旧 ts 到达 → LostLww，数据集不动
        let o0 = op("r0", "a.md", 50, "devB", "old");
        assert_eq!(
            SyncEngine::apply_remote(&l, &store, &o0).unwrap(),
            ApplyOutcome::LostLww
        );
        assert_eq!(store.data.lock()["a.md"]["content"], "v1");
        // 更新 ts → 覆盖
        let o2 = op("r2", "a.md", 200, "devB", "v2");
        assert_eq!(
            SyncEngine::apply_remote(&l, &store, &o2).unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(store.data.lock()["a.md"]["content"], "v2");
        // 重放同 op → Noop
        assert_eq!(
            SyncEngine::apply_remote(&l, &store, &o2).unwrap(),
            ApplyOutcome::Noop
        );
    }

    #[test]
    fn tie_broken_by_device_id() {
        let l = log("tie");
        let store = FakeStore::default();
        // 同 ts："devB" > "devA" → devB 胜
        l.append(&op("t1", "b.md", 100, "devA", "from-a")).unwrap();
        let incoming = op("t2", "b.md", 100, "devB", "from-b");
        assert_eq!(
            SyncEngine::apply_remote(&l, &store, &incoming).unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(store.data.lock()["b.md"]["content"], "from-b");
        // 反向：本地 device 字典序更大 → incoming 输
        let l2 = log("tie2");
        let store2 = FakeStore::default();
        l2.append(&op("t3", "c.md", 100, "devZ", "local-z"))
            .unwrap();
        store2
            .apply_upsert("note", "c.md", &json!({ "content": "local-z" }))
            .unwrap();
        let incoming = op("t4", "c.md", 100, "devA", "from-a");
        assert_eq!(
            SyncEngine::apply_remote(&l2, &store2, &incoming).unwrap(),
            ApplyOutcome::LostLww
        );
        assert_eq!(store2.data.lock()["c.md"]["content"], "local-z");
    }

    #[test]
    fn delete_propagates() {
        let l = log("del");
        let store = FakeStore::default();
        store
            .apply_upsert("note", "d.md", &json!({ "content": "x" }))
            .unwrap();
        let del = OpEntry {
            op_id: "d1".into(),
            entity: "note".into(),
            entity_id: "d.md".into(),
            ts: 300,
            device: "devB".into(),
            value: json!({ "deleted": true }),
        };
        assert_eq!(
            SyncEngine::apply_remote(&l, &store, &del).unwrap(),
            ApplyOutcome::Applied
        );
        assert!(store.data.lock().get("d.md").is_none());
    }

    #[test]
    fn record_local_appends_with_device() {
        let l = log("rec");
        let op = SyncEngine::record_local(&l, "note", "n.md", json!({"content": "hi"}), "self", 42)
            .unwrap();
        assert_eq!(op.device, "self");
        assert_eq!(l.count(), 1);
        assert_eq!(l.latest_for("note", "n.md").unwrap().unwrap().ts, 42);
    }

    /// Arc<dyn ChangeApplier> 对象安全（宿主注入场景）
    #[test]
    fn applier_object_safe() {
        let a: Arc<dyn ChangeApplier> = Arc::new(FakeStore::default());
        assert!(a.snapshot("note", "x").unwrap().is_none());
    }

    /// 任务书（09 §10.2 T-B5-1）字面测试名优先于 rustc 命名惯例
    #[test]
    #[allow(non_snake_case)]
    fn plan_push_boundary_exactLimit_setsMore() {
        let all: Vec<OpEntry> = (0..600)
            .map(|i| op(&format!("p{i}"), &format!("{i}.md"), 1_000 + i, "devA", "x"))
            .collect();

        // 600 条自产 ⇒ 512 + 88 两批（首批恰满 → more=true；次批不满 → more=false）
        let mut cursor = 0i64;
        let mut batches: Vec<(usize, bool)> = Vec::new();
        loop {
            let raw: Vec<OpEntry> = all
                .iter()
                .filter(|o| o.ts > cursor)
                .take(BATCH_LIMIT)
                .cloned()
                .collect();
            let batch = SyncEngine::plan_push(raw, BATCH_LIMIT);
            let n = batch.ops.len();
            let more = batch.more;
            if let Some(last) = batch.ops.last() {
                cursor = last.ts;
            }
            batches.push((n, more));
            if !more {
                break;
            }
        }
        assert_eq!(batches, vec![(512, true), (88, false)]);
        assert_eq!(cursor, 1_599, "两批跑完出站游标落在最后一条 ts");

        // 511 条（差一条不满）⇒ 无后续批
        let slim: Vec<OpEntry> = all.iter().take(BATCH_LIMIT - 1).cloned().collect();
        assert!(!SyncEngine::plan_push(slim, BATCH_LIMIT).more);

        // 空批（游标已对齐）⇒ 不标记续传，会话据此零推送退出
        let empty = SyncEngine::plan_push(vec![], BATCH_LIMIT);
        assert!(empty.ops.is_empty());
        assert!(!empty.more);
    }
}
