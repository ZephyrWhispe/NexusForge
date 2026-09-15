//! C2 存储层（docs/impl/02 C2）
//!
//! - 专属库 `{appData}/db/clipboard.db`（DESIGN O3：每模块独立库）
//! - WAL + FTS5 外部内容表 + 触发器同步
//! - 去重插入：命中 content_hash → 置顶 + usage_count+1
//! - >64KB 内容写 blob 文件（DESIGN O4），主表存引用

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use host_core::error::AppError;
use rusqlite::{params, Connection, OptionalExtension};
use rusqlite_migration::{Migrations, M};

use crate::types::{now_ms, ClipEntry, Page, SearchQuery, BLOB_THRESHOLD};

pub struct ClipStore {
    conn: Arc<Mutex<Connection>>,
    blob_dir: PathBuf,
}

fn err(code: &str, e: impl std::fmt::Display) -> AppError {
    AppError::Storage { code: code.into(), message: e.to_string() }
}

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(
            r#"CREATE TABLE clip_entries (
                id TEXT PRIMARY KEY,
                content_type TEXT NOT NULL,
                content TEXT,
                content_hash TEXT NOT NULL,
                blob_path TEXT,
                origin TEXT NOT NULL DEFAULT 'local',
                source_app TEXT,
                pinned INTEGER DEFAULT 0,
                group_name TEXT,
                secret INTEGER DEFAULT 0,
                created_at INTEGER NOT NULL,
                usage_count INTEGER DEFAULT 0
            );
            CREATE INDEX idx_entries_created ON clip_entries(created_at DESC);
            CREATE INDEX idx_entries_hash ON clip_entries(content_hash);
            CREATE VIRTUAL TABLE clip_fts USING fts5(
                content, content='clip_entries', content_rowid='rowid', tokenize='unicode61');
            CREATE TRIGGER clip_ai AFTER INSERT ON clip_entries BEGIN
                INSERT INTO clip_fts(rowid, content) VALUES (new.rowid, new.content); END;
            CREATE TRIGGER clip_ad AFTER DELETE ON clip_entries BEGIN
                INSERT INTO clip_fts(clip_fts, rowid, content) VALUES('delete', old.rowid, old.content); END;
            CREATE TRIGGER clip_au AFTER UPDATE ON clip_entries BEGIN
                INSERT INTO clip_fts(clip_fts, rowid, content) VALUES('delete', old.rowid, old.content);
                INSERT INTO clip_fts(rowid, content) VALUES (new.rowid, new.content); END;
            CREATE TABLE paste_stack (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                entry_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                FOREIGN KEY (entry_id) REFERENCES clip_entries(id) ON DELETE CASCADE
            );"#,
        ),
    ])
}

