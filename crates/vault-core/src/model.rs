//! V2 保险库数据模型（docs/impl/05 V2）：条目 / 文件夹 / 字段 + SQLite 持久化。
//!
//! 明密文边界：title / folder / favorite 明文（列表与搜索需要）；
//! fields JSON 与 totp_secret 整体加密（[`crate::crypto::seal_field`]），库表只见密文。
//! 加解密在 [`crate::vault::VaultService`]（唯一持 DEK 处），Store 只搬运 Row。

use std::path::Path;
use std::sync::{Arc, Mutex};

use host_core::error::AppError;
use rusqlite::{params, Connection, OptionalExtension};
use rusqlite_migration::{Migrations, M};
use serde::{Deserialize, Serialize};

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn db_err(code: &str, e: impl std::fmt::Display) -> AppError {
    AppError::Storage { code: code.into(), message: e.to_string() }
}

// ---------------------------------------------------------------------------
// 领域模型（解密态；仅存于解锁态内存 / 前端）
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Password,
    Url,
    Note,
    Otp,
    Text,
}

/// 单字段：kind 决定前端渲染（password 打码 + 复制、url 可点击…）
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EntryField {
    pub key: String,
    pub kind: FieldKind,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub folder_id: Option<String>,
    pub title: String,
    pub favorite: bool,
    pub fields: Vec<EntryField>,
    /// base32 TOTP 密钥（RFC 6238）
    pub totp_secret: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub created_at: i64,
}

// ---------------------------------------------------------------------------
// 存储行（密文态）
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct EntryRow {
    pub id: String,
    pub folder_id: Option<String>,
    pub title: String,
    pub favorite: bool,
    /// b64(nonce || ct)：fields JSON 整体加密
    pub fields_ct: String,
    /// b64(nonce || ct)：totp_secret
    pub totp_ct: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(
        r#"CREATE TABLE folders (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE entries (
            id TEXT PRIMARY KEY,
            folder_id TEXT REFERENCES folders(id) ON DELETE SET NULL,
            title TEXT NOT NULL,
            favorite INTEGER NOT NULL DEFAULT 0,
            fields_ct TEXT NOT NULL,
            totp_ct TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE INDEX idx_entries_folder ON entries(folder_id);
        CREATE INDEX idx_entries_updated ON entries(updated_at DESC);"#,
    )])
}

/// 专属库 `{appData}/db/vault.db`（DESIGN O3：每模块独立库；WAL）
pub struct VaultStore {
    conn: Arc<Mutex<Connection>>,
}

