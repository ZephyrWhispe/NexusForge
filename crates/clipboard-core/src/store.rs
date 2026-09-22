//! C2 存储层（docs/impl/02 C2）
//!
//! - 专属库 `{appData}/db/clipboard.db`（DESIGN O3：每模块独立库）
//! - WAL + FTS5 外部内容表 + 触发器同步
//! - 去重插入：命中 content_hash → 刷新鲜度 + usage_count+1（pinned 不动）
//! - >64KB 内容写 blob 文件（DESIGN O4），主表存引用

use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use host_core::error::AppError;
use rusqlite::{params, Connection, OptionalExtension};
use rusqlite_migration::{Migrations, M};

use crate::types::{now_ms, ClipEntry, Page, SearchQuery, StatsDto, SuggestionDto, BLOB_THRESHOLD};

pub struct ClipStore {
    conn: Arc<Mutex<Connection>>,
    blob_dir: PathBuf,
}

fn err(code: &str, e: impl std::fmt::Display) -> AppError {
    AppError::Storage {
        code: code.into(),
        message: e.to_string(),
    }
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
        // T-B3-4 智能分组建议（01§5-1 建议制）：旧行 suggested_group/confidence 为 NULL、
        // suggestion_dismissed 走 DEFAULT 0，零回填零迁移。
        M::up(
            r#"ALTER TABLE clip_entries ADD COLUMN suggested_group TEXT;
           ALTER TABLE clip_entries ADD COLUMN suggested_confidence REAL;
           ALTER TABLE clip_entries ADD COLUMN suggestion_dismissed INTEGER NOT NULL DEFAULT 0;"#,
        ),
        // T-B3-8 HTML 正文（01§8-6 HTML 轨）：旧行 NULL = "没有 HTML"，与"没捕获到"同义，零回填。
        M::up("ALTER TABLE clip_entries ADD COLUMN html TEXT;"),
    ])
}

/// `M::up` 之后仍需幂等补齐的列（备份还原/手工修表/跨版本文件复制的库）
const ADDED_COLUMNS: [(&str, &str); 4] = [
    ("suggested_group", "TEXT"),
    ("suggested_confidence", "REAL"),
    ("suggestion_dismissed", "INTEGER NOT NULL DEFAULT 0"),
    ("html", "TEXT"),
];

/// 幂等补列守卫：schema_version 已记账而列缺失的库（备份还原、手工修表、跨版本文件复制）
/// 单靠 `M::up` 不会再跑 ALTER，`open()` 必须在返回前补齐，否则后续每条 SELECT 都炸。
fn ensure_added_columns(conn: &Connection) -> Result<(), AppError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(clip_entries)")
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
    let have: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);
    for (name, decl) in ADDED_COLUMNS {
        if have.iter().any(|c| c == name) {
            continue;
        }
        conn.execute(
            &format!("ALTER TABLE clip_entries ADD COLUMN {name} {decl}"),
            [],
        )
        .map_err(|e| err("CLIPBOARD_STORAGE_003", e))?;
    }
    Ok(())
}

/// 行 → ClipEntry 的唯一映射：列序契约 id, content_type, content, blob_path, origin,
/// source_app, pinned, group_name, secret, created_at, usage_count，外加具名列 `has_html`
/// （`html IS NOT NULL`，正文本身不进列表查询）。
/// （search 的两条 SELECT 与 stack 的 entries_in_order 共用，改列序须同时改三处）。
fn entry_from_row(r: &rusqlite::Row) -> rusqlite::Result<ClipEntry> {
    let content: Option<String> = r.get(2)?;
    let content_type: String = r.get(1)?;
    let secret: i64 = r.get(8)?;
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
        content_type,
        preview,
        blob_path,
        // D-25：真读 origin 列；'remote' 之外一律兜底 local（兼容旧库/脏值）
        origin: if r.get::<_, String>(4)? == "remote" {
            "remote"
        } else {
            "local"
        },
        source_app: r.get(5)?,
        pinned: r.get::<_, i64>(6)? != 0,
        // 组名是用户输入（任意 Unicode），按列名取值且不再泄漏为 'static
        group: r.get::<_, Option<String>>("group_name")?,
        secret: secret == 1,
        created_at: r.get(9)?,
        usage_count: r.get::<_, i64>(10)? as u32,
        has_html: r.get::<_, i64>("has_html")? != 0,
    })
}

/// 一行文本入库的全部输入（T-B3-4 收口：消多参位置漂移 + 承 suggested 两列 +
/// 为 T-B3-8 的 html 列预留零 churn 扩展位）。
pub struct NewClip<'a> {
    pub text: &'a str,
    /// 写定分组（auto_group 开且有分类结果）；None = 不分组
    pub group: Option<&'a str>,
    /// 分类器建议 (组名, 置信度)：与 group 分开存，采纳流可复算
    pub suggested: Option<(&'a str, f32)>,
    pub secret: bool,
    pub source_app: Option<&'a str>,
    pub origin: &'a str,
    /// `HTML Format` 正文（T-B3-8）：None = 这次复制没有富文本。敏感行恒 None——
    /// HTML 那份是明文副本，加密了正文却顺手存 HTML 等于自己绕开自己的门。
    pub html: Option<&'a str>,
}

