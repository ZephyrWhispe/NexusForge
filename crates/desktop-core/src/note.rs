//! D4 待办与随记（docs/impl/05 D4）：`{appData}/db/desktop.db`（WAL，DESIGN O3 每模块独立库）。
//!
//! - `#标签` 提取（保留原文）
//! - 提醒解析（中文简化版）：`[今天|明天|后天|周X|周X?]` + `[早上|上午|中午|下午|晚上]? HH[点|:|：][MM分|半]?`，
//!   或正文裸 `HH:MM`（已过则顺延明天）；无时间默认明天 09:00
//! - 提醒调度：模块后台线程 `take_due` 轮询 → 发 `desktop.remind_due` 事件
//!   （Task Scheduler 注册列为后续里程碑）

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{DesktopError, Result};

/// 随记条目
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub content: String,
    pub tags: Vec<String>,
    /// 提醒时间毫秒；None = 无提醒
    pub remind_at: Option<i64>,
    /// 已提醒（防重复发事件）
    pub reminded: bool,
    pub done: bool,
    pub created_ms: i64,
}

pub struct NoteStore {
    conn: Arc<Mutex<Connection>>,
}

impl NoteStore {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(DesktopError::Io)?;
        }
        let conn = Connection::open(db_path)
            .map_err(|e| DesktopError::Db(format!("打开 desktop.db 失败: {e}")))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| DesktopError::Db(format!("设置 WAL 失败: {e}")))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS notes (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]',
                remind_at INTEGER,
                reminded INTEGER NOT NULL DEFAULT 0,
                done INTEGER NOT NULL DEFAULT 0,
                created_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_notes_remind ON notes(reminded, done, remind_at);",
        )
        .map_err(|e| DesktopError::Db(format!("建表失败: {e}")))?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// 新增随记：提取 #标签 + 解析提醒时间
    pub fn add(&self, content: &str, now_ms: i64) -> Result<Note> {
        let content = content.trim();
        if content.is_empty() {
            return Err(DesktopError::BadState("随记内容不能为空".into()));
        }
        let tags = extract_tags(content);
        let remind_at = parse_remind(content, now_ms);
        let note = Note {
            id: uuid::Uuid::now_v7().to_string(),
            content: content.to_string(),
            tags,
            remind_at,
            reminded: false,
            done: false,
            created_ms: now_ms,
        };
        let conn = self.conn.lock().map_err(|_| DesktopError::Db("锁污染".into()))?;
        conn.execute(
            "INSERT INTO notes (id, content, tags, remind_at, reminded, done, created_ms)
             VALUES (?1, ?2, ?3, ?4, 0, 0, ?5)",
            params![
                note.id,
                note.content,
                serde_json::to_string(&note.tags).unwrap_or_else(|_| "[]".into()),
                note.remind_at,
                note.created_ms
            ],
        )
        .map_err(|e| DesktopError::Db(format!("插入随记失败: {e}")))?;
        Ok(note)
    }

    /// 列表（新→旧；include_done=false 排除已完成）
    pub fn list(&self, include_done: bool) -> Result<Vec<Note>> {
        let conn = self.conn.lock().map_err(|_| DesktopError::Db("锁污染".into()))?;
        let mut stmt = conn
            .prepare(if include_done {
                "SELECT id, content, tags, remind_at, reminded, done, created_ms FROM notes ORDER BY created_ms DESC"
            } else {
                "SELECT id, content, tags, remind_at, reminded, done, created_ms FROM notes WHERE done = 0 ORDER BY created_ms DESC"
            })
            .map_err(|e| DesktopError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], row_to_note)
            .map_err(|e| DesktopError::Db(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub fn set_done(&self, id: &str, done: bool) -> Result<bool> {
        let conn = self.conn.lock().map_err(|_| DesktopError::Db("锁污染".into()))?;
        let n = conn
            .execute("UPDATE notes SET done = ?2 WHERE id = ?1", params![id, done])
            .map_err(|e| DesktopError::Db(e.to_string()))?;
        Ok(n > 0)
    }

    pub fn remove(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().map_err(|_| DesktopError::Db("锁污染".into()))?;
        let n = conn
            .execute("DELETE FROM notes WHERE id = ?1", params![id])
            .map_err(|e| DesktopError::Db(e.to_string()))?;
        Ok(n > 0)
    }

    /// 取走全部到期提醒（remind_at <= now 且未提醒未完成），并标记已提醒。
    /// 供后台轮询调用（模块层串行调用，无并发竞争）。
    pub fn take_due(&self, now_ms: i64) -> Result<Vec<Note>> {
        let conn = self.conn.lock().map_err(|_| DesktopError::Db("锁污染".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, content, tags, remind_at, reminded, done, created_ms FROM notes
                 WHERE remind_at IS NOT NULL AND reminded = 0 AND done = 0 AND remind_at <= ?1",
            )
            .map_err(|e| DesktopError::Db(e.to_string()))?;
        let notes: Vec<Note> = stmt
            .query_map(params![now_ms], row_to_note)
            .map_err(|e| DesktopError::Db(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();
        for n in &notes {
            conn.execute(
                "UPDATE notes SET reminded = 1 WHERE id = ?1",
                params![n.id],
            )
            .map_err(|e| DesktopError::Db(e.to_string()))?;
        }
        Ok(notes)
    }

    /// 提醒时间（单条查询，测试/展示用）
    pub fn get(&self, id: &str) -> Result<Option<Note>> {
        let conn = self.conn.lock().map_err(|_| DesktopError::Db("锁污染".into()))?;
        conn.query_row(
            "SELECT id, content, tags, remind_at, reminded, done, created_ms FROM notes WHERE id = ?1",
            params![id],
            row_to_note,
        )
        .optional()
        .map_err(|e| DesktopError::Db(e.to_string()))
    }
}

fn row_to_note(row: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
    let tags_raw: String = row.get(2)?;
    Ok(Note {
        id: row.get(0)?,
        content: row.get(1)?,
        tags: serde_json::from_str(&tags_raw).unwrap_or_default(),
        remind_at: row.get(3)?,
        reminded: row.get::<_, i64>(4)? != 0,
        done: row.get::<_, i64>(5)? != 0,
        created_ms: row.get(6)?,
    })
}

/// 提取 `#标签`（# 起始到空白/行尾；# 后至少 1 字符）
pub fn extract_tags(content: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for token in content.split_whitespace() {
        if let Some(tag) = token.strip_prefix('#') {
            if !tag.is_empty() && !tags.iter().any(|t: &String| t == tag) {
                tags.push(tag.to_string());
            }
        }
    }
    tags
}

/// 本地日始毫秒（系统时区，chrono 探测偏移）
fn day_start(now_ms: i64) -> (i64, i64) {
    let offset = local_utc_offset_secs();
    let local = now_ms / 1000 + offset;
    let days = local.div_euclid(86_400);
    ((days * 86_400 - offset) * 1000, days)
}

/// 本地 UTC 偏移秒
fn local_utc_offset_secs() -> i64 {
    use chrono::Offset;
    chrono::Local::now().offset().fix().local_minus_utc() as i64
}

/// 从随记正文解析提醒时间（毫秒）。返回 None = 未识别。
pub fn parse_remind(content: &str, now_ms: i64) -> Option<i64> {
    let (day_offset, rest) = parse_day(content, now_ms)?;
    let (h, m) = parse_time(rest).unwrap_or((9, 0));
    let (start, _) = day_start(now_ms);
    let mut at = start + day_offset * 86_400_000 + (h as i64 * 3600 + m as i64 * 60) * 1000;
    // 今天 + 时间已过 → 顺延明天
    if at <= now_ms && day_offset == 0 {
        at += 86_400_000;
    }
    Some(at)
}

/// 解析日期部分：今天(0)/明天(1)/后天(2)/周X(1..7)/大后天(3)/大前天(-2)/昨天(-1)
fn parse_day(content: &str, _now_ms: i64) -> Option<(i64, &str)> {
    const WEEK: [(&str, i64); 7] = [
        ("周一", 1), ("周二", 2), ("周三", 3), ("周四", 4),
        ("周五", 5), ("周六", 6), ("周日", 0),
    ];
    // 裸 HH:MM（无日期词）
    if !contains_any(content, &["今天", "明天", "后天", "昨天", "大后天", "周"])
        && parse_time(content).is_some()
    {
        return Some((0, content));
    }

    if let Some(i) = content.find("大后天") {
        return Some((3, &content[i + "大后天".len()..]));
    }
    if let Some(i) = content.find("后天") {
        return Some((2, &content[i + "后天".len()..]));
    }
    if let Some(i) = content.find("明天") {
        return Some((1, &content[i + "明天".len()..]));
    }
    if let Some(i) = content.find("今天") {
        return Some((0, &content[i + "今天".len()..]));
    }
    if let Some(i) = content.find("昨天") {
        return Some((-1, &content[i + "昨天".len()..]));
    }
    // 周X → 距今最近的未来该周几（今天已过时刻则顺延 7 天由上层处理）
    for (name, wd) in WEEK {
        if let Some(i) = content.find(name) {
            return Some((weekday_offset(_now_ms, wd), &content[i + name.len()..]));
        }
    }
    None
}

/// 距 now 的下一个周几（0=周日）；今天即是该周几则 +7（今天语义由「今天」表达）
fn weekday_offset(now_ms: i64, target_wd: i64) -> i64 {
    let days = now_ms.div_euclid(86_400_000);
    // 1970-01-01 是周四(4)
    let today_wd = (days + 4).rem_euclid(7);
    let mut off = (target_wd - today_wd).rem_euclid(7);
    if off == 0 {
        off = 7;
    }
    off
}

fn contains_any(s: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| s.contains(n))
}

/// 解析时间：`HH:MM` / `HH：MM` / `X点半` / `X点XX分` / `X点`；
/// 支持前缀 `早上|上午|中午|下午|晚上`（12 小时制换算：下午/晚上 +12，中午=12:00 默认）
fn parse_time(s: &str) -> Option<(u32, u32)> {
    let s = s.trim_start();
    // 12 小时前缀
    let (pm_offset, s) = if let Some(rest) = s.strip_prefix("凌晨") {
        (0, rest)
    } else if let Some(rest) = s.strip_prefix("早上") {
        (0, rest)
    } else if let Some(rest) = s.strip_prefix("上午") {
        (0, rest)
    } else if let Some(rest) = s.strip_prefix("中午") {
        (12, rest)
    } else if let Some(rest) = s.strip_prefix("下午") {
        (12, rest)
    } else if let Some(rest) = s.strip_prefix("晚上") {
        (12, rest)
    } else {
        (0, s)
    };

    // X点半 / X点XX分 / X点
    if let Some(hpos) = s.find('点') {
        let head = &s[..hpos];
        let h: u32 = head.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse().ok()?;
        if h > 23 {
            return None;
        }
        let rest = &s[hpos + '点'.len_utf8()..];
        if rest.starts_with('半') {
            return Some((normalize_hour(h + pm_offset, pm_offset), 30));
        }
        let mins: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(m) = mins.parse::<u32>() {
            if m < 60 {
                return Some((normalize_hour(h + pm_offset, pm_offset), m));
            }
        }
        return Some((normalize_hour(h + pm_offset, pm_offset), 0));
    }

    // HH:MM / HH：MM（按 char 索引迭代，避免多字节切片 panic）
    let chars: Vec<char> = s.chars().collect();
    for (ci, &c) in chars.iter().enumerate() {
        if c == ':' || c == '：' {
            let h: u32 = chars[..ci].iter().rev().take_while(|d| d.is_ascii_digit()).collect::<String>().chars().rev().collect::<String>().parse().ok()?;
            let m: u32 = chars[ci + 1..].iter().take_while(|d| d.is_ascii_digit()).collect::<String>().parse().ok()?;
            if h <= 23 && m < 60 {
                return Some((h, m));
            }
            return None;
        }
    }
    None
}

/// 12 小时前缀换算：仅当显式前缀存在且结果落在 12h 区间
fn normalize_hour(h: u32, pm_offset: u32) -> u32 {
    let h = h % 24;
    if pm_offset == 12 && h < 12 {
        h + 12
    } else {
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdb(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nf_desktop_note_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("desktop.db")
    }

    // 固定基准：2026-09-17（周四）12:00:00 UTC（local offset 由 chrono 探测，
    // 提醒断言只做相对性/存在性检查避免时区抖动）
    fn now() -> i64 {
        1_789_632_000_000 // 2026-09-17 12:00 UTC
    }

    #[test]
    fn add_extracts_tags_and_persists() {
        let store = NoteStore::open(&tmpdb("tags")).unwrap();
        let n = store.add("买牛奶 #生活 #采购 额外加鸡蛋", now()).unwrap();
        assert_eq!(n.tags, vec!["生活", "采购"]);
        assert_eq!(n.remind_at, None);
        assert!(!n.done);

        let list = store.list(true).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].content, "买牛奶 #生活 #采购 额外加鸡蛋");
        assert_eq!(list[0].tags.len(), 2);
    }

    #[test]
    fn add_rejects_empty() {
        let store = NoteStore::open(&tmpdb("empty")).unwrap();
        assert!(store.add("   ", now()).is_err());
    }

    #[test]
    fn done_and_remove_roundtrip() {
        let store = NoteStore::open(&tmpdb("done")).unwrap();
        let n = store.add("任务A", now()).unwrap();
        let n2 = store.add("任务B", now()).unwrap();

        store.set_done(&n.id, true).unwrap();
        assert_eq!(store.list(false).unwrap().len(), 1, "未完成只剩 B");
        assert_eq!(store.list(true).unwrap().len(), 2);

        assert!(store.remove(&n2.id).unwrap());
        assert!(!store.remove(&n2.id).unwrap(), "重复删除返回 false");
        assert_eq!(store.list(true).unwrap().len(), 1);
    }

    #[test]
    fn parse_remind_basic_days() {
        let n = now();
        // 明天 → 存在且 > now
        let at = parse_remind("明天交报告", n).unwrap();
        assert!(at > n);
        // 明天 9 点
        let at9 = parse_remind("明天早上9点开会", n).unwrap();
        assert!(at9 > n);
        // 显式时间优先于默认
        let a = parse_remind("明天 14:30 交", n).unwrap();
        let b = parse_remind("明天 9:00 交", n).unwrap();
        assert!(a > b, "14:30 应晚于 9:00");
        // 今天 + 已过时间 → 顺延明天
        let today_late = parse_remind("今天 1:00 提醒", n).unwrap();
        assert!(today_late > n, "今天凌晨 1 点已过，应顺延明天");
        // 无日期词裸时间
        let bare = parse_remind("18:00 收拾东西", n).unwrap();
        assert!(bare > n);
    }

    #[test]
    fn parse_remind_half_and_pm() {
        let n = now();
        let a = parse_remind("明天下午3点半 取快递", n).unwrap();
        let b = parse_remind("明天下午3点 取快递", n).unwrap();
        assert_eq!(a - b, 1_800_000, "3点半 = 3点 + 30min");
        let c = parse_remind("明天晚上8点 散步", n).unwrap();
        let d = parse_remind("明天早上8点 散步", n).unwrap();
        assert!(c > d, "晚上8点(20:00) > 早上8点(08:00)");
    }

    #[test]
    fn parse_remind_weekday_is_future() {
        let n = now();
        let at = parse_remind("周一提交周报", n).unwrap();
        assert!(at > n);
        // 至少在 1 天后、7 天内
        let (start, _) = day_start(n);
        let off = (at - start).div_euclid(86_400_000);
        assert!((1..=7).contains(&off), "周偏移应在 1..=7，实际 {off}");
    }

    #[test]
    fn no_remind_words_returns_none() {
        let n = now();
        assert_eq!(parse_remind("随便记一笔", n), None);
        assert_eq!(parse_remind("25:00 非法时间", n), None);
    }

    #[test]
    fn take_due_marks_reminded_once() {
        let store = NoteStore::open(&tmpdb("due")).unwrap();
        let n = now();
        let _ = store.add("明天 9:00 到期任务", n).unwrap();
        // 明天 9:00 的时间点
        let (start, _) = day_start(n);
        let tomorrow9 = start + 86_400_000 + 9 * 3_600_000;

        assert!(store.take_due(n).unwrap().is_empty(), "未到期");
        let due = store.take_due(tomorrow9 + 1000).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].content, "明天 9:00 到期任务");
        assert!(store.take_due(tomorrow9 + 2000).unwrap().is_empty(), "不重复提醒");

        // 完成的不再提醒
        let done_note = store.add("明天 9:00 已完成任务", n).unwrap();
        store.set_done(&done_note.id, true).unwrap();
        assert!(store.take_due(tomorrow9 + 3000).unwrap().is_empty());
    }

    #[test]
    fn get_roundtrip() {
        let store = NoteStore::open(&tmpdb("get")).unwrap();
        let n = store.add("明天 8点半 早会", now()).unwrap();
        let got = store.get(&n.id).unwrap().unwrap();
        assert_eq!(got.content, n.content);
        assert_eq!(got.remind_at, n.remind_at);
        assert!(store.get("no-such").unwrap().is_none());
    }
}
