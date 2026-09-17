//! N4 间隔复习：SM-2 简化版 + cards 表（复用 notes.db 连接）。
//!
//! SM-2（docs/impl/06）：quality(0-5) →
//! `EF' = EF + (0.1 - (5-q)*(0.08+(5-q)*0.02))`，EF 下限 1.3；
//! q<3 重置间隔 1 天（reps 归零）；否则 reps+1，
//! 间隔 = reps==1 → 1 天 / reps==2 → 6 天 / 之后 → round(上轮间隔 × EF')。

use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{NoteError, Result};
use crate::model::Card;

/// 一天毫秒
const DAY_MS: i64 = 86_400_000;

/// SM-2 评分（纯函数，便于单测）
pub fn grade(card: &Card, quality: u32, now_ms: i64) -> Result<Card> {
    if quality > 5 {
        return Err(NoteError::Review(format!("quality 超范围: {quality}（0-5）")));
    }
    let q = f64::from(quality);
    let gap = 5.0 - q;
    let mut ef = card.ef + (0.1 - gap * (0.08 + gap * 0.02));
    if ef < 1.3 {
        ef = 1.3;
    }
    let (reps, interval) = if q < 3.0 {
        (0i64, 1i64)
    } else {
        let reps = card.reps + 1;
        let interval = if reps == 1 {
            1
        } else if reps == 2 {
            6
        } else {
            ((card.interval_days as f64 * ef).round() as i64).max(1)
        };
        (reps, interval)
    };
    Ok(Card {
        ef,
        reps,
        interval_days: interval,
        due_ms: now_ms + interval * DAY_MS,
        ..card.clone()
    })
}

pub struct CardStore {
    conn: Arc<Mutex<Connection>>,
}

impl CardStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Result<Self> {
        {
            let c = conn.lock().map_err(|_| NoteError::Db("卡片连接锁污染".into()))?;
            c.execute_batch(
                "CREATE TABLE IF NOT EXISTS cards (
                    id TEXT PRIMARY KEY,
                    note_path TEXT,
                    front TEXT NOT NULL,
                    back TEXT NOT NULL DEFAULT '',
                    ef REAL NOT NULL DEFAULT 2.5,
                    interval_days INTEGER NOT NULL DEFAULT 0,
                    reps INTEGER NOT NULL DEFAULT 0,
                    due_ms INTEGER NOT NULL
                );",
            )
            .map_err(|e| NoteError::Db(format!("建 cards 表失败: {e}")))?;
        }
        Ok(Self { conn })
    }

    pub fn create(&self, front: &str, back: &str, note_path: Option<String>, now_ms: i64) -> Result<Card> {
        let front = front.trim();
        if front.is_empty() {
            return Err(NoteError::Review("卡片正面不能为空".into()));
        }
        let card = Card {
            id: uuid::Uuid::now_v7().to_string(),
            note_path,
            front: front.to_string(),
            back: back.trim().to_string(),
            ef: 2.5,
            interval_days: 0,
            reps: 0,
            due_ms: now_ms, // 新卡立即进入队列
        };
        let c = self.lock();
        c.execute(
            "INSERT INTO cards (id, note_path, front, back, ef, interval_days, reps, due_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params!(
                card.id,
                card.note_path,
                card.front,
                card.back,
                card.ef,
                card.interval_days,
                card.reps,
                card.due_ms
            ),
        )
        .map_err(db)?;
        Ok(card)
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let c = self.lock();
        let n = c.execute("DELETE FROM cards WHERE id = ?1", params!(id)).map_err(db)?;
        Ok(n > 0)
    }

    pub fn list(&self) -> Result<Vec<Card>> {
        let c = self.lock();
        let mut stmt = c
            .prepare("SELECT id, note_path, front, back, ef, interval_days, reps, due_ms FROM cards ORDER BY due_ms")
            .map_err(db)?;
        rows(stmt.query_map([], map_row))
    }

    /// 今天到期队列（due <= now，按 due 升序）
    pub fn queue(&self, now_ms: i64) -> Result<Vec<Card>> {
        let c = self.lock();
        let mut stmt = c
            .prepare(
                "SELECT id, note_path, front, back, ef, interval_days, reps, due_ms FROM cards
                 WHERE due_ms <= ?1 ORDER BY due_ms",
            )
            .map_err(db)?;
        rows(stmt.query_map(params!(now_ms), map_row))
    }

    pub fn get(&self, id: &str) -> Result<Option<Card>> {
        let c = self.lock();
        c.query_row(
            "SELECT id, note_path, front, back, ef, interval_days, reps, due_ms FROM cards WHERE id = ?1",
            params!(id),
            map_row,
        )
        .optional()
        .map_err(db)
    }

    /// 写回评分后的卡片
    pub fn save(&self, card: &Card) -> Result<()> {
        let c = self.lock();
        c.execute(
            "UPDATE cards SET ef = ?2, interval_days = ?3, reps = ?4, due_ms = ?5 WHERE id = ?1",
            params!(card.id, card.ef, card.interval_days, card.reps, card.due_ms),
        )
        .map_err(db)?;
        Ok(())
    }

    /// 删除卡片时清理对已删笔记的关联
    pub fn detach_note(&self, note_path: &str) -> Result<()> {
        let c = self.lock();
        c.execute(
            "UPDATE cards SET note_path = NULL WHERE note_path = ?1",
            params!(note_path),
        )
        .map_err(db)?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("cards 连接锁污染")
    }
}