impl<'a> NewClip<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            group: None,
            suggested: None,
            secret: false,
            source_app: None,
            origin: "local",
            html: None,
        }
    }

    pub fn group(mut self, group: &'a str) -> Self {
        self.group = Some(group);
        self
    }

    pub fn suggested(mut self, suggested: (&'a str, f32)) -> Self {
        self.suggested = Some(suggested);
        self
    }

    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    pub fn from_app(mut self, source_app: &'a str) -> Self {
        self.source_app = Some(source_app);
        self
    }

    pub fn origin(mut self, origin: &'a str) -> Self {
        self.origin = origin;
        self
    }

    pub fn html(mut self, html: &'a str) -> Self {
        self.html = Some(html);
        self
    }
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
        ensure_added_columns(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            blob_dir,
        })
    }

    /// 去重插入（docs/impl/02 C2 算法）；命中 hash → 置顶并返回既有 id。
    /// origin：local|remote（D-25）；dedup 命中且新事件为 local → 晋升 'local'，
    /// remote 事件不得改写既有归属（只收紧不放松）。
    /// dedup 命中只刷新鲜度/计数/归属——分组、建议与 pinned 一律不动
    /// （用户手工组名与已忽略的建议都不因重复复制而复活或覆写）。
    pub fn insert_row(&self, c: &NewClip) -> Result<String, AppError> {
        let hash = content_hash(c.text);
        let now = now_ms();
        let conn = self.conn.lock();
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM clip_entries WHERE content_hash = ?1 LIMIT 1",
                params![hash],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;

        if let Some(id) = existing {
            // 缺陷⑧（09 §8.1）：去重命中只刷新鲜度与计数，pinned 保持不变——
            // 重复复制一个已置顶条目不得静默取消置顶（与图片/文件去重路径同语义）
            // html 走 COALESCE：只补空缺、不覆写既有正文（先复制纯文本再复制富文本时，
            // 那份 HTML 是新增信息而非新意图，用户没要求把旧的换掉）。
            conn.execute(
                "UPDATE clip_entries SET created_at = ?2, usage_count = usage_count + 1,
                 origin = CASE WHEN ?3 = 'local' THEN 'local' ELSE origin END,
                 html = COALESCE(html, ?4) WHERE id = ?1",
                params![id, now, c.origin, c.html],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            return Ok(id);
        }

        let id = uuid::Uuid::now_v7().to_string();
        let (content_col, blob_path): (String, Option<String>) = if c.text.len() > BLOB_THRESHOLD {
            let name = format!("{}.txt", hash);
            let blob = self.blob_dir.join(&name);
            std::fs::write(&blob, c.text).map_err(|e| err("CLIPBOARD_STORAGE_002", e))?;
            (String::new(), Some(name))
        } else if c.secret {
            (String::new(), None) // 密文由管线层写入前替换 content
        } else {
            (c.text.to_string(), None)
        };
        let (sugg_group, sugg_conf) = split_suggestion(c.suggested);
        conn.execute(
            r#"INSERT INTO clip_entries
               (id, content_type, content, content_hash, blob_path, origin, source_app, pinned, group_name, secret, created_at, suggested_group, suggested_confidence, html)
               VALUES (?1, 'text', ?2, ?3, ?4, ?9, ?5, 0, ?6, ?7, ?8, ?10, ?11, ?12)"#,
            params![
                id,
                content_col,
                hash,
                blob_path,
                c.source_app,
                c.group,
                c.secret as i64,
                now,
                c.origin,
                sugg_group,
                sugg_conf,
                c.html
            ],
        )
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(id)
    }

    /// 写入加密后的密文（secret 条目专用；密文为信封格式（D-04），Base64 由调用方处理）
    pub fn insert_encrypted(
        &self,
        encrypted_b64: &str,
        group: Option<&str>,
        suggested: Option<(&str, f32)>,
        source_app: Option<&str>,
        origin: &str,
    ) -> Result<String, AppError> {
        let hash = content_hash(encrypted_b64);
        let id = uuid::Uuid::now_v7().to_string();
        let (sugg_group, sugg_conf) = split_suggestion(suggested);
        let conn = self.conn.lock();
        conn.execute(
            r#"INSERT INTO clip_entries
               (id, content_type, content, content_hash, origin, source_app, pinned, group_name, secret, created_at, suggested_group, suggested_confidence)
               VALUES (?1, 'text', ?2, ?3, ?7, ?4, 0, ?5, 1, ?6, ?8, ?9)"#,
            params![
                id,
                encrypted_b64,
                hash,
                source_app,
                group,
                now_ms(),
                origin,
                sugg_group,
                sugg_conf
            ],
        )
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(id)
    }

    /// 解密读取（clipboard_get 专用；secret 条目需信封解密还原）
    pub fn get_content(
        &self,
        id: &str,
        unprotect: impl Fn(&[u8]) -> Result<Vec<u8>, AppError>,
    ) -> Result<Option<String>, AppError> {
        let conn = self.conn.lock();
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

    /// HTML 正文单点读（T-B3-8）：列表/堆栈查询只带 `has_html` 布尔，正文须经此口显式取。
    /// 无该列值 → Ok(None)（"这次复制没有富文本"），不报错也不给空串冒充。
    pub fn get_html(&self, id: &str) -> Result<Option<String>, AppError> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT html FROM clip_entries WHERE id = ?1",
            params![id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        // 两层 Option 语义不同：外层 None = 没有这一行，内层 None = 有行但无 HTML 正文；
        // 对读口两者都是"这里没有富文本"，故 flatten 成一层。
        .map(|row| row.flatten())
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))
    }

    pub fn search(&self, q: &SearchQuery) -> Result<Page<ClipEntry>, AppError> {
        let size = q.size.unwrap_or(50).min(200) as i64;
        let page = q.page.unwrap_or(0) as i64;
        let conn = self.conn.lock();

        // 条件与绑定值同源（build_filters）：占位符按 args 出现序编号，
        // 追加 FTS MATCH 时接在其后，两分支共用同一条查询路径。
        let syntax = q
            .text
            .as_deref()
            .map(crate::query::parse_search_syntax)
            .unwrap_or_default();
        let (filters, mut args) = build_filters(q);
        let mut conds: Vec<String> = if filters.is_empty() {
            Vec::new()
        } else {
            vec![filters]
        };
        // 语法解出的组名按真名匹配（伪键只属于 group 字段那条筛选维度，两个维度是 AND）
        if let Some(g) = syntax.group {
            conds.push(format!("e.group_name = ?{}", args.len() + 1));
            args.push(Box::new(g));
        }
        let content_type = syntax.content_type.or(q.content_type.clone());
        if let Some(ct) = &content_type {
            conds.push(format!("e.content_type = ?{}", args.len() + 1));
            args.push(Box::new(ct.clone()));
        }
        if syntax.unknown_type {
            conds.push("0".to_string());
        }
        let use_fts = !syntax.text.trim().is_empty();
        if use_fts {
            conds.push(format!("clip_fts MATCH ?{}", args.len() + 1));
            args.push(Box::new(fts_escape(&syntax.text)));
        }
        let where_sql = if conds.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conds.join(" AND "))
        };

        let sql = if use_fts {
            format!(
                r#"SELECT e.id, e.content_type, e.content, e.blob_path, e.origin, e.source_app,
                          e.pinned, e.group_name, e.secret, e.created_at, e.usage_count,
                          e.html IS NOT NULL AS has_html
                   FROM clip_fts f JOIN clip_entries e ON e.rowid = f.rowid
                   {where_sql}
                   ORDER BY rank, e.created_at DESC LIMIT {size} OFFSET {}"#,
                page * size
            )
        } else {
            format!(
                r#"SELECT e.id, e.content_type, e.content, e.blob_path, e.origin, e.source_app,
                          e.pinned, e.group_name, e.secret, e.created_at, e.usage_count,
                          e.html IS NOT NULL AS has_html
                   FROM clip_entries AS e {where_sql}
                   ORDER BY e.pinned DESC, e.created_at DESC LIMIT {size} OFFSET {}"#,
                page * size
            )
        };

        let items: Vec<ClipEntry> = {
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            let rows = stmt
                .query_map(
                    rusqlite::params_from_iter(args.iter().map(|b| &**b)),
                    entry_from_row,
                )
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let has_more = items.len() as i64 == size;
        Ok(Page {
            items,
            has_more,
            total: None,
        })
    }

    pub fn pin(&self, id: &str, pinned: bool) -> Result<(), AppError> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE clip_entries SET pinned = ?2 WHERE id = ?1",
            params![id, pinned as i64],
        )
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<(), AppError> {
        let conn = self.conn.lock();
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
        drop(conn);
        // D-05：删记录即删 blob，明文不再永驻磁盘
        if let Some(b) = blob {
            self.remove_blob(&b, false);
        }
        Ok(())
    }

    pub fn clear(&self, keep_pinned: bool) -> Result<u32, AppError> {
        let (sel_sql, del_sql) = if keep_pinned {
            (
                "SELECT blob_path FROM clip_entries WHERE pinned = 0 AND blob_path IS NOT NULL",
                "DELETE FROM clip_entries WHERE pinned = 0",
            )
        } else {
            (
                "SELECT blob_path FROM clip_entries WHERE blob_path IS NOT NULL",
                "DELETE FROM clip_entries",
            )
        };
        let (blobs, n) = {
            let conn = self.conn.lock();
            let blobs: Vec<String> = conn
                .prepare(sel_sql)
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?
                .query_map([], |r| r.get(0))
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?
                .filter_map(|r| r.ok())
                .collect();
            let n = conn
                .execute(del_sql, [])
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            (blobs, n)
        };
        // D-05：清空 = 覆写删除后 unlink（历史含敏感明文，不留扇区可恢复残片）
        for b in blobs {
            self.remove_blob(&b, true);
        }
        Ok(n as u32)
    }

    /// 入栈（幂等：同一条目重复入栈不加深）；返回入栈后的栈深。
    /// 条目不存在 → 点名 CLIPBOARD_STACK_001（FK 兜底前先看一眼，错误消息才有指路价值）。
    pub fn stack_push(&self, id: &str) -> Result<u32, AppError> {
        let conn = self.conn.lock();
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM clip_entries WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        if exists == 0 {
            return Err(AppError::module(
                "CLIPBOARD_STACK_001",
                "条目不存在，未入栈",
                Some("请先在历史列表刷新后确认该记录仍在库中"),
            ));
        }
        conn.execute(
            "INSERT INTO paste_stack (entry_id, position)
             SELECT ?1, (SELECT COALESCE(MAX(position), 0) + 1 FROM paste_stack)
             WHERE NOT EXISTS (SELECT 1 FROM paste_stack WHERE entry_id = ?2)",
            params![id, id],
        )
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        let depth: i64 = conn
            .query_row("SELECT COUNT(*) FROM paste_stack", [], |r| r.get(0))
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(depth as u32)
    }

    /// 队列入栈顺序（position ASC）
    pub fn stack_list(&self) -> Result<Vec<String>, AppError> {
        let conn = self.conn.lock();
        self.stack_ids(&conn)
    }

    /// 已持锁时的顺序读取（entries_in_order 复用，避免对非重入 Mutex<Connection> 二次加锁）
    fn stack_ids(&self, conn: &Connection) -> Result<Vec<String>, AppError> {
        let mut stmt = conn
            .prepare("SELECT entry_id FROM paste_stack ORDER BY position ASC")
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))
    }

    /// 弹出队首（FIFO：按入栈顺序逐条投递，Ditto/CopyQ 同语义；旧 pop_stack 的
    /// `position DESC` 是 LIFO，与堆栈粘贴语义相反，本行起替换）
    pub fn stack_take_next(&self) -> Result<Option<String>, AppError> {
        let conn = self.conn.lock();
        let row: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, entry_id FROM paste_stack ORDER BY position ASC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        let Some((row_id, entry_id)) = row else {
            return Ok(None);
        };
        conn.execute("DELETE FROM paste_stack WHERE id = ?1", params![row_id])
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(Some(entry_id))
    }

    /// 把 id 移到目标下标（0 基，越界钳到队尾）：读全序 → 本地 splice → 事务内重写 position
    pub fn stack_move(&self, id: &str, to: usize) -> Result<(), AppError> {
        let mut order = self.stack_list()?;
        let from = order
            .iter()
            .position(|e| e == id)
            .ok_or_else(|| AppError::module("CLIPBOARD_STACK_001", "条目不在堆栈中", None))?;
        let item = order.remove(from);
        let to = to.min(order.len());
        order.insert(to, item);
        let conn = self.conn.lock();
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        for (i, entry_id) in order.iter().enumerate() {
            tx.execute(
                "UPDATE paste_stack SET position = ?2 WHERE entry_id = ?1",
                params![entry_id, i as i64],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        }
        tx.commit().map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(())
    }

    /// 移出堆栈（不删历史记录）；返回是否确有其项
    pub fn stack_remove(&self, id: &str) -> Result<bool, AppError> {
        let conn = self.conn.lock();
        let n = conn
            .execute("DELETE FROM paste_stack WHERE entry_id = ?1", params![id])
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(n > 0)
    }

    pub fn stack_clear(&self) -> Result<u32, AppError> {
        let conn = self.conn.lock();
        let n = conn
            .execute("DELETE FROM paste_stack", [])
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(n as u32)
    }

    /// 按给定顺序回填条目：已删条目静默跳过（FK CASCADE 已把它的堆栈行带走），
    /// 调用方在 DTO 侧如实少一行，不占位也不报错。
    pub fn entries_in_order(&self, ids: &[String]) -> Result<Vec<ClipEntry>, AppError> {
        let conn = self.conn.lock();
        let mut out = Vec::with_capacity(ids.len());
        let mut stmt = conn
            .prepare(
                "SELECT id, content_type, content, blob_path, origin, source_app,
                        pinned, group_name, secret, created_at, usage_count,
                        html IS NOT NULL AS has_html
                 FROM clip_entries WHERE id = ?1",
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        for id in ids {
            if let Some(entry) = stmt
                .query_map(params![id], entry_from_row)
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?
                .flatten()
                .next()
            {
                out.push(entry);
            }
        }
        Ok(out)
    }

    /// 图片入库：字节写 blob（{hash}.dib），主表存引用
    pub fn insert_image(
        &self,
        format: &str,
        width: u32,
        height: u32,
        bytes: &[u8],
        source_app: Option<&str>,
        origin: &str,
    ) -> Result<String, AppError> {
        let hash = content_hash_bytes(bytes);
        let conn = self.conn.lock();
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
                "UPDATE clip_entries SET created_at = ?2, usage_count = usage_count + 1,
                 origin = CASE WHEN ?3 = 'local' THEN 'local' ELSE origin END WHERE id = ?1",
                params![id, now_ms(), origin],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            return Ok(id);
        }
        let id = uuid::Uuid::now_v7().to_string();
        let blob = format!("{hash}.{format}");
        std::fs::write(self.blob_dir.join(&blob), bytes)
            .map_err(|e| err("CLIPBOARD_STORAGE_002", e))?;
        conn.execute(
            r#"INSERT INTO clip_entries
               (id, content_type, content, content_hash, blob_path, origin, source_app, pinned, group_name, secret, created_at)
               VALUES (?1, 'image', ?2, ?3, ?4, ?7, ?5, 0, NULL, 0, ?6)"#,
            params![id, format!("{width}x{height}"), hash, blob, source_app, now_ms(), origin],
        )
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(id)
    }

    /// 文件列表入库：路径列表拼接为 content（去重键）
    pub fn insert_files(
        &self,
        paths: &[std::path::PathBuf],
        source_app: Option<&str>,
        origin: &str,
    ) -> Result<String, AppError> {
        let content = paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        self.insert_typed(&content, "files", source_app, origin)
    }

    fn insert_typed(
        &self,
        content: &str,
        content_type: &'static str,
        source_app: Option<&str>,
        origin: &str,
    ) -> Result<String, AppError> {
        let hash = content_hash(content);
        let now = now_ms();
        let conn = self.conn.lock();
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
                "UPDATE clip_entries SET created_at = ?2, usage_count = usage_count + 1,
                 origin = CASE WHEN ?3 = 'local' THEN 'local' ELSE origin END WHERE id = ?1",
                params![id, now, origin],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            return Ok(id);
        }
        let id = uuid::Uuid::now_v7().to_string();
        conn.execute(
            r#"INSERT INTO clip_entries
               (id, content_type, content, content_hash, origin, source_app, pinned, group_name, secret, created_at)
               VALUES (?1, ?2, ?3, ?4, ?7, ?5, 0, NULL, 0, ?6)"#,
            params![id, content_type, content, hash, source_app, now, origin],
        )
        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(id)
    }

    /// 条目载荷（paste 用）：文本 / 图片原始字节
    pub fn get_payload(&self, id: &str) -> Result<Option<Payload>, AppError> {
        let conn = self.conn.lock();
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
                let Some(blob) = blob_path else {
                    return Ok(None);
                };
                let bytes = std::fs::read(self.blob_dir.join(&blob))
                    .map_err(|e| err("CLIPBOARD_STORAGE_002", e))?;
                let format = blob.rsplit('.').next().unwrap_or("dib").to_string();
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
                let Some(content) = content_opt else {
                    return Ok(None);
                };
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
        let conn = self.conn.lock();
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

        // 真实组名（分类器给出的 url/json/code/color 与用户自建名）与 secret 标记行
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
            // 组名是用户输入，可含逗号/空格/Unicode：整名计数（旧实现按 ',' 截首段，
            // 等于把 "a,b" 显示成 "a"）。伪键 text/files/secret/all 与同名用户组会并入
            // 同一计数桶，属既有展示语义，不做静默改名。
            counts.insert(g, serde_json::json!(n));
            total += n as u32;
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

    // ---- 分组数据面（docs/impl/09 §8.2 T-B3-4）----

    /// 手工设定单条目分组（`None` = 取消分组）。手工组名优先级最高：分类器只在插入
    /// 新行时写 group_name，此后任何去重命中都不覆写它。
    pub fn set_entry_group(&self, id: &str, group: Option<&str>) -> Result<(), AppError> {
        let group = group.map(sanitize_group_name).transpose()?;
        let conn = self.conn.lock();
        let n = conn
            .execute(
                "UPDATE clip_entries SET group_name = ?2 WHERE id = ?1",
                params![id, group],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        if n == 0 {
            return Err(AppError::module(
                "CLIPBOARD_GROUP_001",
                "条目不存在，未改分组",
                None,
            ));
        }
        Ok(())
    }

    /// 组名整体重命名，返影响行数。空名/超长在写库前拒（脏组名会以徽标形式长期驻留）。
    pub fn rename_group(&self, from: &str, to: &str) -> Result<u32, AppError> {
        let to = sanitize_group_name(to)?;
        let conn = self.conn.lock();
        let n = conn
            .execute(
                "UPDATE clip_entries SET group_name = ?2 WHERE group_name = ?1",
                params![from, to],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(n as u32)
    }

    /// 删除分组：组内条目回到未分组（记录本身不删），返影响行数
    pub fn delete_group(&self, name: &str) -> Result<u32, AppError> {
        let conn = self.conn.lock();
        let n = conn
            .execute(
                "UPDATE clip_entries SET group_name = NULL WHERE group_name = ?1",
                params![name],
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(n as u32)
    }

    /// 待采纳的分组建议（01§5-1：只列建议，永不自动落库）。
    /// 谓词三条件缺一不可：有建议 / 用户还没手工归组 / 没被忽略过。
    /// 敏感行走同一路径但 preview 由 secret 位遮蔽，密文与类别特征都不外泄。
    pub fn suggestions(&self, limit: u32) -> Result<Vec<SuggestionDto>, AppError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, content, secret, suggested_group, suggested_confidence
                 FROM clip_entries
                 WHERE suggested_group IS NOT NULL AND group_name IS NULL
                   AND suggestion_dismissed = 0
                 ORDER BY suggested_confidence DESC LIMIT ?1",
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        let rows = stmt
            .query_map(params![limit.min(500) as i64], |r| {
                let content: Option<String> = r.get(1)?;
                let secret: i64 = r.get(2)?;
                Ok(SuggestionDto {
                    entry_id: r.get(0)?,
                    preview: build_preview(&content.unwrap_or_default(), secret == 1),
                    suggested_group: r.get(3)?,
                    confidence: r.get::<_, Option<f64>>(4)?.unwrap_or_default() as f32,
                })
            })
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 采纳/忽略一条建议，返实际改动行数（幂等：重复采纳第二次不计数）。
    /// 采纳保留 suggested_group 供反悔与统计复算；忽略置位即永久静默。
    pub fn apply_suggestion(&self, ids: &[String], accept: bool) -> Result<u32, AppError> {
        let conn = self.conn.lock();
        let sql = if accept {
            "UPDATE clip_entries SET group_name = suggested_group
             WHERE id = ?1 AND suggested_group IS NOT NULL AND group_name IS NULL"
        } else {
            "UPDATE clip_entries SET suggestion_dismissed = 1 WHERE id = ?1"
        };
        let mut n = 0u32;
        for id in ids {
            n += conn
                .execute(sql, params![id])
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))? as u32;
        }
        Ok(n)
    }

    /// 统计卡：全部为库内聚合，blob 字节按文件实际大小累加（主表只存引用名）
    pub fn stats(&self) -> Result<StatsDto, AppError> {
        let conn = self.conn.lock();
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM clip_entries", [], |r| r.get(0))
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        let group_by = |sql: &str| -> Result<serde_json::Map<String, serde_json::Value>, AppError> {
            let mut stmt = conn
                .prepare(sql)
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            let mut m = serde_json::Map::new();
            for (k, v) in rows.filter_map(|r| r.ok()) {
                m.insert(k, serde_json::json!(v));
            }
            Ok(m)
        };
        let by_content_type = serde_json::Value::Object(group_by(
            "SELECT content_type, COUNT(*) FROM clip_entries GROUP BY content_type",
        )?);
        let by_group = serde_json::Value::Object(group_by(
            "SELECT COALESCE(group_name, '未分组'), COUNT(*) FROM clip_entries GROUP BY 1",
        )?);
        let top_source_apps = {
            let mut stmt = conn
                .prepare(
                    "SELECT source_app, COUNT(*) FROM clip_entries
                     WHERE source_app IS NOT NULL AND source_app <> ''
                     GROUP BY source_app ORDER BY COUNT(*) DESC, source_app LIMIT 8",
                )
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32))
                })
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            rows.filter_map(|r| r.ok()).collect::<Vec<_>>()
        };
        let inline_bytes: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(content)), 0) FROM clip_entries",
                [],
                |r| r.get(0),
            )
            .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
        let blob_names: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT blob_path FROM clip_entries WHERE blob_path IS NOT NULL")
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let blob_bytes: u64 = blob_names
            .iter()
            .filter_map(|n| std::fs::metadata(self.blob_dir.join(n)).ok())
            .map(|m| m.len())
            .sum();
        Ok(StatsDto {
            total: total as u32,
            by_content_type,
            by_group,
            top_source_apps,
            bytes_blob: inline_bytes.max(0) as u64 + blob_bytes,
        })
    }

    /// C9 清理：retention_days > 0 时按保留期，再按 max_entries 上限淘汰（置顶除外）。
    /// D-05：与手动删除共用"删记录 + 删 blob"实现，杜绝第二条泄漏路径。
    pub fn purge(&self, retention_days: u32, max_entries: u32) -> Result<u32, AppError> {
        let cutoff = now_ms() - (retention_days as i64) * 86_400_000;
        let (blobs_a, blobs_b, n_expired, n_overflow) = {
            let conn = self.conn.lock();
            let collect =
                |sql: &str, args: &[&dyn rusqlite::ToSql]| -> Result<Vec<String>, AppError> {
                    Ok(conn
                        .prepare(sql)
                        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?
                        .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                            r.get::<_, String>(0)
                        })
                        .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?
                        .filter_map(|r| r.ok())
                        .collect())
                };
            let blobs_a = if retention_days > 0 {
                collect(
                    "SELECT blob_path FROM clip_entries WHERE pinned = 0 AND created_at < ?1 AND blob_path IS NOT NULL",
                    &[&cutoff],
                )?
            } else {
                Vec::new()
            };
            let n_expired = if retention_days > 0 {
                conn.execute(
                    "DELETE FROM clip_entries WHERE pinned = 0 AND created_at < ?1",
                    params![cutoff],
                )
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?
            } else {
                0
            };
            let blobs_b = collect(
                r#"SELECT blob_path FROM clip_entries WHERE pinned = 0 AND id IN (
                     SELECT id FROM clip_entries WHERE pinned = 0
                     ORDER BY created_at DESC LIMIT -1 OFFSET ?1) AND blob_path IS NOT NULL"#,
                &[&max_entries],
            )?;
            let n_overflow = conn
                .execute(
                    r#"DELETE FROM clip_entries WHERE pinned = 0 AND id IN (
                         SELECT id FROM clip_entries WHERE pinned = 0
                         ORDER BY created_at DESC LIMIT -1 OFFSET ?1)"#,
                    params![max_entries],
                )
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            (blobs_a, blobs_b, n_expired, n_overflow)
        };
        for b in blobs_a.into_iter().chain(blobs_b) {
            self.remove_blob(&b, true);
        }
        Ok((n_expired + n_overflow) as u32)
    }

    /// D-05 启动期孤儿 blob GC：主表不再引用的文件覆写后清除（含删除失败的补偿）。
    /// 由模块 init 在 open 后调用一次。
    pub fn gc_orphan_blobs(&self) -> Result<u32, AppError> {
        let referenced: std::collections::HashSet<String> = {
            let conn = self.conn.lock();
            let mut stmt = conn
                .prepare("SELECT blob_path FROM clip_entries WHERE blob_path IS NOT NULL")
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
            let rows = stmt
                .query_map([], |r| r.get(0))
                .map_err(|e| err("CLIPBOARD_STORAGE_001", e))?
                .filter_map(|r| r.ok())
                .collect();
            rows
        };
        let mut removed = 0;
        for entry in
            std::fs::read_dir(&self.blob_dir).map_err(|e| err("CLIPBOARD_STORAGE_002", e))?
        {
            let path = entry.map_err(|e| err("CLIPBOARD_STORAGE_002", e))?.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
                continue;
            };
            if referenced.contains(&name) {
                continue;
            }
            tracing::warn!(file = %name, "发现孤儿 blob，覆写清除");
            self.remove_blob(&name, true);
            if !path.exists() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// 删 blob 文件（统一出口）。overwrite=true 时先全量覆写再 unlink，
    /// 使明文/密文不残留可恢复扇区；失败仅告警（下次启动 GC 补偿）。
    fn remove_blob(&self, name: &str, overwrite: bool) {
        let path = self.blob_dir.join(name);
        let result = (|| -> std::io::Result<()> {
            if overwrite {
                if let Ok(meta) = std::fs::metadata(&path) {
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new().write(true).open(&path)?;
                    let block = [0u8; 64 * 1024];
                    let mut left = meta.len();
                    while left > 0 {
                        let n = left.min(block.len() as u64) as usize;
                        f.write_all(&block[..n])?;
                        left -= n as u64;
                    }
                    f.flush()?;
                    f.sync_all()?;
                }
            }
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                r => r,
            }
        })();
        if let Err(e) = result {
            tracing::warn!(file = %path.display(), error = %e, "blob 清理失败，留待启动 GC 补偿");
        }
    }
}

