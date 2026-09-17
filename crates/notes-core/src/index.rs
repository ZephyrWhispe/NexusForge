//! N1/N2 索引层：notes.db（WAL，DESIGN O3 每模块独立库）。
//!
//! 真相源 = 磁盘 .md 文件；本库只是**可全量重建的索引**（notes/tags/links 三表）。
//! path 统一为 `/` 分隔的库内相对路径（跨平台 JOIN key 稳定）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};

use crate::error::{NoteError, Result};
use crate::model::{Backlink, NoteMeta};

/// 索引记录（upsert 入参）：基础字段 + 标签 + 双链
pub struct NoteIndexRow {
    pub path: String,
    pub title: String,
    pub mtime_ms: i64,
    pub size: u64,
    pub tags: Vec<String>,
    /// (dst 原文, 解析后的 dst_path；未解析为 "")
    pub links: Vec<(String, String)>,
}

pub struct NoteIndex {
    conn: Arc<Mutex<Connection>>,
}

impl NoteIndex {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(NoteError::Io)?;
        }
        let conn = Connection::open(db_path)
            .map_err(|e| NoteError::Db(format!("打开 notes.db 失败: {e}")))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| NoteError::Db(format!("设置 WAL 失败: {e}")))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS notes (
                path TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                mtime_ms INTEGER NOT NULL,
                size INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS tags (
                note_path TEXT NOT NULL,
                tag TEXT NOT NULL,
                PRIMARY KEY (note_path, tag)
            );
            CREATE TABLE IF NOT EXISTS links (
                src TEXT NOT NULL,
                dst TEXT NOT NULL,
                dst_path TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (src, dst)
            );
            CREATE INDEX IF NOT EXISTS idx_links_dst ON links(dst_path);",
        )
        .map_err(|e| NoteError::Db(format!("建表失败: {e}")))?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// 卡片存储复用同一连接（N4）
    pub fn conn(&self) -> Arc<Mutex<Connection>> {
        self.conn.clone()
    }

    /// upsert 单篇笔记的索引行（事务）
    pub fn upsert(&self, row: NoteIndexRow) -> Result<()> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction().map_err(db)?;
        tx.execute(
            "INSERT INTO notes (path, title, mtime_ms, size) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET title=?2, mtime_ms=?3, size=?4",
            params!(row.path, row.title, row.mtime_ms, row.size as i64),
        )
        .map_err(db)?;
        tx.execute("DELETE FROM tags WHERE note_path = ?1", params!(row.path)).map_err(db)?;
        for tag in &row.tags {
            tx.execute(
                "INSERT OR IGNORE INTO tags (note_path, tag) VALUES (?1, ?2)",
                params!(row.path, tag),
            )
            .map_err(db)?;
        }
        tx.execute("DELETE FROM links WHERE src = ?1", params!(row.path)).map_err(db)?;
        for (dst, dst_path) in &row.links {
            tx.execute(
                "INSERT OR REPLACE INTO links (src, dst, dst_path) VALUES (?1, ?2, ?3)",
                params!(row.path, dst, dst_path),
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(())
    }

    /// 删除笔记索引（级联 tags/links）
    pub fn remove(&self, path: &str) -> Result<()> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction().map_err(db)?;
        tx.execute("DELETE FROM notes WHERE path = ?1", params!(path)).map_err(db)?;
        tx.execute("DELETE FROM tags WHERE note_path = ?1", params!(path)).map_err(db)?;
        tx.execute("DELETE FROM links WHERE src = ?1", params!(path)).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(())
    }

    /// 重命名索引路径（notes/tags/links.src 同步；links.dst_path 由调用方重索引后自然更新）
    pub fn rename_path(&self, old: &str, new: &str) -> Result<()> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction().map_err(db)?;
        tx.execute("UPDATE notes SET path = ?2 WHERE path = ?1", params!(old, new)).map_err(db)?;
        tx.execute("UPDATE tags SET note_path = ?2 WHERE note_path = ?1", params!(old, new))
            .map_err(db)?;
        tx.execute("UPDATE links SET src = ?2 WHERE src = ?1", params!(old, new)).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(())
    }

    /// 全量列表（tags 两步查询内存合并，避免 group_concat 解析）
    pub fn list(&self) -> Result<Vec<NoteMeta>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT path, title, mtime_ms, size FROM notes ORDER BY path").map_err(db)?;
        let rows: Vec<(String, String, i64, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(db)?
            .collect::<std::result::Result<_, _>>()
            .map_err(db)?;
        let mut stmt2 = conn.prepare("SELECT note_path, tag FROM tags").map_err(db)?;
        let tag_rows: Vec<(String, String)> = stmt2
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db)?
            .collect::<std::result::Result<_, _>>()
            .map_err(db)?;
        drop(stmt);
        drop(stmt2);
        let mut tags_map: HashMap<String, Vec<String>> = HashMap::new();
        for (p, t) in tag_rows {
            tags_map.entry(p).or_default().push(t);
        }
        Ok(rows
            .into_iter()
            .map(|(path, title, mtime_ms, size)| NoteMeta {
                tags: tags_map.remove(&path).unwrap_or_default(),
                path,
                title,
                mtime_ms,
                size: size.max(0) as u64,
            })
            .collect())
    }

    pub fn get(&self, path: &str) -> Result<Option<NoteMeta>> {
        Ok(self.list()?.into_iter().find(|n| n.path == path))
    }

    pub fn paths(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT path FROM notes").map_err(db)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    /// 反链来源（N2）：dst_path 命中即返回 (src, 链接原文)；snippet 由 library 层读文件补
    pub fn links_to(&self, dst_path: &str) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT src, dst FROM links WHERE dst_path = ?1 ORDER BY src").map_err(db)?;
        let rows = stmt
            .query_map(params!(dst_path), |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    /// 本文出链（N2 面板展示）
    pub fn links_from(&self, src: &str) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT dst, dst_path FROM links WHERE src = ?1 ORDER BY dst").map_err(db)?;
        let rows = stmt
            .query_map(params!(src), |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    /// 反链组装（含 title；读文件补 snippet 由 library 层做）
    pub fn backlink_bases(&self, path: &str) -> Result<Vec<Backlink>> {
        Ok(self
            .links_to(path)?
            .into_iter()
            .map(|(src, _)| Backlink { src, title: String::new(), snippet: String::new() })
            .collect())
    }

    /// 全量清空索引三表（reindex 前置；cards 为用户数据不动）
    pub fn clear(&self) -> Result<()> {
        let conn = self.lock();
        conn.execute_batch("DELETE FROM notes; DELETE FROM tags; DELETE FROM links;")
            .map_err(db)?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("notes.db 连接锁污染")
    }
}

fn db(e: rusqlite::Error) -> NoteError {
    NoteError::Db(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdb(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nf_notes_idx_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("notes.db")
    }

    fn row(path: &str, links: Vec<(&str, &str)>) -> NoteIndexRow {
        NoteIndexRow {
            path: path.into(),
            title: "t".into(),
            mtime_ms: 1,
            size: 2,
            tags: vec!["a".into()],
            links: links.into_iter().map(|(d, p)| (d.into(), p.into())).collect(),
        }
    }

    #[test]
    fn upsert_list_remove_roundtrip() {
        let idx = NoteIndex::open(&tmpdb("round")).unwrap();
        idx.upsert(row("a.md", vec![("b", "b.md"), ("x", "")])).unwrap();
        idx.upsert(row("b.md", vec![])).unwrap();
        let list = idx.list().unwrap();
        assert_eq!(list.len(), 2);
        let a = list.iter().find(|n| n.path == "a.md").unwrap();
        assert_eq!(a.tags, vec!["a"]);
        assert_eq!(idx.links_to("b.md").unwrap(), vec![("a.md".into(), "b".into())]);
        assert_eq!(idx.links_from("a.md").unwrap().len(), 2);
        idx.remove("a.md").unwrap();
        assert!(idx.links_to("b.md").unwrap().is_empty());
    }

    #[test]
    fn rename_path_updates_all_tables() {
        let idx = NoteIndex::open(&tmpdb("rename")).unwrap();
        idx.upsert(row("sub/a.md", vec![])).unwrap();
        idx.upsert(row("b.md", vec![("a", "sub/a.md")])).unwrap();
        idx.rename_path("sub/a.md", "sub/c.md").unwrap();
        let paths = idx.paths().unwrap();
        assert!(paths.contains(&"sub/c.md".to_string()));
        // src 为 sub/a.md 的 links 已迁移
        assert_eq!(idx.links_from("sub/c.md").unwrap().len(), 0);
        assert_eq!(idx.links_to("sub/a.md").unwrap().len(), 1); // b.md 的 dst_path 未重索引仍指旧
    }
}
