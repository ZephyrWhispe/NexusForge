//! SYNC2 变更流（docs/impl/07 SYNC2）：op_log 追加表 + 双向设备游标 + 冲突历史表 + 同步流水表。
//!
//! 游标两维分表：`cursors` = 从该设备**收到**的最大 ts（入站），
//! `push_cursors` = 本机自产变更**推给**该设备的最大 ts（出站，发起方按此分批续传）。
//!
//! `conflict_log` = LWW 判负一侧的内容快照（T-B5-2：败方值曾经只存在于一次事件里，
//! 事件即焚 ⇒ 冲突事后无从查、无从回滚；主键是确定性 id，故同一败方快照重放只得一行）。
//!
//! `sync_run` = 每轮同步会话的结果流水（T-B5-3：会话摘要曾经只进 tracing 日志，
//! 阅后即焚 ⇒ 面板说不清"上次到底同步了没"；失败同样入表，`error` 列非空即失败行）。
//!
//! op_log = 实体快照式变更记录（v1 实体级 LWW，字段级合并随 SYNC3 深化）：
//! `{ op_id(ULID→uuid v7), entity, entity_id, ts, device, value }`
//! value = 实体 JSON 快照（notes: {content, title}；删除 = {"deleted": true}）。
//! 同步 = 交换游标 → 拉取缺失 → 本地应用（LWW，见 engine.rs）。

use parking_lot::Mutex;
use std::collections::HashMap;
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

/// `sync_run.role` 取值域：主动发起一轮 = initiator，被动应答一次会话 = responder。
///
/// 承重④的对称面：被动侧的结果过去只写 tracing 日志，本机面板问不出"刚才谁来过"。
pub const ROLE_INITIATOR: &str = "initiator";
pub const ROLE_RESPONDER: &str = "responder";

/// 一轮同步会话的流水行（`id` 由 SQLite 自增分配，写入侧留 0 即可）。
///
/// 记账入口只有一个（`module.rs::finish_run`），**失败也记**：红线"失败不静默"——
/// 失败与成功同表同形，面板才不会把"没同步成"渲染成"没什么要同步"。
///
/// - `ts_ms` = 会话**开始**时刻（用户读作"这一轮发生在"），`duration_ms` = 结束−开始；
/// - 四个计数是失败前已完成的部分（半途断线时"推出去了 300 条"是真事实，不抹成 NULL）；
/// - `peer` = 握手成功后是对端 device_id；握手前失败只能如实记 socket 地址
///   （无从得知身份，既不编造 id 也不塞占位串）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRun {
    pub id: i64,
    pub ts_ms: i64,
    pub peer: String,
    pub role: String,
    pub pushed: u32,
    pub pulled_applied: u32,
    pub pulled_lost: u32,
    pub conflicts: u32,
    pub duration_ms: i64,
    pub error: Option<String>,
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
CREATE TABLE IF NOT EXISTS sync_run (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms          INTEGER NOT NULL,
    peer           TEXT NOT NULL,
    role           TEXT NOT NULL,
    pushed         INTEGER,
    pulled_applied INTEGER,
    pulled_lost    INTEGER,
    conflicts      INTEGER,
    duration_ms    INTEGER,
    error          TEXT
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

    /// 该设备产出且 `ts` 严格大于给定值的条目数
    ///
    /// **pending 的唯一算式**（承重③）：`count_ops_after(self_device, push_cursor(peer))`
    /// = "本机已产生、但尚未推给该对端"。它与"对端落后我"是同一事实的两半——出站游标记
    /// "我推到哪"，本查询数"那之后还有多少条"。op 的 ts 非连续、且同一毫秒可有多条，
    /// 所以 pending 必须真查一次 COUNT，不能拿两个游标相减凑。
    pub fn count_ops_after(&self, device: &str, ts: i64) -> Result<u64> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM op_log WHERE device = ?1 AND ts > ?2",
            rusqlite::params![device, ts],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n as u64)
        .map_err(|e| SyncError::Db(e.to_string()))
    }

    /// 全部入站游标（device 升序稳定；"我从每台收到过哪一版"）
    pub fn all_cursors(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT device, last_ts FROM cursors ORDER BY device")
            .map_err(|e| SyncError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| SyncError::Db(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| SyncError::Db(e.to_string()))
    }

    /// 全部出站游标（device 升序稳定；"我给每台推到哪一版"）
    ///
    /// 与 `all_cursors` 分表读：两维同名设备互不污染是 T-B5-1 分表裁定的红线（承重②）。
    pub fn all_push_cursors(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT device, last_ts FROM push_cursors ORDER BY device")
            .map_err(|e| SyncError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| SyncError::Db(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| SyncError::Db(e.to_string()))
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

    /// 记一行同步流水（无 UNIQUE 约束：一次尝试就该留一次痕迹，
    /// 与 `conflict_log` 的确定性主键刻意分野——重放同步不是"同一事实被再次观测"）
    pub fn record_run(&self, r: &SyncRun) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO sync_run
             (ts_ms, peer, role, pushed, pulled_applied, pulled_lost, conflicts, duration_ms, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                r.ts_ms,
                r.peer,
                r.role,
                r.pushed,
                r.pulled_applied,
                r.pulled_lost,
                r.conflicts,
                r.duration_ms,
                r.error
            ],
        )
        .map_err(|e| SyncError::Db(e.to_string()))?;
        Ok(())
    }

    /// 流水分页（新行在前；`id` 自增 ⇒ 按 id 降序就是按发生顺序倒放，不受时钟回跳影响）
    pub fn runs(&self, limit: i64) -> Result<Vec<SyncRun>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM sync_run ORDER BY id DESC LIMIT ?1",
                RUN_COLS
            ))
            .map_err(|e| SyncError::Db(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params![limit], row_to_run)
            .map_err(|e| SyncError::Db(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| SyncError::Db(e.to_string()))
    }

    /// 每个对端最近一次会话（T-B5-4 的 `last_sync_ms`/`last_error` 单一事实源）
    ///
    /// 一次 GROUP BY 取每 peer 的最大 id：结果行数由对端数封顶，不随流水表长度增长。
    pub fn last_run_per_peer(&self) -> Result<HashMap<String, SyncRun>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM sync_run WHERE id IN (SELECT MAX(id) FROM sync_run GROUP BY peer)",
                RUN_COLS
            ))
            .map_err(|e| SyncError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], row_to_run)
            .map_err(|e| SyncError::Db(e.to_string()))?;
        let mut out = HashMap::new();
        for r in rows {
            let r = r.map_err(|e| SyncError::Db(e.to_string()))?;
            out.insert(r.peer.clone(), r);
        }
        Ok(out)
    }
}

