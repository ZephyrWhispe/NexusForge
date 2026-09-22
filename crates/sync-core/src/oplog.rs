//! SYNC2 变更流（docs/impl/07 SYNC2）：op_log 追加表 + 双向设备游标 + 冲突历史表。
//!
//! 游标两维分表：`cursors` = 从该设备**收到**的最大 ts（入站），
//! `push_cursors` = 本机自产变更**推给**该设备的最大 ts（出站，发起方按此分批续传）。
//!
//! `conflict_log` = LWW 判负一侧的内容快照（T-B5-2：败方值曾经只存在于一次事件里，
//! 事件即焚 ⇒ 冲突事后无从查、无从回滚；主键是确定性 id，故同一败方快照重放只得一行）。
//!
//! op_log = 实体快照式变更记录（v1 实体级 LWW，字段级合并随 SYNC3 深化）：
//! `{ op_id(ULID→uuid v7), entity, entity_id, ts, device, value }`
//! value = 实体 JSON 快照（notes: {content, title}；删除 = {"deleted": true}）。
//! 同步 = 交换游标 → 拉取缺失 → 本地应用（LWW，见 engine.rs）。

use parking_lot::Mutex;
use std::path::Path;
use std::sync::Arc;

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
        self.value
            .get(DELETED_KEY)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
}

/// 冲突行（LWW 败方快照）：`lost_*` = 本次判负、本地未采用的远端条目；
/// `winner_*` = 当时压住它的本地条目（审计时看得出"谁把谁比下去了"）。
///
/// 敏感度：`lost_value` 就是笔记内容明文，与笔记库本体同级（笔记本就是明文文件），
/// 落 `sync.db` 不新增泄漏面；但本表**不进同步流**——它不是 entity，
/// 数据集白名单（T-B5-5 `SYNC_ENTITIES`）天然排除它。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictEntry {
    pub conflict_id: String,
    pub entity: String,
    pub entity_id: String,
    /// 败方条目的时间戳（= 该 op 的 ts）
    pub lost_ts: i64,
    pub lost_device: String,
    pub winner_device: String,
    pub winner_ts: i64,
    /// 败方内容快照（删除标记同样如实存 `{"deleted":true}`）
    pub lost_value: serde_json::Value,
    /// 落盘时刻（视图排序 + 保留窗裁剪依据，与 ts 两轴不可混用）
    pub recorded_ms: i64,
}

impl ConflictEntry {
    pub fn is_delete(&self) -> bool {
        self.lost_value
            .get(DELETED_KEY)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
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
CREATE TABLE IF NOT EXISTS push_cursors (
    device  TEXT PRIMARY KEY,
    last_ts INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS conflict_log (
    conflict_id   TEXT PRIMARY KEY,
    entity        TEXT NOT NULL,
    entity_id     TEXT NOT NULL,
    lost_ts       INTEGER NOT NULL,
    lost_device   TEXT NOT NULL,
    winner_device TEXT NOT NULL,
    winner_ts     INTEGER NOT NULL,
    lost_value    TEXT NOT NULL,
    recorded_ms   INTEGER NOT NULL DEFAULT 0
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
        conn.execute_batch(SCHEMA)
            .map_err(|e| SyncError::Db(e.to_string()))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 追加变更（op_id 主键 → INSERT OR IGNORE 幂等；重复推送无副作用）
    pub fn append(&self, e: &OpEntry) -> Result<()> {
        let conn = self.conn.lock();
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
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT op_id, entity, entity_id, ts, device, value FROM op_log
                 WHERE device = ?1 AND ts > ?2 ORDER BY ts LIMIT ?3",
            )
            .map_err(|e| SyncError::Db(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params![device, since_ts, limit as i64], row_to_op)
            .map_err(|e| SyncError::Db(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| SyncError::Db(e.to_string()))
    }

    /// 实体当前最新变更（LWW 对比用）
    pub fn latest_for(&self, entity: &str, entity_id: &str) -> Result<Option<OpEntry>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT op_id, entity, entity_id, ts, device, value FROM op_log
                 WHERE entity = ?1 AND entity_id = ?2 ORDER BY ts DESC, device DESC LIMIT 1",
            )
            .map_err(|e| SyncError::Db(e.to_string()))?;
        let mut rows = stmt
            .query_map(rusqlite::params![entity, entity_id], row_to_op)
            .map_err(|e| SyncError::Db(e.to_string()))?;
        rows.next()
            .transpose()
            .map_err(|e| SyncError::Db(e.to_string()))
    }

    /// 设备游标（从该设备已收到的最大 ts）
    pub fn cursor(&self, device: &str) -> i64 {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT last_ts FROM cursors WHERE device = ?1",
            rusqlite::params![device],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    /// 推进游标（只进不退）
    pub fn set_cursor(&self, device: &str, ts: i64) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO cursors (device, last_ts) VALUES (?1, ?2)
             ON CONFLICT(device) DO UPDATE SET last_ts = MAX(last_ts, ?2)",
            rusqlite::params![device, ts],
        )
        .map_err(|e| SyncError::Db(e.to_string()))?;
        Ok(())
    }

    /// 出站游标：本机自产变更已推给该设备的最大 ts（缺行 = 0，与 cursor 同形）
    ///
    /// 与入站 `cursors` 分表：两方向单调性互斥，混表会让一次重放把"我推到哪"污染成"对端推到哪"。
    pub fn push_cursor(&self, device: &str) -> i64 {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT last_ts FROM push_cursors WHERE device = ?1",
            rusqlite::params![device],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    /// 推进出站游标（只进不退，与入站同一纪律）
    pub fn set_push_cursor(&self, device: &str, ts: i64) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO push_cursors (device, last_ts) VALUES (?1, ?2)
             ON CONFLICT(device) DO UPDATE SET last_ts = MAX(last_ts, excluded.last_ts)",
            rusqlite::params![device, ts],
        )
        .map_err(|e| SyncError::Db(e.to_string()))?;
        Ok(())
    }

