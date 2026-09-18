//! SYNC2 变更流（docs/impl/07 SYNC2）：op_log 追加表 + 设备游标。
//!
//! op_log = 实体快照式变更记录（v1 实体级 LWW，字段级合并随 SYNC3 深化）：
//! `{ op_id(ULID→uuid v7), entity, entity_id, ts, device, value }`
//! value = 实体 JSON 快照（notes: {content, title}；删除 = {"deleted": true}）。
//! 同步 = 交换游标 → 拉取缺失 → 本地应用（LWW，见 engine.rs）。

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{Result, SyncError};

/// 删除标记（value 内）
pub const DELETED_KEY: &str = "deleted";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpEntry {
    pub op_id: String,
    pub entity: String,
    pub entity_id: String,
    /// 毫秒时间戳（LWW 主键）
    pub ts: i64,
    /// 产生该变更的设备 id（三方传递时保留原始 device）
    pub device: String,
    /// 实体 JSON 快照
    pub value: serde_json::Value,
}

impl OpEntry {
    pub fn is_delete(&self) -> bool {
        self.value.get(DELETED_KEY).and_then(|v| v.as_bool()).unwrap_or(false)
    }
}

/// op_log 追加存储（sync.db，WAL；独立库文件——docs/DESIGN.md 每模块独立库约定）
pub struct OpLog {
    conn: Arc<Mutex<Connection>>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS op_log (
    op_id     TEXT PRIMARY KEY,
    entity    TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    ts        INTEGER NOT NULL,
    device    TEXT NOT NULL,
    value     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_op_log_ts ON op_log(ts);
CREATE INDEX IF NOT EXISTS idx_op_log_entity ON op_log(entity, entity_id);
CREATE TABLE IF NOT EXISTS cursors (
    device  TEXT PRIMARY KEY,
    last_ts INTEGER NOT NULL DEFAULT 0
);
";

impl OpLog {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SyncError::Db(e.to_string()))?;
        }
        let conn = Connection::open(db_path).map_err(|e| SyncError::Db(e.to_string()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| SyncError::Db(e.to_string()))?;
        conn.execute_batch(SCHEMA).map_err(|e| SyncError::Db(e.to_string()))?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// 追加变更（op_id 主键 → INSERT OR IGNORE 幂等；重复推送无副作用）
    pub fn append(&self, e: &OpEntry) -> Result<()> {
        let conn = self.conn.lock().expect("op_log 锁污染");
        conn.execute(
            "INSERT OR IGNORE INTO op_log (op_id, entity, entity_id, ts, device, value)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                e.op_id,
                e.entity,
                e.entity_id,
                e.ts,
                e.device,
                serde_json::to_string(&e.value).map_err(|er| SyncError::Db(er.to_string()))?
            ],
        )
        .map_err(|er| SyncError::Db(er.to_string()))?;
        Ok(())
    }

