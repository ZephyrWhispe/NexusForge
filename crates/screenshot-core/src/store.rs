//! 截图历史存取（docs/impl/03 P8 screenshot_history_list）
//!
//! 独立 SQLite 库文件（DESIGN O3：每模块一库）。

use std::path::Path;
use std::sync::Mutex;

use host_core::error::AppError;
use rusqlite::Connection;

use crate::types::{HistoryQuery, Page, ShotItem};

fn err(code: &str, m: impl std::fmt::Display) -> AppError {
    AppError::module(code, m.to_string(), None)
}

/// 连接以 Mutex 包裹：ShotStore 经 Arc 跨线程共享（rusqlite Connection 非 Sync）
pub struct ShotStore {
    conn: Mutex<Connection>,
}

impl ShotStore {
    pub fn open(path: &Path) -> Result<Self, AppError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| err("SCREENSHOT_STORE_001", format!("创建目录失败: {e}")))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| err("SCREENSHOT_STORE_001", format!("打开库失败: {e}")))?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS shots(
                id TEXT PRIMARY KEY,
                created_ms INTEGER NOT NULL,
                width INTEGER NOT NULL,
                height INTEGER NOT NULL,
                file TEXT,
                ocr_text TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_shots_created ON shots(created_ms DESC);",
        )
        .map_err(|e| err("SCREENSHOT_STORE_002", format!("建表失败: {e}")))?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn insert(&self, item: &ShotItem) -> Result<(), AppError> {
        let conn = self.conn.lock().expect("ShotStore 连接锁");
        conn.execute(
                "INSERT INTO shots(id, created_ms, width, height, file, ocr_text)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    item.id,
                    item.created_ms,
                    item.width as i64,
                    item.height as i64,
                    item.file,
                    item.ocr_text
                ],
            )
            .map_err(|e| err("SCREENSHOT_STORE_003", e.to_string()))?;
        Ok(())
    }

    /// 关联 OCR 识别文本（识别在截图完成之后发生时回填）
    pub fn set_ocr_text(&self, id: &str, text: &str) -> Result<(), AppError> {
        let conn = self.conn.lock().expect("ShotStore 连接锁");
        conn.execute(
            "UPDATE shots SET ocr_text = ?2 WHERE id = ?1",
            rusqlite::params![id, text],
        )
        .map_err(|e| err("SCREENSHOT_STORE_003", e.to_string()))?;
        Ok(())
    }

    pub fn list(&self, q: &HistoryQuery) -> Result<Page<ShotItem>, AppError> {
        let conn = self.conn.lock().expect("ShotStore 连接锁");
        let size = q.size.clamp(1, 100);
        let page = q.page.max(1);
        let total: u32 = conn
            .query_row("SELECT COUNT(*) FROM shots", [], |r| r.get::<_, i64>(0))
            .map(|n| n as u32)
            .map_err(|e| err("SCREENSHOT_STORE_004", e.to_string()))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, created_ms, width, height, file, ocr_text
                 FROM shots ORDER BY created_ms DESC LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| err("SCREENSHOT_STORE_004", e.to_string()))?;
        let items = stmt
            .query_map(
                rusqlite::params![size as i64, ((page - 1) * size) as i64],
                |r| {
                    Ok(ShotItem {
                        id: r.get(0)?,
                        created_ms: r.get(1)?,
                        width: r.get::<_, i64>(2)? as u32,
                        height: r.get::<_, i64>(3)? as u32,
                        file: r.get(4)?,
                        ocr_text: r.get(5)?,
                    })
                },
            )
            .map_err(|e| err("SCREENSHOT_STORE_004", e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(Page { items, total, page, size })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> (tempfile::TempDir, ShotStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ShotStore::open(&dir.path().join(format!("{tag}.db"))).unwrap();
        (dir, store)
    }

    fn item(id: &str, ms: i64) -> ShotItem {
        ShotItem {
            id: id.into(),
            created_ms: ms,
            width: 100,
            height: 80,
            file: Some(format!("/x/{id}.png")),
            ocr_text: None,
        }
    }

    #[test]
    fn insert_list_order_and_paging() {
        let (_d, s) = tmp_store("shot_hist");
        for i in 0..5 {
            s.insert(&item(&format!("s{i}"), 1000 + i)).unwrap();
        }
        let page = s.list(&HistoryQuery { page: 1, size: 2 }).unwrap();
        assert_eq!(page.total, 5);
        assert_eq!(page.items.len(), 2);
        // created_ms 降序
        assert_eq!(page.items[0].id, "s4");
        let page2 = s.list(&HistoryQuery { page: 3, size: 2 }).unwrap();
        assert_eq!(page2.items.len(), 1);
        assert_eq!(page2.items[0].id, "s0");
    }

    #[test]
    fn ocr_text_update() {
        let (_d, s) = tmp_store("shot_ocr");
        s.insert(&item("a", 1)).unwrap();
        s.set_ocr_text("a", "识别文本").unwrap();
        let page = s.list(&HistoryQuery { page: 1, size: 10 }).unwrap();
        assert_eq!(page.items[0].ocr_text.as_deref(), Some("识别文本"));
    }
}