    /// op 总数（状态面板）
    pub fn count(&self) -> u64 {
        let conn = self.conn.lock();
        conn.query_row("SELECT COUNT(*) FROM op_log", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u64
    }

    /// 记一行冲突历史（`INSERT OR IGNORE`：conflict_id 确定性 ⇒ 同一败方快照重放只得一行）
    pub fn record_conflict(&self, e: &ConflictEntry) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR IGNORE INTO conflict_log
             (conflict_id, entity, entity_id, lost_ts, lost_device, winner_device, winner_ts, lost_value, recorded_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                e.conflict_id,
                e.entity,
                e.entity_id,
                e.lost_ts,
                e.lost_device,
                e.winner_device,
                e.winner_ts,
                serde_json::to_string(&e.lost_value).map_err(|er| SyncError::Db(er.to_string()))?,
                e.recorded_ms
            ],
        )
        .map_err(|e| SyncError::Db(e.to_string()))?;
        Ok(())
    }

    /// 冲突历史分页（新落盘的在前：recorded_ms 是"我何时知道"，比 lost_ts 更贴视图语义）
    pub fn conflicts(&self, limit: i64, offset: i64) -> Result<Vec<ConflictEntry>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM conflict_log ORDER BY recorded_ms DESC, conflict_id LIMIT ?1 OFFSET ?2",
                CONFLICT_COLS
            ))
            .map_err(|e| SyncError::Db(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params![limit, offset], row_to_conflict)
            .map_err(|e| SyncError::Db(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| SyncError::Db(e.to_string()))
    }

    /// 按 id 取单行（回滚入口读快照；无此行返回 None，由调用方如实报错）
    pub fn find_conflict(&self, conflict_id: &str) -> Result<Option<ConflictEntry>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM conflict_log WHERE conflict_id = ?1",
                CONFLICT_COLS
            ))
            .map_err(|e| SyncError::Db(e.to_string()))?;
        let mut rows = stmt
            .query_map(rusqlite::params![conflict_id], row_to_conflict)
            .map_err(|e| SyncError::Db(e.to_string()))?;
        rows.next()
            .transpose()
            .map_err(|e| SyncError::Db(e.to_string()))
    }

    /// 冲突历史保留窗裁剪（`recorded_ms < cutoff_ms` 者删；返回删除行数）
    pub fn prune_conflicts_before(&self, cutoff_ms: i64) -> Result<u64> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM conflict_log WHERE recorded_ms < ?1",
            rusqlite::params![cutoff_ms],
        )
        .map(|n| n as u64)
        .map_err(|e| SyncError::Db(e.to_string()))
    }
}

const CONFLICT_COLS: &str = "conflict_id, entity, entity_id, lost_ts, lost_device, winner_device, winner_ts, lost_value, recorded_ms";