fn map_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Card> {
    Ok(Card {
        id: r.get(0)?,
        note_path: r.get(1)?,
        front: r.get(2)?,
        back: r.get(3)?,
        ef: r.get(4)?,
        interval_days: r.get(5)?,
        reps: r.get(6)?,
        due_ms: r.get(7)?,
    })
}

fn rows(
    it: rusqlite::Result<rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<Card>>>,
) -> Result<Vec<Card>> {
    it.map_err(db)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(db)
}

fn db(e: rusqlite::Error) -> NoteError {
    NoteError::Db(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdb(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nf_notes_review_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("notes.db")
    }

    fn card() -> Card {
        Card {
            id: "c1".into(),
            note_path: None,
            front: "F".into(),
            back: "B".into(),
            ef: 2.5,
            interval_days: 0,
            reps: 0,
            due_ms: 0,
        }
    }

    #[test]
    fn sm2_first_grades() {
        let now = 1_000_000_000;
        // q=5：reps1 → 1 天，EF 不变（gap 0）
        let c = grade(&card(), 5, now).unwrap();
        assert_eq!(c.reps, 1);
        assert_eq!(c.interval_days, 1);
        assert!((c.ef - 2.6).abs() < 1e-9);
        // q=4：EF' = 2.5 + 0.1 - 1*(0.08+0.02) = 2.5
        let c = grade(&card(), 4, now).unwrap();
        assert!((c.ef - 2.5).abs() < 1e-9);
        // q=3：EF' = 2.5 + 0.1 - 2*(0.08+0.04) = 2.36
        let c = grade(&card(), 3, now).unwrap();
        assert!((c.ef - 2.36).abs() < 1e-9);
    }

    #[test]
    fn sm2_interval_progression() {
        let now = 0;
        let c1 = grade(&card(), 5, now).unwrap(); // 1 天，EF 2.6
        let c2 = grade(&c1, 5, now).unwrap(); // 6 天，EF 2.7
        assert_eq!(c2.interval_days, 6);
        let c3 = grade(&c2, 5, now).unwrap(); // round(6 * 2.8) = 17 天，EF 2.8
        assert_eq!(c3.interval_days, 17);
        assert!((c3.ef - 2.8).abs() < 1e-9);
        assert_eq!(c3.due_ms, 17 * DAY_MS);
    }

    #[test]
    fn sm2_fail_resets_and_ef_floor() {
        let now = 0;
        let c1 = grade(&card(), 5, now).unwrap();
        assert!(c1.ef > 2.5);
        // 反复低分压到 EF 下限 1.3
        let mut c = card();
        for _ in 0..20 {
            c = grade(&c, 0, now).unwrap();
        }
        assert!((c.ef - 1.3).abs() < 1e-9);
        assert_eq!(c.reps, 0);
        assert_eq!(c.interval_days, 1);
        // q<3 后答对 4：reps1 → 1 天
        let c2 = grade(&c, 4, now).unwrap();
        assert_eq!(c2.reps, 1);
        assert_eq!(c2.interval_days, 1);
    }

    #[test]
    fn quality_out_of_range() {
        assert!(grade(&card(), 6, 0).is_err());
    }

    #[test]
    fn card_store_roundtrip_and_queue() {
        let idx = crate::index::NoteIndex::open(&tmpdb("store")).unwrap();
        let store = CardStore::new(idx.conn()).unwrap();
        let now = 10_000;
        let c1 = store.create(" 正面 ", "背面", Some("a.md".into()), now).unwrap();
        assert_eq!(c1.front, "正面");
        assert_eq!(c1.due_ms, now);
        store.create("卡二", "", None, now + 100).unwrap();
        assert!(store.create("  ", "", None, now).is_err());

        let q = store.queue(now + 50).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].id, c1.id);

        let graded = grade(&q[0], 5, now + 50).unwrap();
        store.save(&graded).unwrap();
        let after = store.get(&c1.id).unwrap().unwrap();
        assert_eq!(after.reps, 1);
        assert_eq!(after.interval_days, 1);
        assert!(store.queue(now + 50).unwrap().is_empty());

        // 关联清理
        store.detach_note("a.md").unwrap();
        assert!(store.get(&c1.id).unwrap().unwrap().note_path.is_none());
        assert!(store.delete(&c1.id).unwrap());
        assert!(store.delete(&c1.id).unwrap() == false);
    }
}