impl VaultStore {
    pub fn open(db_path: &Path) -> Result<Self, AppError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| db_err("VAULT_DB_002", e))?;
        }
        let mut conn = Connection::open(db_path).map_err(|e| db_err("VAULT_DB_001", e))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| db_err("VAULT_DB_001", e))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| db_err("VAULT_DB_001", e))?;
        migrations()
            .to_latest(&mut conn)
            .map_err(|e| db_err("VAULT_DB_003", e))?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    // ---- 文件夹 ----

    pub fn create_folder(&self, name: &str) -> Result<Folder, AppError> {
        let folder = Folder { id: uuid::Uuid::now_v7().to_string(), name: name.into(), created_at: now_ms() };
        let conn = self.conn.lock().expect("vault db 锁");
        conn.execute(
            "INSERT INTO folders (id, name, created_at) VALUES (?1, ?2, ?3)",
            params![folder.id, folder.name, folder.created_at],
        )
        .map_err(|e| db_err("VAULT_DB_004", e))?;
        Ok(folder)
    }

    pub fn list_folders(&self) -> Result<Vec<Folder>, AppError> {
        let conn = self.conn.lock().expect("vault db 锁");
        let mut stmt = conn
            .prepare("SELECT id, name, created_at FROM folders ORDER BY created_at")
            .map_err(|e| db_err("VAULT_DB_005", e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Folder { id: r.get(0)?, name: r.get(1)?, created_at: r.get(2)? })
            })
            .map_err(|e| db_err("VAULT_DB_005", e))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| db_err("VAULT_DB_005", e))
    }

    pub fn rename_folder(&self, id: &str, name: &str) -> Result<bool, AppError> {
        let conn = self.conn.lock().expect("vault db 锁");
        let n = conn
            .execute("UPDATE folders SET name = ?2 WHERE id = ?1", params![id, name])
            .map_err(|e| db_err("VAULT_DB_006", e))?;
        Ok(n > 0)
    }

    /// 删除文件夹：条目 folder_id 置 NULL（ON DELETE SET NULL），条目保留
    pub fn delete_folder(&self, id: &str) -> Result<bool, AppError> {
        let conn = self.conn.lock().expect("vault db 锁");
        let n = conn
            .execute("DELETE FROM folders WHERE id = ?1", params![id])
            .map_err(|e| db_err("VAULT_DB_007", e))?;
        Ok(n > 0)
    }

    // ---- 条目 ----

    pub fn insert_entry(&self, row: &EntryRow) -> Result<(), AppError> {
        let conn = self.conn.lock().expect("vault db 锁");
        conn.execute(
            "INSERT INTO entries (id, folder_id, title, favorite, fields_ct, totp_ct, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                row.id,
                row.folder_id,
                row.title,
                row.favorite as i64,
                row.fields_ct,
                row.totp_ct,
                row.created_at,
                row.updated_at
            ],
        )
        .map_err(|e| db_err("VAULT_DB_008", e))?;
        Ok(())
    }

    pub fn update_entry(&self, row: &EntryRow) -> Result<bool, AppError> {
        let conn = self.conn.lock().expect("vault db 锁");
        let n = conn
            .execute(
                "UPDATE entries SET folder_id = ?2, title = ?3, favorite = ?4, fields_ct = ?5,
                 totp_ct = ?6, updated_at = ?7 WHERE id = ?1",
                params![
                    row.id,
                    row.folder_id,
                    row.title,
                    row.favorite as i64,
                    row.fields_ct,
                    row.totp_ct,
                    row.updated_at
                ],
            )
            .map_err(|e| db_err("VAULT_DB_009", e))?;
        Ok(n > 0)
    }

    pub fn delete_entry(&self, id: &str) -> Result<bool, AppError> {
        let conn = self.conn.lock().expect("vault db 锁");
        let n = conn
            .execute("DELETE FROM entries WHERE id = ?1", params![id])
            .map_err(|e| db_err("VAULT_DB_010", e))?;
        Ok(n > 0)
    }

    /// 列表：folder_id None = 全部；search 按 title LIKE（明文列）
    pub fn list_entries(
        &self,
        folder_id: Option<&str>,
        search: Option<&str>,
    ) -> Result<Vec<EntryRow>, AppError> {
        let conn = self.conn.lock().expect("vault db 锁");
        let mut sql = String::from(
            "SELECT id, folder_id, title, favorite, fields_ct, totp_ct, created_at, updated_at FROM entries",
        );
        let mut conds: Vec<String> = Vec::new();
        let mut next_param = 1;
        if folder_id.is_some() {
            conds.push(format!("folder_id = ?{next_param}"));
            next_param += 1;
        }
        if let Some(s) = search {
            if !s.is_empty() {
                conds.push(format!("title LIKE ?{next_param}"));
                next_param += 1;
            }
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        sql.push_str(" ORDER BY updated_at DESC");
        let mut stmt = conn.prepare(&sql).map_err(|e| db_err("VAULT_DB_011", e))?;

        let like = search.map(|s| format!("%{s}%"));
        let map_row = |r: &rusqlite::Row| -> rusqlite::Result<EntryRow> {
            Ok(EntryRow {
                id: r.get(0)?,
                folder_id: r.get(1)?,
                title: r.get(2)?,
                favorite: r.get::<_, i64>(3)? != 0,
                fields_ct: r.get(4)?,
                totp_ct: r.get(5)?,
                created_at: r.get(6)?,
                updated_at: r.get(7)?,
            })
        };
        let rows = match (folder_id, like) {
            (Some(f), Some(l)) => stmt.query_map(params![f, l], map_row),
            (Some(f), None) => stmt.query_map(params![f], map_row),
            (None, Some(l)) => stmt.query_map(params![l], map_row),
            (None, None) => stmt.query_map([], map_row),
        }
        .map_err(|e| db_err("VAULT_DB_011", e))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| db_err("VAULT_DB_011", e))
    }

    pub fn get_entry(&self, id: &str) -> Result<Option<EntryRow>, AppError> {
        let conn = self.conn.lock().expect("vault db 锁");
        conn.query_row(
            "SELECT id, folder_id, title, favorite, fields_ct, totp_ct, created_at, updated_at
             FROM entries WHERE id = ?1",
            params![id],
            |r| {
                Ok(EntryRow {
                    id: r.get(0)?,
                    folder_id: r.get(1)?,
                    title: r.get(2)?,
                    favorite: r.get::<_, i64>(3)? != 0,
                    fields_ct: r.get(4)?,
                    totp_ct: r.get(5)?,
                    created_at: r.get(6)?,
                    updated_at: r.get(7)?,
                })
            },
        )
        .optional()
        .map_err(|e| db_err("VAULT_DB_012", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vault-db-{tag}-{}", uuid::Uuid::now_v7()));
        dir.join("vault.db")
    }

    fn row(id: &str, title: &str) -> EntryRow {
        EntryRow {
            id: id.into(),
            folder_id: None,
            title: title.into(),
            favorite: false,
            fields_ct: "ct".into(),
            totp_ct: None,
            created_at: now_ms(),
            updated_at: now_ms(),
        }
    }

    #[test]
    fn folder_crud_and_entry_isolation() {
        let store = VaultStore::open(&temp_db("crud")).unwrap();
        let f = store.create_folder("工作").unwrap();
        assert!(store.rename_folder(&f.id, "个人").unwrap());
        assert_eq!(store.list_folders().unwrap()[0].name, "个人");

        let mut r = row("e1", "GitHub");
        r.folder_id = Some(f.id.clone());
        store.insert_entry(&r).unwrap();
        assert_eq!(store.list_entries(Some(&f.id), None).unwrap().len(), 1);
        assert_eq!(store.list_entries(None, None).unwrap().len(), 1);

        // 删文件夹 → 条目保留、folder_id 置空
        assert!(store.delete_folder(&f.id).unwrap());
        assert!(store.list_folders().unwrap().is_empty());
        let kept = store.get_entry("e1").unwrap().unwrap();
        assert!(kept.folder_id.is_none());
    }

    #[test]
    fn entry_update_delete_search() {
        let store = VaultStore::open(&temp_db("ops")).unwrap();
        store.insert_entry(&row("a", "GitHub")).unwrap();
        store.insert_entry(&row("b", "GitLab")).unwrap();

        assert_eq!(store.list_entries(None, Some("git")).unwrap().len(), 2);
        assert_eq!(store.list_entries(None, Some("hub")).unwrap().len(), 1);

        let mut r = store.get_entry("a").unwrap().unwrap();
        r.title = "GitHub 改".into();
        r.favorite = true;
        assert!(store.update_entry(&r).unwrap());
        let got = store.get_entry("a").unwrap().unwrap();
        assert_eq!(got.title, "GitHub 改");
        assert!(got.favorite);

        assert!(store.delete_entry("a").unwrap());
        assert!(store.get_entry("a").unwrap().is_none());
    }
}