/// 条目载荷
pub enum Payload {
    Text(String),
    Files(Vec<std::path::PathBuf>),
    Image {
        format: String,
        bytes: Vec<u8>,
    },
    /// 加密文本（Base64），由调用方经 CryptoPort 解密
    SecretB64(String),
}

fn content_hash(text: &str) -> String {
    content_hash_bytes(text.as_bytes())
}

/// 建议两列写入拆分：无建议时两列皆 NULL（NULL 与置信度 0.0 语义不同）
fn split_suggestion(s: Option<(&str, f32)>) -> (Option<&str>, Option<f32>) {
    s.map(|(g, c)| (Some(g), Some(c))).unwrap_or((None, None))
}

fn content_hash_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

use host_core::util::hex_lower as hex;

/// FTS5 语法转义：拆词并加前缀匹配，防止语法错误（docs/impl/02 C2 潜在问题）
fn fts_escape(q: &str) -> String {
    q.split_whitespace()
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// 分组过滤条件 →（SQL 片段, 绑定值）。红线：值一律走绑定——组名从 T-B3-4 起是
/// 用户可自取的任意 Unicode 文本，拼串等于把 SQL 语法交给一条被复制的字符串。
/// 片段内占位符按 `args` 的出现顺序编号（?1..?n），调用方追加条件时续号。
fn build_filters(q: &SearchQuery) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let Some(group) = q.group.as_deref().filter(|g| *g != "all") else {
        return (String::new(), Vec::new());
    };
    // 伪分组键与 group_counts 的桶语义一一对应（"text" = 未分组且非敏感的文本）；
    // 其余一律按真实组名匹配。旧实现把这两个桶的条件写成 `group_name = 'text'`，
    // 于是「文本」筛选永远筛不到它计数的那批行。
    match group {
        "secret" => ("e.secret = 1".to_string(), Vec::new()),
        "files" => ("e.content_type = 'files'".to_string(), Vec::new()),
        "text" => (
            "e.group_name IS NULL AND e.secret = 0 AND e.content_type = 'text'".to_string(),
            Vec::new(),
        ),
        name => (
            "e.group_name = ?1".to_string(),
            vec![Box::new(name.to_string()) as Box<dyn rusqlite::types::ToSql>],
        ),
    }
}

