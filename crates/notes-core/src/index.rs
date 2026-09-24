//! N1/N2 索引层：notes.db（WAL，DESIGN O3 每模块独立库）。
//!
//! 真相源 = 磁盘 .md 文件；本库只是**可全量重建的索引**（notes/tags/links/notes_fts 四表）。
//! path 统一为 `/` 分隔的库内相对路径（跨平台 JOIN key 稳定）。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{NoteError, Result};
use crate::model::{Backlink, NoteMeta, SearchHit};

/// 索引记录（upsert 入参）：基础字段 + 标签 + 双链 + 正文（FTS5 用）
pub struct NoteIndexRow {
    pub path: String,
    pub title: String,
    pub mtime_ms: i64,
    pub size: u64,
    pub tags: Vec<String>,
    /// (dst 原文, 解析后的 dst_path；未解析为 "")
    pub links: Vec<(String, String)>,
    /// 去 frontmatter 后的正文（T-B7-21 全文索引列）
    pub body: String,
}

// ---------- T-B7-21：CJK 感知分词（索引与查询共用同一函数，单口防漂移） ----------

fn is_cjk(ch: char) -> bool {
    matches!(u32::from(ch),
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
        | 0x20000..=0x2A6DF | 0x2A700..=0x2EBEF | 0x30000..=0x3134F)
}

/// 在任一相邻对至少含一个 CJK 字符处插入单空格，令 unicode61 把 CJK 切成单字 token。
/// 已是空格则不重复插入（幂等）。
pub fn cjk_space(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 2);
    let mut prev_cjk = false;
    for ch in text.chars() {
        let cur_cjk = is_cjk(ch);
        if (cur_cjk || prev_cjk) && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push(ch);
        prev_cjk = cur_cjk;
    }
    out
}

/// 展示回并：去掉两个 CJK 字符之间的单个空格（snippet 预览用；
/// 代价：原文中 CJK-CJK 间本就存在的空格在预览里塌缩——登记）
fn cjk_unspace(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &ch) in chars.iter().enumerate() {
        if ch == ' ' && i > 0 && i + 1 < chars.len() && is_cjk(chars[i - 1]) && is_cjk(chars[i + 1])
        {
            continue;
        }
        out.push(ch);
    }
    out
}

