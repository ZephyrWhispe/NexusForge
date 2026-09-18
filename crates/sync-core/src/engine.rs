//! SYNC3 冲突解决（docs/impl/07 SYNC3）：默认 LWW(ts, device_id 字典序破平)。
//!
//! v1 裁剪：不做 diff-match-patch 3-way 文本合并（依赖重、价值密度低）；
//! LWW 输掉的本地改动以 sync.conflict 事件通知（UI 可查），数据本身不丢
//! （op_log 仍保留本地条目，下一次本地修改 ts 更新即可胜出）。

use uuid::Uuid;

use crate::error::{Result, SyncError};
use crate::oplog::{OpEntry, OpLog};

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

pub struct SyncEngine;

impl SyncEngine {
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
    use std::sync::{Arc, Mutex};
    use serde_json::json;

    /// 内存数据集（HashMap 投影）
    #[derive(Default)]
    struct FakeStore {
        data: Mutex<std::collections::HashMap<String, serde_json::Value>>,
    }
    impl ChangeApplier for FakeStore {
        fn snapshot(&self, _entity: &str, id: &str) -> Result<Option<serde_json::Value>> {
            Ok(self.data.lock().unwrap().get(id).cloned())
        }
        fn apply_upsert(&self, _entity: &str, id: &str, value: &serde_json::Value) -> Result<()> {
            self.data.lock().unwrap().insert(id.to_string(), value.clone());
            Ok(())
        }
        fn apply_delete(&self, _entity: &str, id: &str) -> Result<()> {
            self.data.lock().unwrap().remove(id);
            Ok(())
        }
    }

    fn log(tag: &str) -> OpLog {
        OpLog::open(&std::env::temp_dir().join(format!("nf_sync_eng_{tag}_{}.db", std::process::id()))).unwrap()
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
        assert_eq!(SyncEngine::apply_remote(&l, &store, &o1).unwrap(), ApplyOutcome::Applied);
        assert_eq!(store.data.lock().unwrap()["a.md"]["content"], "v1");
        // 旧 ts 到达 → LostLww，数据集不动
        let o0 = op("r0", "a.md", 50, "devB", "old");
        assert_eq!(SyncEngine::apply_remote(&l, &store, &o0).unwrap(), ApplyOutcome::LostLww);
        assert_eq!(store.data.lock().unwrap()["a.md"]["content"], "v1");
        // 更新 ts → 覆盖
        let o2 = op("r2", "a.md", 200, "devB", "v2");
        assert_eq!(SyncEngine::apply_remote(&l, &store, &o2).unwrap(), ApplyOutcome::Applied);
        assert_eq!(store.data.lock().unwrap()["a.md"]["content"], "v2");
        // 重放同 op → Noop
        assert_eq!(SyncEngine::apply_remote(&l, &store, &o2).unwrap(), ApplyOutcome::Noop);
    }

    #[test]
    fn tie_broken_by_device_id() {
        let l = log("tie");
        let store = FakeStore::default();
        // 同 ts："devB" > "devA" → devB 胜
        l.append(&op("t1", "b.md", 100, "devA", "from-a")).unwrap();
        let incoming = op("t2", "b.md", 100, "devB", "from-b");
        assert_eq!(SyncEngine::apply_remote(&l, &store, &incoming).unwrap(), ApplyOutcome::Applied);
        assert_eq!(store.data.lock().unwrap()["b.md"]["content"], "from-b");
        // 反向：本地 device 字典序更大 → incoming 输
        let l2 = log("tie2");
        let store2 = FakeStore::default();
        l2.append(&op("t3", "c.md", 100, "devZ", "local-z")).unwrap();
        store2.apply_upsert("note", "c.md", &json!({ "content": "local-z" })).unwrap();
        let incoming = op("t4", "c.md", 100, "devA", "from-a");
        assert_eq!(SyncEngine::apply_remote(&l2, &store2, &incoming).unwrap(), ApplyOutcome::LostLww);
        assert_eq!(store2.data.lock().unwrap()["c.md"]["content"], "local-z");
    }

    #[test]
    fn delete_propagates() {
        let l = log("del");
        let store = FakeStore::default();
        store.apply_upsert("note", "d.md", &json!({ "content": "x" })).unwrap();
        let del = OpEntry {
            op_id: "d1".into(),
            entity: "note".into(),
            entity_id: "d.md".into(),
            ts: 300,
            device: "devB".into(),
            value: json!({ "deleted": true }),
        };
        assert_eq!(SyncEngine::apply_remote(&l, &store, &del).unwrap(), ApplyOutcome::Applied);
        assert!(store.data.lock().unwrap().get("d.md").is_none());
    }

    #[test]
    fn record_local_appends_with_device() {
        let l = log("rec");
        let op = SyncEngine::record_local(&l, "note", "n.md", json!({"content": "hi"}), "self", 42).unwrap();
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
}