/// 组名边界校验：去首尾空白后非空且不超过 64 字符——组名会作为徽标长期展示，
/// 脏名比脏内容更难事后清理（空名尤其会在 SubNav 造出一个无法点选的幽灵分组）。
fn sanitize_group_name(name: &str) -> Result<String, AppError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AppError::module(
            "CLIPBOARD_GROUP_002",
            "分组名不能为空",
            Some("要取消分组请用删除分组或清空条目分组"),
        ));
    }
    if trimmed.chars().count() > 64 {
        return Err(AppError::module(
            "CLIPBOARD_GROUP_002",
            "分组名过长",
            Some("请缩短到 64 字符以内"),
        ));
    }
    Ok(trimmed.to_string())
}

fn build_preview(content: &str, secret: bool) -> String {
    if secret {
        return format!("[{}] 已加密存储", crate::types::SECRET_CATEGORY_LABEL);
    }
    let mut s: String = content.chars().take(200).collect();
    if content.chars().count() > 200 {
        s.push('…');
    }
    s
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
        let id1 = s.insert_row(&NewClip::new("第一条内容")).unwrap();
        let id2 = s.insert_row(&NewClip::new("第一条内容")).unwrap();
        assert_eq!(id1, id2, "相同内容应去重返回同一 id");
        let page = s.search(&SearchQuery::default()).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].usage_count, 1);
    }

    /// 缺陷⑧（09 §8.1）红线：重复复制已置顶条目只刷新鲜度与计数，置顶态必须保留
    #[test]
    #[allow(non_snake_case)]
    fn dedup_hit_keepsPinnedAndRefreshesRecency() {
        let s = open_temp("dedup_pin");
        let id = s
            .insert_row(&NewClip::new("置顶后又被复制").from_app("app"))
            .unwrap();
        s.pin(&id, true).unwrap();
        let before = s
            .search(&SearchQuery::default())
            .unwrap()
            .items
            .into_iter()
            .find(|e| e.id == id)
            .expect("插入后应可查到");
        assert!(before.pinned);
        assert_eq!(before.usage_count, 0);

        std::thread::sleep(std::time::Duration::from_millis(5));
        let again = s
            .insert_row(&NewClip::new("置顶后又被复制").from_app("app"))
            .unwrap();
        assert_eq!(again, id, "去重命中不得新增行");

        let page = s.search(&SearchQuery::default()).unwrap();
        assert_eq!(page.items.len(), 1, "去重只更新既有行");
        let after = page.items.into_iter().find(|e| e.id == id).unwrap();
        assert!(after.pinned, "重复复制不得静默取消置顶");
        assert_eq!(after.usage_count, 1);
        assert!(
            after.created_at > before.created_at,
            "created_at 应刷新鲜度（旧 {} 新 {}）",
            before.created_at,
            after.created_at
        );
    }

    #[test]
    fn fts_search_finds_text() {
        let s = open_temp("fts");
        s.insert_row(&NewClip::new("设计原则：Windows 原生优先"))
            .unwrap();
        s.insert_row(&NewClip::new("cargo build --release").group("code"))
            .unwrap();
        let q = SearchQuery {
            text: Some("原生".into()),
            ..Default::default()
        };
        let page = s.search(&q).unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.items[0].preview.contains("原生"));
        // FTS 语法注入防护
        let q2 = SearchQuery {
            text: Some("原\"生".into()),
            ..Default::default()
        };
        let _ = s.search(&q2).unwrap();
    }

    #[test]
    fn pin_clear_and_delete() {
        let s = open_temp("ops");
        let a = s.insert_row(&NewClip::new("A")).unwrap();
        let _ = s.insert_row(&NewClip::new("B")).unwrap();
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
        let id = s.insert_row(&NewClip::new(&big)).unwrap();
        let entry = &s.search(&SearchQuery::default()).unwrap().items[0];
        assert!(entry.blob_path.is_some(), ">64KB 内容应转 blob");
        let back = s.get_content(&id, |c| Ok(c.to_vec())).unwrap().unwrap();
        assert_eq!(back.len(), big.len());
    }

    // ---- D-05 blob 生命周期（安全红线，含负例） ----

    fn open_temp_dir(tag: &str) -> (ClipStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nf_clip_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let blobs = dir.join("blobs");
        (
            ClipStore::open(&dir.join("clipboard.db"), blobs.clone()).unwrap(),
            blobs,
        )
    }

    fn blob_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn big(text: &str) -> String {
        format!("{text}-{}", "x".repeat(BLOB_THRESHOLD + 10))
    }

    #[test]
    fn delete_entry_removes_blob_file() {
        let (s, blobs) = open_temp_dir("d05_del");
        let id = s.insert_row(&NewClip::new(&big("payload"))).unwrap();
        assert_eq!(blob_files(&blobs).len(), 1, "入库应写出 blob");
        s.delete(&id).unwrap();
        assert!(
            !blob_files(&blobs).iter().any(|f| f.ends_with(".txt")),
            "负例：删除条目后 blob 文件必须不存在（明文不得永驻磁盘）"
        );
    }

    #[test]
    fn clear_removes_all_blobs_and_keeps_pinned_blob() {
        let (s, blobs) = open_temp_dir("d05_clear");
        let a = s.insert_row(&NewClip::new(&big("a"))).unwrap();
        let _b = s.insert_row(&NewClip::new(&big("b"))).unwrap();
        s.pin(&a, true).unwrap();
        let removed = s.clear(true).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(
            blob_files(&blobs).len(),
            1,
            "keep_pinned：仅存置顶条目的 blob"
        );
        let retained = blob_files(&blobs)[0].clone();
        s.clear(false).unwrap();
        assert!(
            blob_files(&blobs).is_empty(),
            "负例：全量清空后 blobs 目录必须为空"
        );
        let _ = retained;
    }

    #[test]
    fn startup_gc_removes_orphans_only() {
        let (s, blobs) = open_temp_dir("d05_gc");
        let _live = s.insert_row(&NewClip::new(&big("live"))).unwrap();
        let live_file = blob_files(&blobs)[0].clone();
        std::fs::write(blobs.join("orphan-deadbeef.txt"), b"residual plaintext").unwrap();
        std::fs::write(blobs.join("orphan-image.dib"), b"residual image").unwrap();
        let removed = s.gc_orphan_blobs().unwrap();
        assert_eq!(removed, 2, "人为放置的孤儿 blob 应被启动 GC 清除");
        assert_eq!(blob_files(&blobs), vec![live_file], "被引用 blob 不得误删");
        // 重入安全：无孤儿时返回 0
        assert_eq!(s.gc_orphan_blobs().unwrap(), 0);
    }

    #[test]
    fn purge_max_entries_removes_blobs() {
        let (s, blobs) = open_temp_dir("d05_purge");
        for i in 0..5 {
            s.insert_row(&NewClip::new(&big(&format!("e{i}")))).unwrap();
        }
        assert_eq!(blob_files(&blobs).len(), 5);
        let n = s.purge(0, 2).unwrap();
        assert_eq!(n, 3);
        assert_eq!(
            blob_files(&blobs).len(),
            2,
            "上限淘汰必须同步删 blob（共用实现）"
        );
        assert_eq!(s.group_counts().unwrap()["all"].as_i64(), Some(2));
    }

    #[test]
    fn reopen_migrates_old_db_and_preserves_rows_blob_fts() {
        // M7：旧库 → 重开（rusqlite-migration to_latest 幂等）→ 行/blob/FTS 全部保留
        let dir = std::env::temp_dir().join(format!("nf_clip_reopen_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("clipboard.db");
        let blobs = dir.join("blobs");
        let payload = big("重开载荷");
        let (text_id, blob_id) = {
            let s = ClipStore::open(&db, blobs.clone()).unwrap();
            let t = s.insert_row(&NewClip::new("重开验证文本")).unwrap();
            let b = s.insert_row(&NewClip::new(&payload)).unwrap();
            (t, b)
        };
        let s = ClipStore::open(&db, blobs.clone()).unwrap();
        assert_eq!(
            s.search(&SearchQuery::default()).unwrap().items.len(),
            2,
            "重开后历史必须保留"
        );
        assert_eq!(
            s.get_content(&text_id, |c| Ok(c.to_vec())).unwrap(),
            Some("重开验证文本".into())
        );
        assert_eq!(
            s.get_content(&blob_id, |c| Ok(c.to_vec())).unwrap(),
            Some(payload),
            "blob 引用重开后可原样读回"
        );
        assert_eq!(blob_files(&blobs).len(), 1);
        assert_eq!(
            s.gc_orphan_blobs().unwrap(),
            0,
            "负例：重开后 GC 不得误删被引用 blob"
        );
        // FTS5 外部内容表索引跨重开可用
        assert_eq!(
            s.search(&SearchQuery {
                text: Some("重开验证".into()),
                ..Default::default()
            })
            .unwrap()
            .items
            .len(),
            1
        );
    }

    // ---- D-25 origin 归属（含旧库/脏值兼容） ----

    fn open_temp_with_db(tag: &str) -> (ClipStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nf_clip_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("clipboard.db");
        (ClipStore::open(&db, dir.join("blobs")).unwrap(), db)
    }

    #[test]
    fn origin_roundtrip_local_and_remote() {
        let s = open_temp("origin_rt");
        let rid = s
            .insert_row(
                &NewClip::new("remote-row")
                    .from_app("kvm:dev-x")
                    .origin("remote"),
            )
            .unwrap();
        s.insert_row(&NewClip::new("local-row")).unwrap();
        let page = s.search(&SearchQuery::default()).unwrap();
        assert_eq!(page.items.len(), 2);
        let remote = page.items.iter().find(|e| e.id == rid).unwrap();
        assert_eq!(remote.origin, "remote");
        assert_eq!(remote.source_app.as_deref(), Some("kvm:dev-x"));
        assert_eq!(
            page.items.iter().find(|e| e.id != rid).unwrap().origin,
            "local"
        );
    }

    #[test]
    fn dedup_promotes_local_only_never_remote_overwrite() {
        let s = open_temp("origin_promote");
        // 远端先入：origin=remote；本地重拷同内容 → 晋升 local
        s.insert_row(&NewClip::new("dup-text").origin("remote"))
            .unwrap();
        s.insert_row(&NewClip::new("dup-text")).unwrap();
        assert_eq!(
            s.search(&SearchQuery::default()).unwrap().items[0].origin,
            "local"
        );
        // 反向：本地先入，远端后到不得覆写
        let s2dir =
            std::env::temp_dir().join(format!("nf_clip_origin_demix_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&s2dir);
        let s2 = ClipStore::open(&s2dir.join("clipboard.db"), s2dir.join("blobs")).unwrap();
        let lid = s2.insert_row(&NewClip::new("keep-local")).unwrap();
        s2.insert_row(&NewClip::new("keep-local").origin("remote"))
            .unwrap();
        let row = s2
            .search(&SearchQuery::default())
            .unwrap()
            .items
            .into_iter()
            .find(|e| e.id == lid)
            .unwrap();
        assert_eq!(row.origin, "local", "远端事件不得放松既有归属");
        assert_eq!(row.usage_count, 1, "去重命中仍累加 usage");
    }

    #[test]
    fn dirty_or_legacy_origin_value_reads_as_local() {
        // 验收⑤：旧库无值/脏值一律兜底为合法枚举 'local'（前端 DTO 只有 local|remote）
        let (s, db) = open_temp_with_db("origin_dirty");
        let id = s
            .insert_row(&NewClip::new("dirty-origin").origin("remote"))
            .unwrap();
        {
            let raw = Connection::open(&db).unwrap();
            raw.execute(
                "UPDATE clip_entries SET origin = 'weird-legacy' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }
        let row = &s.search(&SearchQuery::default()).unwrap().items[0];
        assert_eq!(
            row.origin, "local",
            "非法 origin 值必须兜底 local 而非 panic"
        );
    }

    /// 红线方向钉：粘贴堆栈按入栈顺序逐条投递（FIFO）。反 LIFO 回归当场判红。
    #[test]
    #[allow(non_snake_case)]
    fn stack_takeNext_isFifoInPushOrder() {
        let s = open_temp("stack_fifo");
        let a = s.insert_row(&NewClip::new("stk-a")).unwrap();
        let b = s.insert_row(&NewClip::new("stk-b")).unwrap();
        let c = s.insert_row(&NewClip::new("stk-c")).unwrap();
        for id in [&a, &b, &c] {
            s.stack_push(id).unwrap();
        }
        assert_eq!(
            s.stack_list().unwrap(),
            vec![a.clone(), b.clone(), c.clone()],
            "position ASC 即入栈序"
        );
        assert_eq!(s.stack_take_next().unwrap().as_deref(), Some(a.as_str()));
        assert_eq!(s.stack_take_next().unwrap().as_deref(), Some(b.as_str()));
        assert_eq!(s.stack_take_next().unwrap().as_deref(), Some(c.as_str()));
        assert_eq!(s.stack_take_next().unwrap(), None, "空栈不得假成功");
    }

    /// 五写面往返：入栈幂等 + 列读 + 重排 + 移出 + 清空
    #[test]
    #[allow(non_snake_case)]
    fn stack_pushListMoveRemoveClear_roundtrip() {
        let s = open_temp("stack_rt");
        let a = s.insert_row(&NewClip::new("mv-a")).unwrap();
        let b = s.insert_row(&NewClip::new("mv-b")).unwrap();
        let c = s.insert_row(&NewClip::new("mv-c")).unwrap();
        assert_eq!(s.stack_push(&a).unwrap(), 1);
        assert_eq!(s.stack_push(&b).unwrap(), 2);
        assert_eq!(s.stack_push(&c).unwrap(), 3);
        // 重复入栈幂等：深度不变、顺序不变
        assert_eq!(s.stack_push(&a).unwrap(), 3);
        assert_eq!(
            s.stack_list().unwrap(),
            vec![a.clone(), b.clone(), c.clone()]
        );

        s.stack_move(&c, 0).unwrap();
        assert_eq!(
            s.stack_list().unwrap(),
            vec![c.clone(), a.clone(), b.clone()]
        );
        // 越界目标钳到队尾（不报错也不丢项）
        s.stack_move(&c, 99).unwrap();
        assert_eq!(
            s.stack_list().unwrap(),
            vec![a.clone(), b.clone(), c.clone()]
        );

        assert!(s.stack_remove(&b).unwrap());
        assert!(!s.stack_remove(&b).unwrap(), "二次移出不谎报成功");
        assert_eq!(s.stack_list().unwrap(), vec![a.clone(), c.clone()]);
        // 不存在的条目点名拒绝，且不改变栈
        let e = s.stack_push("no-such-id").unwrap_err();
        assert_eq!(e.code(), "CLIPBOARD_STACK_001");
        assert_eq!(s.stack_list().unwrap().len(), 2);
        assert_eq!(s.stack_clear().unwrap(), 2);
        assert!(s.stack_list().unwrap().is_empty());
    }

    /// 条目被删（FK CASCADE 带走堆栈行）后队列仍可读、顺序不丢、少一行如实
    #[test]
    #[allow(non_snake_case)]
    fn stack_list_skipsDeletedEntryWithoutLosingOrder() {
        let s = open_temp("stack_cascade");
        let a = s.insert_row(&NewClip::new("cas-a")).unwrap();
        let b = s.insert_row(&NewClip::new("cas-b")).unwrap();
        let c = s.insert_row(&NewClip::new("cas-c")).unwrap();
        for id in [&a, &b, &c] {
            s.stack_push(id).unwrap();
        }
        s.delete(&b).unwrap();
        let ids = s.stack_list().unwrap();
        assert_eq!(ids, vec![a.clone(), c.clone()], "CASCADE 后队列须自净");
        let entries = s.entries_in_order(&ids).unwrap();
        assert_eq!(
            entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec![a.as_str(), c.as_str()],
            "回填顺序 = 入栈顺序"
        );
        // 已删 id 直接进 entries_in_order：静默少一行，不报错不占位
        assert_eq!(s.entries_in_order(&[a, b, c]).unwrap().len(), 2);
    }

    // ---- T-B3-4 分组数据面（建议制 + 全绑定过滤 + 统计，docs/impl/09 §8.2）----

    fn group_of(s: &ClipStore, id: &str) -> Option<String> {
        s.search(&SearchQuery::default())
            .unwrap()
            .items
            .into_iter()
            .find(|e| e.id == id)
            .and_then(|e| e.group)
    }

    fn row_count(s: &ClipStore) -> usize {
        s.search(&SearchQuery::default()).unwrap().items.len()
    }

    /// 重命名与删除都是"整组重定向"：条目永不随之消失，只改指向
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-4）字面测试名优先于 rustc 命名惯例
    fn groupRename_andDelete_retargetRows() {
        let s = open_temp("grp_rename");
        let a = s
            .insert_row(&NewClip::new("rn-a").group("工作 项目"))
            .unwrap();
        let b = s
            .insert_row(&NewClip::new("rn-b").group("工作 项目"))
            .unwrap();
        let c = s.insert_row(&NewClip::new("rn-c")).unwrap();

        assert_eq!(
            s.rename_group("工作 项目", "私人 项目").unwrap(),
            2,
            "重命名作用于整组"
        );
        assert_eq!(group_of(&s, &a).as_deref(), Some("私人 项目"));
        assert_eq!(group_of(&s, &b).as_deref(), Some("私人 项目"));
        assert_eq!(group_of(&s, &c), None, "组外条目不得被波及");
        assert_eq!(
            s.rename_group("无人用的组", "y").unwrap(),
            0,
            "零命中如实返 0"
        );
        assert_eq!(s.delete_group("y").unwrap(), 0, "不存在的组删除影响 0 行");

        assert_eq!(s.rename_group("私人 项目", "合并目标").unwrap(), 2);
        assert_eq!(
            s.delete_group("合并目标").unwrap(),
            2,
            "删除分组只解除归属，返影响行数"
        );
        assert_eq!(row_count(&s), 3, "记录不随分组消失");
        assert_eq!(group_of(&s, &a), None);
        assert_eq!(group_of(&s, &c), None);

        // 脏名在写库前拒：空/纯空白会让 SubNav 长出点不动的幽灵分组
        assert_eq!(
            s.rename_group("合并目标", "   ").unwrap_err().code(),
            "CLIPBOARD_GROUP_002"
        );
        assert_eq!(row_count(&s), 3, "被拒的重命名不得改动任何行");
    }

    /// 组名是用户输入：Unicode、空格、逗号都要能原名往返（旧实现按 ',' 截首段显示）
    #[test]
    #[allow(non_snake_case)]
    fn entrySetGroup_userName_withUnicodeAndSpaces_roundtrip() {
        let s = open_temp("grp_rt");
        let id = s.insert_row(&NewClip::new("uni-payload")).unwrap();
        let name = "前端, UI 组件 · v2（临时）";
        s.set_entry_group(&id, Some(&format!("  {name}  ")))
            .expect("首尾空白应被裁掉而非拒绝");
        assert_eq!(group_of(&s, &id).as_deref(), Some(name));
        assert_eq!(
            s.group_counts().unwrap()[name].as_i64(),
            Some(1),
            "徽标按整名计数，逗号后不得截断"
        );

        s.set_entry_group(&id, None).unwrap();
        assert_eq!(group_of(&s, &id), None, "None = 取消分组");

        assert_eq!(
            s.set_entry_group("不存在", Some("g")).unwrap_err().code(),
            "CLIPBOARD_GROUP_001"
        );
        assert_eq!(
            s.set_entry_group(&id, Some("")).unwrap_err().code(),
            "CLIPBOARD_GROUP_002"
        );
        let too_long = "组".repeat(65);
        assert_eq!(
            s.set_entry_group(&id, Some(&too_long)).unwrap_err().code(),
            "CLIPBOARD_GROUP_002"
        );
        assert_eq!(group_of(&s, &id), None, "被拒的写不得留下半改状态");
    }

    /// 红线：组名走绑定后，SQL 注入串只是一个"匹配不到东西的普通组名"——
    /// 既不报错、也不吞行、表行数不变（拼串实现下这条查询要么语法错要么端出全表）
    #[test]
    #[allow(non_snake_case)]
    fn searchSyntax_groupInjection_matchesNothingAndTableIntact() {
        let s = open_temp("grp_inject");
        let plain = s.insert_row(&NewClip::new("inj-plain")).unwrap();
        s.insert_row(&NewClip::new("inj-grouped").group("正常组"))
            .unwrap();
        s.insert_encrypted("aW5qLXNlY3JldA==", None, None, None, "local")
            .unwrap();
        s.insert_files(
            &[std::path::PathBuf::from("C:/inj/a.txt")],
            Some("explorer.exe"),
            "local",
        )
        .unwrap();
        let before = row_count(&s);
        assert_eq!(before, 4);

        for evil in [
            "a' OR 1=1--",
            "\"; DROP TABLE clip_entries;--",
            "a' --",
            "1=1 OR group_name IS NOT NULL",
        ] {
            let page = s
                .search(&SearchQuery {
                    group: Some(evil.into()),
                    ..Default::default()
                })
                .unwrap();
            assert!(
                page.items.is_empty(),
                "注入串 {evil:?} 被当作组名匹配时不得命中任何行"
            );
        }
        assert_eq!(row_count(&s), before, "表与行数在注入尝试后完好");
        let fresh = s.insert_row(&NewClip::new("inj-after-attempt")).unwrap();
        assert_eq!(row_count(&s), before + 1, "库仍可写");
        s.delete(&fresh).unwrap();

        // 正对照：合法组名与四个伪键都命中它该命中的行（防"永远返回空"式假安全）
        let grouped = s
            .search(&SearchQuery {
                group: Some("正常组".into()),
                ..Default::default()
            })
            .unwrap()
            .items;
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].group.as_deref(), Some("正常组"));
        for (key, want) in [("all", 4), ("text", 1), ("secret", 1), ("files", 1)] {
            let n = s
                .search(&SearchQuery {
                    group: Some(key.into()),
                    ..Default::default()
                })
                .unwrap()
                .items
                .len();
            assert_eq!(n, want, "伪键 {key} 的筛选须与 group_counts 计数同语义");
        }
        assert_eq!(
            s.search(&SearchQuery {
                group: Some("text".into()),
                ..Default::default()
            })
            .unwrap()
            .items[0]
                .id,
            plain,
            "「文本」桶 = 未分组且非敏感的文本"
        );
    }

    /// 采纳：写入分组 + 离开队列 + 保留 suggested_group（供反悔与统计复算）
    #[test]
    #[allow(non_snake_case)]
    fn suggestionAccept_writesGroupAndClearsQueue() {
        let (s, db) = open_temp_with_db("sugg_accept");
        let id = s
            .insert_row(&NewClip::new("https://example.com/a").suggested(("url", 0.95)))
            .unwrap();
        let list = s.suggestions(50).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].entry_id, id);
        assert_eq!(list[0].suggested_group, "url");
        assert!((list[0].confidence - 0.95).abs() < 1e-6);
        assert!(list[0].preview.contains("example.com"));

        assert_eq!(
            s.apply_suggestion(std::slice::from_ref(&id), true).unwrap(),
            1
        );
        assert_eq!(group_of(&s, &id).as_deref(), Some("url"));
        assert!(
            s.suggestions(50).unwrap().is_empty(),
            "采纳后必须离开建议队列"
        );
        assert_eq!(
            s.apply_suggestion(std::slice::from_ref(&id), true).unwrap(),
            0,
            "重复采纳幂等，不谎报第二行"
        );

        let kept: Option<String> = {
            let raw = Connection::open(&db).unwrap();
            raw.query_row(
                "SELECT suggested_group FROM clip_entries WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            kept.as_deref(),
            Some("url"),
            "采纳只写 group_name，suggested_* 保留"
        );
    }

    /// 忽略 = 永久静默且不改分组；去重命中也不得让被忽略的建议复活
    #[test]
    #[allow(non_snake_case)]
    fn suggestionIgnore_dismissedSticky() {
        let s = open_temp("sugg_ignore");
        let id = s
            .insert_row(&NewClip::new("{\"a\": 1}").suggested(("json", 0.9)))
            .unwrap();
        assert_eq!(
            s.apply_suggestion(std::slice::from_ref(&id), false)
                .unwrap(),
            1
        );
        assert!(s.suggestions(50).unwrap().is_empty());
        assert_eq!(group_of(&s, &id), None, "红线：忽略不改分组");

        // 再次复制同内容 = 分类器重跑（带建议的去重命中）
        let again = s
            .insert_row(&NewClip::new("{\"a\": 1}").suggested(("json", 0.9)))
            .unwrap();
        assert_eq!(again, id, "去重命中同一行");
        assert!(
            s.suggestions(50).unwrap().is_empty(),
            "去重命中不得重置 suggestion_dismissed"
        );

        // 正对照：新条目照样进队列（证明这不是"队列永久关死"）
        s.insert_row(&NewClip::new("https://other.example").suggested(("url", 0.95)))
            .unwrap();
        assert_eq!(s.suggestions(50).unwrap().len(), 1);
    }

    /// 手工组名优先级最高：任何后续分类/去重都不覆写它
    #[test]
    #[allow(non_snake_case)]
    fn userGroup_survivesClassifierRerun() {
        let s = open_temp("grp_sticky");
        let text = "let handle = tokio::spawn(f());";
        let id = s.insert_row(&NewClip::new(text)).unwrap();
        s.set_entry_group(&id, Some("我的草稿")).unwrap();

        // 模拟 auto_group 开启后的重跑：group 与 suggested 都带着分类结果再来一遍
        s.insert_row(&NewClip {
            text,
            group: Some("code"),
            suggested: Some(("code", 0.9)),
            ..NewClip::new(text)
        })
        .unwrap();
        assert_eq!(
            group_of(&s, &id).as_deref(),
            Some("我的草稿"),
            "手工分组永不被分类器覆写"
        );
        assert_eq!(row_count(&s), 1, "同内容仍走去重");

        // 未手工分组的行才吃自动分组
        let other = s
            .insert_row(&NewClip::new("https://auto.example").group("url"))
            .unwrap();
        assert_eq!(group_of(&s, &other).as_deref(), Some("url"));
    }

    /// 统计卡：类型/分组/来源三向聚合 + 字节数（内联与 blob 分列同源）
    #[test]
    #[allow(non_snake_case)]
    fn stats_countsByTypeGroupAndSource() {
        let s = open_temp("stats");
        s.insert_row(&NewClip::new("st-a").group("A组").from_app("notepad.exe"))
            .unwrap();
        s.insert_row(&NewClip::new("st-b").from_app("notepad.exe"))
            .unwrap();
        s.insert_row(&NewClip::new("st-c").group("B组").from_app("chrome.exe"))
            .unwrap();
        s.insert_row(&NewClip::new("st-d")).unwrap();
        s.insert_files(
            &[
                std::path::PathBuf::from("C:/x.txt"),
                std::path::PathBuf::from("C:/y.txt"),
            ],
            Some("explorer.exe"),
            "local",
        )
        .unwrap();

        let st = s.stats().unwrap();
        assert_eq!(st.total, 5);
        let bt = &st.by_content_type;
        assert_eq!(bt["text"].as_i64(), Some(4));
        assert_eq!(bt["files"].as_i64(), Some(1));
        let bg = &st.by_group;
        assert_eq!(bg["A组"].as_i64(), Some(1));
        assert_eq!(bg["B组"].as_i64(), Some(1));
        assert_eq!(bg["未分组"].as_i64(), Some(3), "NULL 分组归未分组桶");

        assert!(
            st.top_source_apps
                .contains(&("notepad.exe".to_string(), 2u32)),
            "Top 来源按计数聚合：{:?}",
            st.top_source_apps
        );
        assert!(
            st.top_source_apps
                .iter()
                .all(|(app, _)| !app.trim().is_empty()),
            "无来源的行不进气味桶：{:?}",
            st.top_source_apps
        );

        let inline = 4 * 4 + "C:/x.txt\nC:/y.txt".len() as u64;
        assert_eq!(
            st.bytes_blob, inline,
            "无 blob 时字节数 = 内联 content 长度"
        );

        let big = "x".repeat(BLOB_THRESHOLD + 10);
        s.insert_row(&NewClip::new(&big)).unwrap();
        let st2 = s.stats().unwrap();
        assert_eq!(st2.total, 6);
        assert!(
            st2.bytes_blob >= inline + big.len() as u64,
            "blob 行按文件实际大小计入：{}",
            st2.bytes_blob
        );
    }

    /// 旧库重开幂等：三新列缺失时 PRAGMA 守卫补齐（schema_version 已记账，M::up 不会再跑），
    /// 旧行建议列为 NULL、dismissed 落默认 0，零回填零丢失
    #[test]
    #[allow(non_snake_case)]
    fn reopen_legacyDb_fillsSuggestedColumnsWithDefaults() {
        let dir = std::env::temp_dir().join(format!("nf_clip_legacy_sugg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("clipboard.db");
        let blobs = dir.join("blobs");

        let id = {
            let s = ClipStore::open(&db, blobs.clone()).unwrap();
            s.insert_row(&NewClip::new("legacy-row").group("旧组"))
                .unwrap()
        };
        // 合成 T-B3-4 之前的库形：列存在但 schema_version 已记满两版
        {
            let raw = Connection::open(&db).unwrap();
            for col in [
                "suggested_group",
                "suggested_confidence",
                "suggestion_dismissed",
            ] {
                raw.execute(&format!("ALTER TABLE clip_entries DROP COLUMN {col}"), [])
                    .unwrap();
            }
            let have: Vec<String> = raw
                .prepare("PRAGMA table_info(clip_entries)")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            assert!(
                !have.iter().any(|c| c == "suggested_group"),
                "前置条件不成立：旧列没删掉"
            );
        }

        let s = ClipStore::open(&db, blobs.clone()).unwrap();
        assert_eq!(
            group_of(&s, &id).as_deref(),
            Some("旧组"),
            "重开不得丢历史分组"
        );
        assert!(
            s.suggestions(50).unwrap().is_empty(),
            "旧行无建议：NULL 谓词生效而非报错"
        );
        let raw = Connection::open(&db).unwrap();
        let have: Vec<String> = raw
            .prepare("PRAGMA table_info(clip_entries)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for col in [
            "suggested_group",
            "suggested_confidence",
            "suggestion_dismissed",
        ] {
            assert!(have.iter().any(|c| c == col), "{col} 未补齐");
        }
        let dismissed: i64 = raw
            .query_row(
                "SELECT suggestion_dismissed FROM clip_entries WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dismissed, 0, "旧行按 DEFAULT 0 兜底，不回填不报错");
        drop(raw);

        // 补齐后写口可用 + 再重开一次仍幂等（守卫不重复 ALTER）
        let new = s
            .insert_row(&NewClip::new("post-legacy").suggested(("url", 0.95)))
            .unwrap();
        assert_eq!(s.suggestions(50).unwrap()[0].entry_id, new);
        drop(s);
        let s = ClipStore::open(&db, blobs).unwrap();
        assert_eq!(
            s.suggestions(50).unwrap().len(),
            1,
            "二次重开不得因重复 ALTER 报错"
        );
    }

    /// T-B3-8 同族守卫：`html` 列缺失但 schema_version 已记满三版的库（备份还原/手工修表）
    /// 重开时必须补齐，且旧行落 NULL = "没有 HTML"（不是报错、也不是空串冒充）。
    /// 二次重开只证明幂等：若守卫不查 `PRAGMA table_info` 就 ALTER，这里会以
    /// "duplicate column name" 直接炸开，比缺列更早暴露问题。
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-8）字面测试名优先于 rustc 命名惯例
    fn migration_reopen_legacyDb_addsHtmlColumnIdempotent() {
        let dir = std::env::temp_dir().join(format!("nf_clip_legacy_html_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("clipboard.db");
        let blobs = dir.join("blobs");

        let legacy_id = {
            let s = ClipStore::open(&db, blobs.clone()).unwrap();
            s.insert_row(&NewClip::new("旧库正文")).unwrap()
        };
        {
            let raw = Connection::open(&db).unwrap();
            raw.execute("ALTER TABLE clip_entries DROP COLUMN html", [])
                .unwrap();
            let have = html_column(&raw);
            assert!(
                !have.iter().any(|c| c == "html"),
                "前置条件不成立：html 列没删掉，本测就是空转"
            );
        }

        let s = ClipStore::open(&db, blobs.clone()).unwrap();
        assert!(
            html_column(&Connection::open(&db).unwrap())
                .iter()
                .any(|c| c == "html"),
            "重开后 html 列应由 PRAGMA 守卫补齐"
        );
        let page = s.search(&SearchQuery::default()).unwrap();
        let legacy = page.items.iter().find(|e| e.id == legacy_id).unwrap();
        assert!(!legacy.has_html, "旧行 NULL 语义是「没有 HTML」");
        assert_eq!(
            s.get_html(&legacy_id).unwrap(),
            None,
            "缺列补齐后的旧行经读口应得 None，不得报错也不得给空串"
        );

        // 补齐后写口可用
        let rich = s
            .insert_row(&NewClip::new("新库富文本").html("<p><b>新库富文本</b></p>"))
            .unwrap();
        assert!(
            s.search(&SearchQuery::default())
                .unwrap()
                .items
                .iter()
                .find(|e| e.id == rich)
                .unwrap()
                .has_html
        );
        assert_eq!(
            s.get_html(&rich).unwrap().as_deref(),
            Some("<p><b>新库富文本</b></p>")
        );
        drop(s);

        // 再重开一次：守卫不得重复 ALTER（duplicate column name 即红）
        let s = ClipStore::open(&db, blobs).unwrap();
        assert_eq!(
            s.get_html(&rich).unwrap().as_deref(),
            Some("<p><b>新库富文本</b></p>"),
            "二次重开不得丢已入库的 HTML"
        );
        assert_eq!(
            html_column(&Connection::open(&db).unwrap())
                .iter()
                .filter(|c| c.as_str() == "html")
                .count(),
            1,
            "html 列只应有一枚"
        );
    }

    fn html_column(conn: &Connection) -> Vec<String> {
        conn.prepare("PRAGMA table_info(clip_entries)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    // ---- T-B3-6 搜索语法（09 §8.2 行字面回归）----

    /// 取命中 id 集合并排序，便于"命中恰好是哪几行"式断言（本文件其余测试多用条数，
    /// 语法筛选要钉的是身份而非数量，否则正负例可能只是碰巧同数）。
    fn ids(page: Page<ClipEntry>) -> Vec<String> {
        sorted(page.items.into_iter().map(|e| e.id).collect())
    }
    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-6）字面测试名优先于 rustc 命名惯例
    fn searchSyntax_groupAndTypePrefixes_extractedFromText() {
        let s = open_temp("syntax_g_t");
        let a = s.insert_row(&NewClip::new("周报A").group("工作")).unwrap();
        let b = s
            .insert_files(&[std::path::PathBuf::from("d:/planB.txt")], None, "local")
            .unwrap();
        s.set_entry_group(&b, Some("工作")).unwrap();
        let c = s
            .insert_files(&[std::path::PathBuf::from("d:/planC.txt")], None, "local")
            .unwrap();
        let d = s.insert_row(&NewClip::new("周报D")).unwrap();

        // 两枚前缀都从 text 分离：若残渣留在 text 里，FTS 分支会被打开并滤空全部行
        assert_eq!(
            ids(s
                .search(&SearchQuery {
                    text: Some("group:工作 type:files".into()),
                    ..Default::default()
                })
                .unwrap()),
            vec![b.clone()],
            "组名 + 类型两条件 AND，且语法不进字面查询"
        );
        // 三枚单条件正对照：证明上面那枚不是"语法永远滤空"式假严格
        assert_eq!(
            ids(s
                .search(&SearchQuery {
                    text: Some("group:工作".into()),
                    ..Default::default()
                })
                .unwrap()),
            sorted(vec![a.clone(), b.clone()])
        );
        assert_eq!(
            ids(s
                .search(&SearchQuery {
                    text: Some("type:files".into()),
                    ..Default::default()
                })
                .unwrap()),
            sorted(vec![b.clone(), c])
        );
        assert_eq!(
            ids(s
                .search(&SearchQuery {
                    text: Some("周报".into()),
                    ..Default::default()
                })
                .unwrap()),
            sorted(vec![a, d])
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn searchSyntax_quotedGroupName_matchesSpaceBearingGroup() {
        let s = open_temp("syntax_quote");
        let spaced = s
            .insert_row(&NewClip::new("评审要点").group("工作 笔记"))
            .unwrap();
        s.insert_row(&NewClip::new("另一条")).unwrap();

        assert_eq!(
            ids(s
                .search(&SearchQuery {
                    text: Some(r#"group:"工作 笔记" 评审"#.into()),
                    ..Default::default()
                })
                .unwrap()),
            vec![spaced.clone()],
            "引号内空白属同一个组名"
        );
        // 负对照：组名按全等匹配，前缀式误切（把"工作"当组名）命中 0 行——
        // 若解析器丢了引号感知，上面那枚会以"工作"为组名而落到这条断言上判红。
        assert!(
            s.search(&SearchQuery {
                text: Some("group:工作 评审".into()),
                ..Default::default()
            })
            .unwrap()
            .items
            .is_empty(),
            "组名不做前缀匹配：误切出的「工作」不该命中「工作 笔记」"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn searchSyntax_unknownTypeValue_returnsNothingNotError() {
        let s = open_temp("syntax_unknown");
        s.insert_row(&NewClip::new("普通一行")).unwrap();
        s.insert_files(&[std::path::PathBuf::from("d:/x.txt")], None, "local")
            .unwrap();

        let res = s.search(&SearchQuery {
            text: Some("type:pdf".into()),
            ..Default::default()
        });
        assert!(
            res.is_ok(),
            "未知类型是查询条件不成立，不是命令失败：{:?}",
            res.err()
        );
        assert!(
            res.unwrap().items.is_empty(),
            "红线：不谎称有效、也不静默忽略该条件返回全部"
        );
        // 同一库、同一入口：去掉那枚条件就有行——零命中来自条件而非空库
        assert_eq!(
            s.search(&SearchQuery {
                text: Some("type:text".into()),
                ..Default::default()
            })
            .unwrap()
            .items
            .len(),
            1
        );
        assert_eq!(
            s.search(&SearchQuery::default()).unwrap().items.len(),
            2,
            "无条件时全库可见（正对照）"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn searchSyntax_bareWordWithColon_staysLiteralText() {
        let s = open_temp("syntax_url");
        let url = s
            .insert_row(&NewClip::new("http://a:b/c 参考链接"))
            .unwrap();
        let weird = s.insert_row(&NewClip::new("type://files 怪串")).unwrap();
        s.insert_row(&NewClip::new("group://工作 怪串二")).unwrap();

        assert_eq!(
            ids(s
                .search(&SearchQuery {
                    text: Some("http://a:b/c".into()),
                    ..Default::default()
                })
                .unwrap()),
            vec![url],
            "URL 型 token 按字面搜"
        );
        // `type:` 恰好是已知前缀，只有 `://` 守卫能救它：被吃成语法即 unknown_type → AND 0
        assert_eq!(
            ids(s
                .search(&SearchQuery {
                    text: Some("type://files".into()),
                    ..Default::default()
                })
                .unwrap()),
            vec![weird],
            "含 :// 的 token 不得进语法分支"
        );
        assert_eq!(
            ids(s
                .search(&SearchQuery {
                    text: Some("group://工作".into()),
                    ..Default::default()
                })
                .unwrap())
            .len(),
            1,
            "同上：group:// 型 token 也是字面文本"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn searchSyntax_repeatedPrefix_lastWins() {
        let s = open_temp("syntax_repeat");
        let g1 = s.insert_row(&NewClip::new("甲组一行").group("甲")).unwrap();
        let g2 = s.insert_row(&NewClip::new("乙组一行").group("乙")).unwrap();
        let f1 = s
            .insert_files(&[std::path::PathBuf::from("d:/r1.txt")], None, "local")
            .unwrap();
        let f2 = s
            .insert_files(&[std::path::PathBuf::from("d:/r2.txt")], None, "local")
            .unwrap();

        assert_eq!(
            ids(s
                .search(&SearchQuery {
                    text: Some("group:甲 group:乙".into()),
                    ..Default::default()
                })
                .unwrap()),
            vec![g2],
            "重复 group 前缀：后现覆盖先前"
        );
        assert_eq!(
            ids(s
                .search(&SearchQuery {
                    text: Some("type:text type:files".into()),
                    ..Default::default()
                })
                .unwrap()),
            sorted(vec![f1, f2]),
            "重复 type 前缀同样后覆盖先（两行 files 都在，两行 text 都不在）"
        );
        // 正对照：单枚前缀各自命中自己的那行（否则"last wins"只是"总是滤空"）
        assert_eq!(
            ids(s
                .search(&SearchQuery {
                    text: Some("group:甲".into()),
                    ..Default::default()
                })
                .unwrap()),
            vec![g1]
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn searchSyntax_contentTypeField_andSyntax_agreeOnSameResult() {
        let s = open_temp("syntax_field");
        s.insert_row(&NewClip::new("文本行")).unwrap();
        let f1 = s
            .insert_files(&[std::path::PathBuf::from("d:/f1.txt")], None, "local")
            .unwrap();
        let f2 = s
            .insert_files(&[std::path::PathBuf::from("d:/f2.txt")], None, "local")
            .unwrap();
        let files = sorted(vec![f1, f2]);

        let by_field = ids(s
            .search(&SearchQuery {
                content_type: Some("files".into()),
                ..Default::default()
            })
            .unwrap());
        let by_syntax = ids(s
            .search(&SearchQuery {
                text: Some("type:files".into()),
                ..Default::default()
            })
            .unwrap());
        let by_both = ids(s
            .search(&SearchQuery {
                text: Some("type:files".into()),
                content_type: Some("files".into()),
                ..Default::default()
            })
            .unwrap());
        assert_eq!(by_field, files, "显式字段面");
        assert_eq!(by_syntax, by_field, "语法与字段同值必须同结果");
        assert_eq!(by_both, by_syntax);

        // 冲突面：语法优先（当场敲进搜索框的那句才是本次意图），且不是"两条件 AND 成空"
        let conflict = ids(s
            .search(&SearchQuery {
                text: Some("type:files".into()),
                content_type: Some("text".into()),
                ..Default::default()
            })
            .unwrap());
        assert_eq!(conflict, by_syntax, "语法优先，不与字段求交");
    }
}