impl ClipStore {
    pub fn open(db_path: &Path, blob_dir: PathBuf) -> Result<Self, AppError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| err("CLIPBOARD_STORAGE_002", e))?;
        }
        std::fs::create_dir_all(&blob_dir).map_err(|e| err("CLIPBOARD_STORAGE_002", e))?;
        let conn = Connection::open(db_path).map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        let mut conn = conn;
        migrations()
            .to_latest(&mut conn)
            .map_err(|e| err("CLIPBOARD_STORAGE_003", e))?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)), blob_dir })
    }

    /// 去重插入（docs/impl/02 C2 算法）；命中 hash → 置顶并返回既有 id
    pub fn insert(&self, text: &str, group: Option<&'static str>, secret: bool, source_app: Option<&str>) -> Result<String, AppError> {
        let hash = content_hash(text);
        let now = now_ms();
        let conn = self.conn.lock().expect("clipboard db 锁");
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM clip_entries WHERE content_hash = ?1 LIMIT 1",
                params![hash],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;

        if let Some(id) = existing {
            conn.execute(
                "UPDATE clip_entries SET created_at = ?2, usage_count = usage_count + 1, pinned = 0 WHERE id = ?1",
                params![id, now],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            return Ok(id);
        }

        let id = uuid::Uuid::now_v7().to_string();
        let (content_col, blob_path): (String, Option<String>) =
            if text.len() > BLOB_THRESHOLD {
                let name = format!("{}.txt", hash);
                let blob = self.blob_dir.join(&name);
                std::fs::write(&blob, text).map_err(|e| err("CLIPBOARD_STORAGE_002", e))?;
                (String::new(), Some(name))
            } else if secret {
                (String::new(), None) // 密文由管线层写入前替换 content
            } else {
                (text.to_string(), None)
            };
        conn.execute(
            r#"INSERT INTO clip_entries
               (id, content_type, content, content_hash, blob_path, origin, source_app, pinned, group_name, secret, created_at)
               VALUES (?1, 'text', ?2, ?3, ?4, 'local', ?5, 0, ?6, ?7, ?8)"#,
            params![id, content_col, hash, blob_path, source_app, group, secret as i64, now],
        )
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(id)
    }

    /// 写入加密后的密文（secret 条目专用；密文经 DPAPI，Base64 由调用方处理）
    pub fn insert_encrypted(&self, encrypted_b64: &str, group: Option<&'static str>, source_app: Option<&str>) -> Result<String, AppError> {
        let hash = content_hash(encrypted_b64);
        let id = uuid::Uuid::now_v7().to_string();
        let conn = self.conn.lock().expect("clipboard db 锁");
        conn.execute(
            r#"INSERT INTO clip_entries
               (id, content_type, content, content_hash, origin, source_app, pinned, group_name, secret, created_at)
               VALUES (?1, 'text', ?2, ?3, 'local', ?4, 0, ?5, 1, ?6)"#,
            params![id, encrypted_b64, hash, source_app, group, now_ms()],
        )
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(id)
    }

    /// 解密读取（clipboard_get 专用；secret 条目需 DPAPI 还原）
    pub fn get_content(&self, id: &str, unprotect: impl Fn(&[u8]) -> Result<Vec<u8>, AppError>) -> Result<Option<String>, AppError> {
        let conn = self.conn.lock().expect("clipboard db 锁");
        let row: Option<(String, Option<String>, i64)> = conn
            .query_row(
                "SELECT content, blob_path, secret FROM clip_entries WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        let Some((content, blob_path, is_secret)) = row else {
            return Ok(None);
        };
        if let Some(blob) = blob_path {
            let raw = std::fs::read(self.blob_dir.join(blob))
                .map_err(|e| err("CLIPBOARD_STORAGE_002", e))?;
            return Ok(Some(String::from_utf8_lossy(&raw).to_string()));
        }
        if is_secret == 1 {
            use base64::Engine;
            let cipher = base64::engine::general_purpose::STANDARD
                .decode(&content)
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            let plain = unprotect(&cipher)?;
            return Ok(Some(String::from_utf8_lossy(&plain).to_string()));
        }
        Ok(Some(content))
    }

    pub fn search(&self, q: &SearchQuery) -> Result<Page<ClipEntry>, AppError> {
        let size = q.size.unwrap_or(50).min(200) as i64;
        let page = q.page.unwrap_or(0) as i64;
        let conn = self.conn.lock().expect("clipboard db 锁");

        let where_parts = build_filters(q);
        let where_sql = if where_parts.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_parts.join(" AND "))
        };

        let use_fts = q.text.as_deref().map(|t| !t.trim().is_empty()).unwrap_or(false);
        let (sql, fts_query) = if use_fts {
            let fts_query = fts_escape(q.text.as_deref().unwrap());
            (
                format!(
                    r#"SELECT e.id, e.content_type, e.content, e.blob_path, e.origin, e.source_app,
                              e.pinned, e.group_name, e.secret, e.created_at, e.usage_count
                       FROM clip_fts f JOIN clip_entries e ON e.rowid = f.rowid
                       {where_sql} AND clip_fts MATCH ?1
                       ORDER BY rank, e.created_at DESC LIMIT {size} OFFSET {}"#,
                    page * size
                ),
                fts_query,
            )
        } else {
            (
                format!(
                    r#"SELECT id, content_type, content, blob_path, origin, source_app,
                              pinned, group_name, secret, created_at, usage_count
                       FROM clip_entries {where_sql}
                       ORDER BY pinned DESC, created_at DESC LIMIT {size} OFFSET {}"#,
                    page * size
                ),
                String::new(),
            )
        };

        let mapper = |r: &rusqlite::Row| -> rusqlite::Result<ClipEntry> {
            let content: Option<String> = r.get(2)?;
            let content_type: String = r.get(1)?;
            let secret: i64 = r.get(8)?;
            let group: Option<String> = r.get(7)?;
            let blob_path: Option<String> = r.get(3)?;
            let preview = match content_type.as_str() {
                "image" => match content.as_deref() {
                    Some(dims) => format!("[图片 {dims}]"),
                    None => "[图片]".into(),
                },
                "files" => {
                    let n = content.as_deref().map(|c| c.lines().count()).unwrap_or(0);
                    let first = content
                        .as_deref()
                        .and_then(|c| c.lines().next())
                        .unwrap_or("");
                    format!("[文件 ×{n}] {}", first.chars().take(60).collect::<String>())
                }
                _ => build_preview(&content.unwrap_or_default(), secret == 1),
            };
            Ok(ClipEntry {
                id: r.get(0)?,
                content_type: content_type.leak() as &'static str,
                preview,
                blob_path,
                origin: "local",
                source_app: r.get(5)?,
                pinned: r.get::<_, i64>(6)? != 0,
                group: group.map(|g| leak_group(&g)),
                secret: secret == 1,
                created_at: r.get(9)?,
                usage_count: r.get::<_, i64>(10)? as u32,
            })
        };

        let items: Vec<ClipEntry> = if use_fts {
            let mut stmt = conn.prepare(&sql).map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            let rows = stmt.query_map(params![fts_query], mapper).map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            rows.filter_map(|r| r.ok()).collect()
        } else if let Some(g) = &q.group {
            let mut stmt = conn.prepare(&sql).map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            let rows = stmt.query_map(params![g], mapper).map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            rows.filter_map(|r| r.ok()).collect()
        } else {
            let mut stmt = conn.prepare(&sql).map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            let rows = stmt.query_map([], mapper).map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let has_more = items.len() as i64 == size;
        Ok(Page { items, has_more, total: None })
    }

    pub fn pin(&self, id: &str, pinned: bool) -> Result<(), AppError> {
        let conn = self.conn.lock().expect("clipboard db 锁");
        conn.execute("UPDATE clip_entries SET pinned = ?2 WHERE id = ?1", params![id, pinned as i64])
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<Option<String>, AppError> {
        let conn = self.conn.lock().expect("clipboard db 锁");
        let blob: Option<String> = conn
            .query_row(
                "SELECT blob_path FROM clip_entries WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?
            .flatten();
        conn.execute("DELETE FROM clip_entries WHERE id = ?1", params![id])
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(blob)
    }

    pub fn clear(&self, keep_pinned: bool) -> Result<u32, AppError> {
        let conn = self.conn.lock().expect("clipboard db 锁");
        let n = conn
            .execute(
                if keep_pinned {
                    "DELETE FROM clip_entries WHERE pinned = 0"
                } else {
                    "DELETE FROM clip_entries"
                },
                [],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(n as u32)
    }

    pub fn push_stack(&self, id: &str) -> Result<(), AppError> {
        let conn = self.conn.lock().expect("clipboard db 锁");
        let pos: i64 = conn
            .query_row("SELECT COALESCE(MAX(position), 0) + 1 FROM paste_stack", [], |r| r.get(0))
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        conn.execute("INSERT INTO paste_stack (entry_id, position) VALUES (?1, ?2)", params![id, pos])
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(())
    }

    pub fn pop_stack(&self) -> Result<Option<String>, AppError> {
        let conn = self.conn.lock().expect("clipboard db 锁");
        let id: Option<String> = conn
            .query_row(
                "SELECT entry_id FROM paste_stack ORDER BY position DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        if let Some(id) = &id {
            conn.execute(
                "DELETE FROM paste_stack WHERE id = (SELECT id FROM paste_stack ORDER BY position DESC LIMIT 1)",
                [],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        }
        Ok(id)
    }

    /// 图片入库：字节写 blob（{hash}.dib），主表存引用
    pub fn insert_image(&self, format: &str, width: u32, height: u32, bytes: &[u8], source_app: Option<&str>) -> Result<String, AppError> {
        let hash = content_hash_bytes(bytes);
        let conn = self.conn.lock().expect("clipboard db 锁");
        let existing: Option<String> = conn
            .query_row("SELECT id FROM clip_entries WHERE content_hash = ?1 LIMIT 1", params![hash], |r| r.get(0))
            .optional()
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        if let Some(id) = existing {
            conn.execute(
                "UPDATE clip_entries SET created_at = ?2, usage_count = usage_count + 1 WHERE id = ?1",
                params![id, now_ms()],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            return Ok(id);
        }
        let id = uuid::Uuid::now_v7().to_string();
        let blob = format!("{hash}.{format}");
        std::fs::write(self.blob_dir.join(&blob), bytes).map_err(|e| err("CLIPBOARD_STORAGE_002", e))?;
        conn.execute(
            r#"INSERT INTO clip_entries
               (id, content_type, content, content_hash, blob_path, origin, source_app, pinned, group_name, secret, created_at)
               VALUES (?1, 'image', ?2, ?3, ?4, 'local', ?5, 0, NULL, 0, ?6)"#,
            params![id, format!("{width}x{height}"), hash, blob, source_app, now_ms()],
        )
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(id)
    }

    /// 文件列表入库：路径列表拼接为 content（去重键）
    pub fn insert_files(&self, paths: &[std::path::PathBuf], source_app: Option<&str>) -> Result<String, AppError> {
        let content = paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        self.insert_typed(&content, "files", source_app)
    }

    fn insert_typed(&self, content: &str, content_type: &'static str, source_app: Option<&str>) -> Result<String, AppError> {
        let hash = content_hash(content);
        let now = now_ms();
        let conn = self.conn.lock().expect("clipboard db 锁");
        let existing: Option<String> = conn
            .query_row("SELECT id FROM clip_entries WHERE content_hash = ?1 LIMIT 1", params![hash], |r| r.get(0))
            .optional()
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        if let Some(id) = existing {
            conn.execute(
                "UPDATE clip_entries SET created_at = ?2, usage_count = usage_count + 1 WHERE id = ?1",
                params![id, now],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            return Ok(id);
        }
        let id = uuid::Uuid::now_v7().to_string();
        conn.execute(
            r#"INSERT INTO clip_entries
               (id, content_type, content, content_hash, origin, source_app, pinned, group_name, secret, created_at)
               VALUES (?1, ?2, ?3, ?4, 'local', ?5, 0, NULL, 0, ?6)"#,
            params![id, content_type, content, hash, source_app, now],
        )
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(id)
    }

    /// 条目载荷（paste 用）：文本 / 图片原始字节
    pub fn get_payload(&self, id: &str) -> Result<Option<Payload>, AppError> {
        let conn = self.conn.lock().expect("clipboard db 锁");
        let row: Option<(String, Option<String>, Option<String>, i64)> = conn
            .query_row(
                "SELECT content_type, content, blob_path, secret FROM clip_entries WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        let Some((content_type, content_opt, blob_path, secret)) = row else {
            return Ok(None);
        };
        match content_type.as_str() {
            "image" => {
                let Some(blob) = blob_path else { return Ok(None) };
                let bytes = std::fs::read(self.blob_dir.join(&blob))
                    .map_err(|e| err("CLIPBOARD_STORAGE_002", e))?;
                let format = blob
                    .rsplit('.')
                    .next()
                    .unwrap_or("dib")
                    .to_string();
                Ok(Some(Payload::Image { format, bytes }))
            }
            "files" => Ok(Some(Payload::Files(
                content_opt
                    .unwrap_or_default()
                    .lines()
                    .map(std::path::PathBuf::from)
                    .collect(),
            ))),
            _ => {
                let content = content_opt.unwrap_or_default();
                if secret == 1 {
                    Ok(Some(Payload::SecretB64(content)))
                } else {
                    Ok(Some(Payload::Text(content)))
                }
            }
        }
    }

    /// 分组计数（SubNav 角标；text=无分组文本，secret 按 secret 标记，files 单独）
    pub fn group_counts(&self) -> Result<serde_json::Value, AppError> {
        let conn = self.conn.lock().expect("clipboard db 锁");
        let mut counts = serde_json::Map::new();
        let mut total = 0u32;

        // text：无分组且非敏感的文本
        let text: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM clip_entries
                 WHERE content_type = 'text' AND group_name IS NULL AND secret = 0",
                [],
                |r| r.get(0),
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        counts.insert("text".into(), serde_json::json!(text));
        total += text as u32;

        // 有分组（url/json/code/color…）与 secret
        let rows = conn
            .prepare(
                "SELECT COALESCE(group_name, CASE WHEN secret = 1 THEN 'secret' END) AS g,
                        COUNT(*) AS n
                 FROM clip_entries
                 WHERE group_name IS NOT NULL OR secret = 1
                 GROUP BY g",
            )
            .and_then(|mut s| {
                s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                    .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            })
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        for (g, n) in rows {
            if let Some(g) = g.split(',').next() {
                counts.insert(g.to_string(), serde_json::json!(n));
                total += n as u32;
            }
        }

        // files
        let files: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM clip_entries WHERE content_type = 'files'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        counts.insert("files".into(), serde_json::json!(files));
        total += files as u32;

        counts.insert("all".into(), serde_json::json!(total));
        Ok(serde_json::Value::Object(counts))
    }

    /// C9 清理：retention_days > 0 时按保留期，再按 max_entries 上限淘汰（置顶除外）
    pub fn purge(&self, retention_days: u32, max_entries: u32) -> Result<u32, AppError> {
        let conn = self.conn.lock().expect("clipboard db 锁");
        if retention_days > 0 {
            let cutoff = now_ms() - (retention_days as i64) * 86_400_000;
            let _ = conn.execute(
                "DELETE FROM clip_entries WHERE pinned = 0 AND created_at < ?1",
                params![cutoff],
            );
        }
        let n = conn
            .execute(
                r#"DELETE FROM clip_entries WHERE pinned = 0 AND id IN (
                     SELECT id FROM clip_entries WHERE pinned = 0
                     ORDER BY created_at DESC LIMIT -1 OFFSET ?1)"#,
                params![max_entries],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(n as u32)
    }
}

/// 条目载荷
pub enum Payload {
    Text(String),
    Files(Vec<std::path::PathBuf>),
    Image { format: String, bytes: Vec<u8> },
    /// 加密文本（Base64），由调用方经 CryptoPort 解密
    SecretB64(String),
}

fn content_hash(text: &str) -> String {
    content_hash_bytes(text.as_bytes())
}

fn content_hash_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// FTS5 语法转义：拆词并加前缀匹配，防止语法错误（docs/impl/02 C2 潜在问题）
fn fts_escape(q: &str) -> String {
    q.split_whitespace()
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn build_filters(q: &SearchQuery) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(g) = &q.group {
        if g != "all" {
            if g == "secret" {
                parts.push("e.secret = 1".into());
            } else if g == "files" {
                parts.push("e.content_type = 'files'".into());
            } else {
                parts.push(format!("e.group_name = '{}'", g.replace('\'', "")));
            }
        }
    }
    parts
}

fn build_preview(content: &str, secret: bool) -> String {
    if secret {
        return "[敏感内容] 已加密存储".into();
    }
    let mut s: String = content.chars().take(200).collect();
    if content.chars().count() > 200 {
        s.push('…');
    }
    s
}

fn leak_group(g: &str) -> &'static str {
    // 分组名来自固定枚举（url/json/code/color/secret），安全泄漏为 'static
    match g {
        "url" => "url",
        "json" => "json",
        "code" => "code",
        "color" => "color",
        "secret" => "secret",
        other => Box::leak(other.to_string().into_boxed_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_temp(tag: &str) -> ClipStore {
        let dir = std::env::temp_dir().join(format!("nf_clip_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ClipStore::open(&dir.join("clipboard.db"), dir.join("blobs")).unwrap()
    }

    #[test]
    fn insert_dedup_pins_and_bumps_usage() {
        let s = open_temp("dedup");
        let id1 = s.insert("第一条内容", None, false, None).unwrap();
        let id2 = s.insert("第一条内容", None, false, None).unwrap();
        assert_eq!(id1, id2, "相同内容应去重返回同一 id");
        let page = s.search(&SearchQuery::default()).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].usage_count, 1);
    }

    #[test]
    fn fts_search_finds_text() {
        let s = open_temp("fts");
        s.insert("设计原则：Windows 原生优先", None, false, None).unwrap();
        s.insert("cargo build --release", Some("code"), false, None).unwrap();
        let q = SearchQuery { text: Some("原生".into()), ..Default::default() };
        let page = s.search(&q).unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.items[0].preview.contains("原生"));
        // FTS 语法注入防护
        let q2 = SearchQuery { text: Some("原\"生".into()), ..Default::default() };
        let _ = s.search(&q2).unwrap();
    }

    #[test]
    fn pin_clear_and_delete() {
        let s = open_temp("ops");
        let a = s.insert("A", None, false, None).unwrap();
        let _ = s.insert("B", None, false, None).unwrap();
        s.pin(&a, true).unwrap();
        let removed = s.clear(true).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(s.search(&SearchQuery::default()).unwrap().items.len(), 1);
        s.delete(&a).unwrap();
        assert_eq!(s.search(&SearchQuery::default()).unwrap().items.len(), 0);
    }

    #[test]
    fn big_content_goes_to_blob() {
        let s = open_temp("blob");
        let big = "x".repeat(BLOB_THRESHOLD + 10);
        let id = s.insert(&big, None, false, None).unwrap();
        let entry = &s.search(&SearchQuery::default()).unwrap().items[0];
        assert!(entry.blob_path.is_some(), ">64KB 内容应转 blob");
        let back = s.get_content(&id, |c| Ok(c.to_vec())).unwrap().unwrap();
        assert_eq!(back.len(), big.len());
    }
}