/// 查询串 → FTS5 MATCH 表达式（B3 clipboard fts_escape 逐形扩展）：
/// 按空白分段；含 CJK 的段整体作多词短语（一对引号内空格分词 ⇒ 单字 token 序列 ≈ 子串命中），
/// 非 CJK 段保持短语前缀 `"w"*` 形态。引号剥离 ⇒ 注入串只会无害地匹配不到。
fn fts_match_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|seg| {
            let strip = |s: String| s.replace('"', "");
            if seg.chars().any(is_cjk) {
                format!("\"{}\"", strip(cjk_space(seg)))
            } else {
                format!("\"{}\"*", strip(seg.to_string()))
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ")
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
            CREATE INDEX IF NOT EXISTS idx_links_dst ON links(dst_path);
            CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
                path,
                title,
                body,
                path_key UNINDEXED,
                tokenize='unicode61'
            );",
        )
        .map_err(|e| NoteError::Db(format!("建表失败: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
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
        tx.execute("DELETE FROM tags WHERE note_path = ?1", params!(row.path))
            .map_err(db)?;
        for tag in &row.tags {
            tx.execute(
                "INSERT OR IGNORE INTO tags (note_path, tag) VALUES (?1, ?2)",
                params!(row.path, tag),
            )
            .map_err(db)?;
        }
        tx.execute("DELETE FROM links WHERE src = ?1", params!(row.path))
            .map_err(db)?;
        for (dst, dst_path) in &row.links {
            tx.execute(
                "INSERT OR REPLACE INTO links (src, dst, dst_path) VALUES (?1, ?2, ?3)",
                params!(row.path, dst, dst_path),
            )
            .map_err(db)?;
        }
        // FTS5： indexed 列等值删除实为 MATCH 约束（会误删长路径），
        // 故用 path_key UNINDEXED 列做精确删除/定位（偏离登记：任务书三列签名 + 一列 UNINDEXED）
        tx.execute(
            "DELETE FROM notes_fts WHERE path_key = ?1",
            params!(row.path),
        )
        .map_err(db)?;
        tx.execute(
            "INSERT INTO notes_fts (path, title, body, path_key) VALUES (?1, ?2, ?3, ?4)",
            params!(
                cjk_space(&row.path),
                cjk_space(&row.title),
                cjk_space(&row.body),
                row.path
            ),
        )
        .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(())
    }

    /// 删除笔记索引（级联 tags/links）
    pub fn remove(&self, path: &str) -> Result<()> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction().map_err(db)?;
        tx.execute("DELETE FROM notes WHERE path = ?1", params!(path))
            .map_err(db)?;
        tx.execute("DELETE FROM tags WHERE note_path = ?1", params!(path))
            .map_err(db)?;
        tx.execute("DELETE FROM links WHERE src = ?1", params!(path))
            .map_err(db)?;
        tx.execute("DELETE FROM notes_fts WHERE path_key = ?1", params!(path))
            .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(())
    }

    /// 重命名索引路径（notes/tags/links.src 同步；links.dst_path 由调用方重索引后自然更新）
    pub fn rename_path(&self, old: &str, new: &str) -> Result<()> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction().map_err(db)?;
        tx.execute(
            "UPDATE notes SET path = ?2 WHERE path = ?1",
            params!(old, new),
        )
        .map_err(db)?;
        tx.execute(
            "UPDATE tags SET note_path = ?2 WHERE note_path = ?1",
            params!(old, new),
        )
        .map_err(db)?;
        tx.execute(
            "UPDATE links SET src = ?2 WHERE src = ?1",
            params!(old, new),
        )
        .map_err(db)?;
        // fts 行随路径迁移：title/body 列内容不变（已是 cjk_space 形态），仅重建 path/path_key 列
        let old_row: Option<(String, String)> = tx
            .query_row(
                "SELECT title, body FROM notes_fts WHERE path_key = ?1",
                params!(old),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db)?;
        if let Some((title, body)) = old_row {
            tx.execute("DELETE FROM notes_fts WHERE path_key = ?1", params!(old))
                .map_err(db)?;
            tx.execute(
                "INSERT INTO notes_fts (path, title, body, path_key) VALUES (?1, ?2, ?3, ?4)",
                params!(cjk_space(new), title, body, new),
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(())
    }

    /// 全量列表（tags 两步查询内存合并，避免 group_concat 解析）
    pub fn list(&self) -> Result<Vec<NoteMeta>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT path, title, mtime_ms, size FROM notes ORDER BY path")
            .map_err(db)?;
        let rows: Vec<(String, String, i64, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(db)?
            .collect::<std::result::Result<_, _>>()
            .map_err(db)?;
        let mut stmt2 = conn
            .prepare("SELECT note_path, tag FROM tags")
            .map_err(db)?;
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
        let mut stmt = conn
            .prepare("SELECT src, dst FROM links WHERE dst_path = ?1 ORDER BY src")
            .map_err(db)?;
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
        let mut stmt = conn
            .prepare("SELECT dst, dst_path FROM links WHERE src = ?1 ORDER BY dst")
            .map_err(db)?;
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
            .map(|(src, _)| Backlink {
                src,
                title: String::new(),
                snippet: String::new(),
            })
            .collect())
    }

    /// 全量清空索引四表（reindex 前置；cards 为用户数据不动）
    pub fn clear(&self) -> Result<()> {
        let conn = self.lock();
        conn.execute_batch(
            "DELETE FROM notes; DELETE FROM tags; DELETE FROM links; DELETE FROM notes_fts;",
        )
        .map_err(db)?;
        Ok(())
    }

    // ---------- T-B7-21：正文搜索 ----------

    /// FTS5 全文检索：path/title/body 三列均可命中；snippet 取正文列；
    /// `ORDER BY rank, path_key` 保证同分稳定序；limit 上限 200，0 视为参数非法（登记实形：拒）。
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>> {
        if query.trim().is_empty() {
            return Ok(vec![]);
        }
        if limit == 0 {
            return Err(NoteError::BadState("搜索 limit 必须 ≥1".into()));
        }
        let match_query = fts_match_query(query);
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT f.path_key, n.title,
                        snippet(notes_fts, 2, '', '', ' ... ', 20), f.rank
                 FROM notes_fts AS f
                 LEFT JOIN notes AS n ON n.path = f.path_key
                 WHERE notes_fts MATCH ?1
                 ORDER BY f.rank, f.path_key
                 LIMIT ?2",
            )
            .map_err(db)?;
        let rows = stmt
            .query_map(params!(match_query, limit.min(200)), |r| {
                Ok(SearchHit {
                    path: r.get(0)?,
                    title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    snippet: cjk_unspace(&r.get::<_, String>(2)?),
                    rank: r.get(3)?,
                })
            })
            .map_err(db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    fn lock(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.conn.lock()
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
            links: links
                .into_iter()
                .map(|(d, p)| (d.into(), p.into()))
                .collect(),
            body: String::new(),
        }
    }

    #[test]
    fn upsert_list_remove_roundtrip() {
        let idx = NoteIndex::open(&tmpdb("round")).unwrap();
        idx.upsert(row("a.md", vec![("b", "b.md"), ("x", "")]))
            .unwrap();
        idx.upsert(row("b.md", vec![])).unwrap();
        let list = idx.list().unwrap();
        assert_eq!(list.len(), 2);
        let a = list.iter().find(|n| n.path == "a.md").unwrap();
        assert_eq!(a.tags, vec!["a"]);
        assert_eq!(
            idx.links_to("b.md").unwrap(),
            vec![("a.md".into(), "b".into())]
        );
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

    // ---------- T-B7-21：FTS5 正文搜索（任务书字面测试名优先于命名惯例） ----------

    #[allow(non_snake_case)]
    fn srow(path: &str, title: &str, body: &str) -> NoteIndexRow {
        NoteIndexRow {
            path: path.into(),
            title: title.into(),
            mtime_ms: 1,
            size: 2,
            tags: vec![],
            links: vec![],
            body: body.into(),
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-21）字面测试名优先于 rustc 命名惯例
    fn notesSearch_cjkQuery_hitsBody() {
        let idx = NoteIndex::open(&tmpdb("cjk")).unwrap();
        idx.upsert(srow("a.md", "日常记录", "今天同步失败了两遍"))
            .unwrap();
        // 诱饵：含「同步」不含「失败」/含「失败」不含「同步」——短语相邻语义必须拒止
        idx.upsert(srow("b.md", "运行日志", "同步一切正常"))
            .unwrap();
        idx.upsert(srow("c.md", "错误清单", "操作失败了")).unwrap();
        let hits = idx.search("同步失败", 10).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].path, "a.md");
        assert_eq!(hits[0].title, "日常记录");
        assert!(
            hits[0].snippet.contains("同步失败"),
            "{:?}",
            hits[0].snippet
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名：注入红线
    fn notesSearch_injectionString_noResultsNoPanic() {
        let idx = NoteIndex::open(&tmpdb("inj")).unwrap();
        idx.upsert(srow("a.md", "同步笔记", "备份与恢复流程"))
            .unwrap();
        let r = idx.search("a' OR 1=1--", 10);
        assert!(r.is_ok(), "{:?}", r.err());
        assert!(r.unwrap().is_empty());
        // 双引号变异形同样不炸
        assert!(idx.search("\"OR\" 1=1", 10).is_ok());
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名
    fn notesSearch_limitBounded() {
        let idx = NoteIndex::open(&tmpdb("lim")).unwrap();
        for i in 0..3 {
            idx.upsert(srow(&format!("note{i}.md"), "标题", "同步失败记录"))
                .unwrap();
        }
        // 0 → 拒（登记实形）
        assert!(idx.search("同步失败", 0).is_err());
        assert_eq!(idx.search("同步失败", 1).unwrap().len(), 1);
        assert_eq!(idx.search("同步失败", 2).unwrap().len(), 2);
        // 超上限 → 钳位 200，不炸不截真值（此处只 3 篇）
        assert_eq!(idx.search("同步失败", u32::MAX).unwrap().len(), 3);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名
    fn notesSearch_titlePathBody_rankedGroups() {
        let idx = NoteIndex::open(&tmpdb("grp")).unwrap();
        idx.upsert(srow("x.md", "sync 配置手册", "无关正文"))
            .unwrap();
        idx.upsert(srow("sync.md", "周报", "例行内容")).unwrap();
        idx.upsert(srow("z.md", "故障单", "sync 超时了三小时"))
            .unwrap();
        let first = idx.search("sync", 50).unwrap();
        let paths: Vec<&str> = first.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths.len(), 3, "{paths:?}");
        for p in ["x.md", "sync.md", "z.md"] {
            assert!(paths.contains(&p), "{paths:?}");
        }
        // 同词命中三列：排序稳定（二次调用逐字节一致）
        let second = idx.search("sync", 50).unwrap();
        let a: Vec<(&str, f64)> = first.iter().map(|h| (h.path.as_str(), h.rank)).collect();
        let b: Vec<(&str, f64)> = second.iter().map(|h| (h.path.as_str(), h.rank)).collect();
        assert_eq!(a, b);
    }
}