const CONFLICT_COLS: &str = "conflict_id, entity, entity_id, lost_ts, lost_device, winner_device, winner_ts, lost_value, recorded_ms";

const RUN_COLS: &str =
    "id, ts_ms, peer, role, pushed, pulled_applied, pulled_lost, conflicts, duration_ms, error";

fn row_to_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<SyncRun> {
    Ok(SyncRun {
        id: r.get(0)?,
        ts_ms: r.get(1)?,
        peer: r.get(2)?,
        role: r.get(3)?,
        pushed: r.get(4)?,
        pulled_applied: r.get(5)?,
        pulled_lost: r.get(6)?,
        conflicts: r.get(7)?,
        duration_ms: r.get(8)?,
        error: r.get(9)?,
    })
}

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

    /// 流水行样本：`n` 同时充当 pushed 与 ts 的序标记，便于断言"新行在前"取到哪一行
    fn run(peer: &str, role: &str, n: i64, error: Option<&str>) -> SyncRun {
        SyncRun {
            id: 0,
            ts_ms: 1_000 + n,
            peer: peer.into(),
            role: role.into(),
            pushed: n as u32,
            pulled_applied: 0,
            pulled_lost: 0,
            conflicts: 0,
            duration_ms: 7,
            error: error.map(String::from),
        }
    }

    /// 任务书（09 §10.2 T-B5-3）字面测试名优先于 rustc 命名惯例
    #[test]
    #[allow(non_snake_case)]
    fn allCursors_twoDimensionsIndependent() {
        let l = log("cursors2d");
        l.set_cursor("devD", 300).unwrap();
        l.set_push_cursor("devD", 900).unwrap();
        // 同一台设备两维各自成行，互不污染（T-B5-1 分表裁定的红线钉）
        assert_eq!(l.all_cursors().unwrap(), vec![("devD".to_string(), 300)]);
        assert_eq!(
            l.all_push_cursors().unwrap(),
            vec![("devD".to_string(), 900)],
            "出站游标读数不得被入站写入污染，反之亦然"
        );
        l.set_push_cursor("devA", 10).unwrap();
        assert_eq!(l.all_push_cursors().unwrap().len(), 2);
        assert_eq!(
            l.all_cursors().unwrap().len(),
            1,
            "只写出站维 ⇒ 入站表不跟着长行"
        );
        assert_eq!(
            l.all_push_cursors().unwrap()[0].0,
            "devA",
            "读侧按 device 升序稳定（面板逐项对齐用）"
        );

        // pending 算式与游标两维各自独立：只按 device + ts 真查
        l.append(&op("z1", "note", "1.md", 100, "devD", "x"))
            .unwrap();
        l.append(&op("z2", "note", "2.md", 400, "devD", "x"))
            .unwrap();
        assert_eq!(l.count_ops_after("devD", 300).unwrap(), 1, "ts 严格大于");
        assert_eq!(l.count_ops_after("devD", 0).unwrap(), 2);
        assert_eq!(l.count_ops_after("devZ", 0).unwrap(), 0, "别的设备一条不算");
    }

    /// 任务书（09 §10.2 T-B5-3）字面测试名优先于 rustc 命名惯例
    #[test]
    #[allow(non_snake_case)]
    fn runs_boundedLimit_honoursRequest() {
        let l = log("runslimit");
        for n in 0..5i64 {
            l.record_run(&run("devPeer", ROLE_INITIATOR, n, None))
                .unwrap();
        }
        let all = l.runs(10).unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].pushed, 4, "新行在前");
        assert!(all[0].id > all[1].id, "按自增 id 倒放，不受时钟回跳影响");

        let two = l.runs(2).unwrap();
        assert_eq!(
            two.iter().map(|r| r.pushed).collect::<Vec<_>>(),
            vec![4, 3],
            "limit 是硬上限：表可无限长，视图不跟着无界"
        );
        assert!(l.runs(0).unwrap().is_empty());

        // 每 peer 最近一次：同 peer 取最新，两 peer 互不串，失败行照实带 error
        let per = l.last_run_per_peer().unwrap();
        assert_eq!(per.len(), 1);
        assert_eq!(per["devPeer"].pushed, 4);
        assert_eq!(per["devPeer"].error, None);
        l.record_run(&run("127.0.0.1:5599", ROLE_RESPONDER, 1, Some("连接超时")))
            .unwrap();
        let per = l.last_run_per_peer().unwrap();
        assert_eq!(per.len(), 2);
        assert_eq!(per["127.0.0.1:5599"].role, ROLE_RESPONDER);
        assert_eq!(per["127.0.0.1:5599"].error.as_deref(), Some("连接超时"));
        assert_eq!(per["devPeer"].pushed, 4, "另一台设备的行不影响本机读数");
    }
}