    /// 拉取指定设备产出的变更（ts 升序；pull 响应方查自己的产出，push 方查自产）
    pub fn ops_of_device(&self, device: &str, since_ts: i64, limit: usize) -> Result<Vec<OpEntry>> {
        let conn = self.conn.lock().expect("op_log 锁污染");
        let mut stmt = conn
            .prepare(
                "SELECT op_id, entity, entity_id, ts, device, value FROM op_log
                 WHERE device = ?1 AND ts > ?2 ORDER BY ts LIMIT ?3",
            )
            .map_err(|e| SyncError::Db(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params![device, since_ts, limit as i64], row_to_op)
            .map_err(|e| SyncError::Db(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(|e| SyncError::Db(e.to_string()))
    }

    /// 实体当前最新变更（LWW 对比用）
    pub fn latest_for(&self, entity: &str, entity_id: &str) -> Result<Option<OpEntry>> {
        let conn = self.conn.lock().expect("op_log 锁污染");
        let mut stmt = conn
            .prepare(
                "SELECT op_id, entity, entity_id, ts, device, value FROM op_log
                 WHERE entity = ?1 AND entity_id = ?2 ORDER BY ts DESC, device DESC LIMIT 1",
            )
            .map_err(|e| SyncError::Db(e.to_string()))?;
        let mut rows = stmt
            .query_map(rusqlite::params![entity, entity_id], row_to_op)
            .map_err(|e| SyncError::Db(e.to_string()))?;
        rows.next().transpose().map_err(|e| SyncError::Db(e.to_string()))
    }

    /// 设备游标（从该设备已收到的最大 ts）
    pub fn cursor(&self, device: &str) -> i64 {
        let conn = self.conn.lock().expect("op_log 锁污染");
        conn.query_row(
            "SELECT last_ts FROM cursors WHERE device = ?1",
            rusqlite::params![device],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    /// 推进游标（只进不退）
    pub fn set_cursor(&self, device: &str, ts: i64) -> Result<()> {
        let conn = self.conn.lock().expect("op_log 锁污染");
        conn.execute(
            "INSERT INTO cursors (device, last_ts) VALUES (?1, ?2)
             ON CONFLICT(device) DO UPDATE SET last_ts = MAX(last_ts, ?2)",
            rusqlite::params![device, ts],
        )
        .map_err(|e| SyncError::Db(e.to_string()))?;
        Ok(())
    }

    /// op 总数（状态面板）
    pub fn count(&self) -> u64 {
        let conn = self.conn.lock().expect("op_log 锁污染");
        conn.query_row("SELECT COUNT(*) FROM op_log", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u64
    }
}

fn row_to_op(r: &rusqlite::Row<'_>) -> rusqlite::Result<OpEntry> {
    let value_raw: String = r.get(5)?;
    Ok(OpEntry {
        op_id: r.get(0)?,
        entity: r.get(1)?,
        entity_id: r.get(2)?,
        ts: r.get(3)?,
        device: r.get(4)?,
        value: serde_json::from_str(&value_raw).unwrap_or(serde_json::Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn op(id: &str, entity: &str, eid: &str, ts: i64, device: &str, content: &str) -> OpEntry {
        OpEntry {
            op_id: id.into(),
            entity: entity.into(),
            entity_id: eid.into(),
            ts,
            device: device.into(),
            value: json!({ "content": content }),
        }
    }

    fn log(tag: &str) -> OpLog {
        OpLog::open(&std::env::temp_dir().join(format!("nf_sync_{tag}_{}.db", std::process::id()))).unwrap()
    }

    #[test]
    fn append_idempotent_and_latest() {
        let l = log("append");
        l.append(&op("a1", "note", "x.md", 100, "devA", "v1")).unwrap();
        l.append(&op("a2", "note", "x.md", 200, "devA", "v2")).unwrap();
        // 同 op_id 重放幂等
        l.append(&op("a1", "note", "x.md", 100, "devA", "v1")).unwrap();
        assert_eq!(l.count(), 2);

        let latest = l.latest_for("note", "x.md").unwrap().unwrap();
        assert_eq!(latest.op_id, "a2");
        assert_eq!(latest.value["content"], "v2");
        assert!(l.latest_for("note", "nope.md").unwrap().is_none());
    }

    #[test]
    fn since_and_cursor_monotonic() {
        let l = log("cursor");
        l.append(&op("b1", "note", "1.md", 100, "devB", "a")).unwrap();
        l.append(&op("b2", "note", "2.md", 200, "devB", "b")).unwrap();
        // 自产过滤：device 不符不返回
        assert!(l.ops_of_device("devA", 0, 100).unwrap().is_empty());
        let ops = l.ops_of_device("devB", 100, 100).unwrap();
        assert_eq!(ops.len(), 1); // ts > 100 严格大于
        assert_eq!(ops[0].op_id, "b2");

        assert_eq!(l.cursor("devB"), 0);
        l.set_cursor("devB", 200).unwrap();
        l.set_cursor("devB", 150).unwrap(); // 只进不退
        assert_eq!(l.cursor("devB"), 200);
    }
}