fn row_to_conflict(r: &rusqlite::Row<'_>) -> rusqlite::Result<ConflictEntry> {
    let value_raw: String = r.get(7)?;
    Ok(ConflictEntry {
        conflict_id: r.get(0)?,
        entity: r.get(1)?,
        entity_id: r.get(2)?,
        lost_ts: r.get(3)?,
        lost_device: r.get(4)?,
        winner_device: r.get(5)?,
        winner_ts: r.get(6)?,
        lost_value: serde_json::from_str(&value_raw).unwrap_or(serde_json::Value::Null),
        recorded_ms: r.get(8)?,
    })
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
        OpLog::open(
            &std::env::temp_dir().join(format!("nf_sync_{tag}_{}.db", uuid::Uuid::now_v7())),
        )
        .unwrap()
    }

    #[test]
    fn append_idempotent_and_latest() {
        let l = log("append");
        l.append(&op("a1", "note", "x.md", 100, "devA", "v1"))
            .unwrap();
        l.append(&op("a2", "note", "x.md", 200, "devA", "v2"))
            .unwrap();
        // 同 op_id 重放幂等
        l.append(&op("a1", "note", "x.md", 100, "devA", "v1"))
            .unwrap();
        assert_eq!(l.count(), 2);

        let latest = l.latest_for("note", "x.md").unwrap().unwrap();
        assert_eq!(latest.op_id, "a2");
        assert_eq!(latest.value["content"], "v2");
        assert!(l.latest_for("note", "nope.md").unwrap().is_none());
    }

    #[test]
    fn since_and_cursor_monotonic() {
        let l = log("cursor");
        l.append(&op("b1", "note", "1.md", 100, "devB", "a"))
            .unwrap();
        l.append(&op("b2", "note", "2.md", 200, "devB", "b"))
            .unwrap();
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

    /// 任务书（09 §10.2 T-B5-1）字面测试名优先于 rustc 命名惯例
    #[test]
    #[allow(non_snake_case)]
    fn pushCursor_staleAck_neverRewinds() {
        let l = log("pushcursor");
        assert_eq!(l.push_cursor("devC"), 0, "缺行即 0（旧库零迁移）");
        l.set_push_cursor("devC", 500).unwrap();
        l.set_push_cursor("devC", 120).unwrap(); // 对端回了个旧 ts 回执
        assert_eq!(l.push_cursor("devC"), 500, "出站游标与入站同一只进不退纪律");
        l.set_push_cursor("devC", 640).unwrap();
        assert_eq!(l.push_cursor("devC"), 640);
    }

    fn conflict(id: &str, eid: &str, content: &str, recorded_ms: i64) -> ConflictEntry {
        ConflictEntry {
            conflict_id: id.into(),
            entity: "note".into(),
            entity_id: eid.into(),
            lost_ts: recorded_ms + 1,
            lost_device: "devB".into(),
            winner_device: "devA".into(),
            winner_ts: recorded_ms + 2,
            lost_value: json!({ "content": content }),
            recorded_ms,
        }
    }

    /// 任务书（09 §10.2 T-B5-2）字面测试名优先于 rustc 命名惯例
    #[test]
    #[allow(non_snake_case)]
    fn conflictLog_pruneKeepsUnexpiredRows() {
        let path = std::env::temp_dir().join(format!("nf_sync_prune_{}.db", uuid::Uuid::now_v7()));
        {
            let l = OpLog::open(&path).unwrap();
            l.record_conflict(&conflict("c1", "old.md", "过期", 1_000))
                .unwrap();
            l.record_conflict(&conflict("c2", "new.md", "在窗内", 9_000))
                .unwrap();
            // cutoff=5_000：recorded_ms 严格小于者出窗
            assert_eq!(l.prune_conflicts_before(5_000).unwrap(), 1);
            let left = l.conflicts(10, 0).unwrap();
            assert_eq!(left.len(), 1);
            assert_eq!(left[0].conflict_id, "c2");
        }
        let _ = std::fs::remove_file(&path);
    }

    /// 任务书（09 §10.2 T-B5-2）字面测试名优先于 rustc 命名惯例
    ///
    /// "阅后即焚"时代的终结证据：关连接重开，败方原文仍可查。
    #[test]
    #[allow(non_snake_case)]
    fn conflictLog_survivesReopen() {
        let path = std::env::temp_dir().join(format!("nf_sync_reopen_{}.db", uuid::Uuid::now_v7()));
        OpLog::open(&path)
            .unwrap()
            .record_conflict(&conflict("c9", "keep.md", "这段字只在这里活下来", 42))
            .unwrap();
        let l = OpLog::open(&path).unwrap();
        let got = l
            .find_conflict("c9")
            .unwrap()
            .expect("重开库后冲突行必须仍在");
        assert_eq!(got.entity_id, "keep.md");
        assert_eq!(got.lost_value["content"], "这段字只在这里活下来");
        assert_eq!(got.winner_device, "devA");
        assert!(l.find_conflict("nope").unwrap().is_none());
        let _ = std::fs::remove_file(&path);
    }
}
