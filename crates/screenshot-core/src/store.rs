//! 截图历史存取（docs/impl/03 P8 screenshot_history_list）
//!
//! 独立 SQLite 库文件（DESIGN O3：每模块一库）。

use parking_lot::Mutex;
use std::path::Path;

use host_core::error::AppError;
use rusqlite::{Connection, OptionalExtension};

use crate::types::{Annotation, HistoryQuery, Page, ShotItem};

use host_core::util::app_err as err;

/// 幂等补列守卫（T-B4-1）：`CREATE TABLE IF NOT EXISTS` 对已存在的旧表是空操作，
/// 新列只会由这条 ALTER 补上。先查 PRAGMA 再 ALTER 而不是硬吃"duplicate column"错误，
/// 因为 SQLite 对重复 ADD COLUMN 直接报错并中断 execute_batch——旧库重开会当场失败。
fn ensure_annotations_column(conn: &Connection) -> Result<(), AppError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(shots)")
        .map_err(|e| err("SCREENSHOT_STORE_002", e.to_string()))?;
    let have: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| err("SCREENSHOT_STORE_002", e.to_string()))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);
    if have.iter().any(|c| c == "annotations") {
        return Ok(());
    }
    conn.execute("ALTER TABLE shots ADD COLUMN annotations TEXT", [])
        .map_err(|e| err("SCREENSHOT_STORE_002", e.to_string()))?;
    Ok(())
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
                ocr_text TEXT,
                annotations TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_shots_created ON shots(created_ms DESC);",
        )
        .map_err(|e| err("SCREENSHOT_STORE_002", format!("建表失败: {e}")))?;
        ensure_annotations_column(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn insert(&self, item: &ShotItem) -> Result<(), AppError> {
        let conn = self.conn.lock();
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
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE shots SET ocr_text = ?2 WHERE id = ?1",
            rusqlite::params![id, text],
        )
        .map_err(|e| err("SCREENSHOT_STORE_003", e.to_string()))?;
        Ok(())
    }

    /// 关联标注矢量（T-B4-1"收即持久"）：`None` 写 NULL——空数组不落 `"[]"`，
    /// 这样历史列不需要区分"没画标注"和"标注为空"两种语义
    pub fn set_annotations(&self, id: &str, json: Option<&str>) -> Result<(), AppError> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE shots SET annotations = ?2 WHERE id = ?1",
            rusqlite::params![id, json],
        )
        .map_err(|e| err("SCREENSHOT_STORE_003", e.to_string()))?;
        Ok(())
    }

    /// 读回某条历史的标注（NULL → 空表；列内坏 JSON → 点名报错而非静默给空表，
    /// 静默会让一次磁盘损坏看起来像用户从没画过标注）
    pub fn annotations_of(&self, id: &str) -> Result<Vec<Annotation>, AppError> {
        let conn = self.conn.lock();
        let raw: Option<Option<String>> = conn
            .query_row(
                "SELECT annotations FROM shots WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| err("SCREENSHOT_STORE_004", e.to_string()))?;
        match raw.flatten() {
            Some(text) => {
                serde_json::from_str(&text).map_err(|e| err("SCREENSHOT_STORE_005", e.to_string()))
            }
            None => Ok(Vec::new()),
        }
    }

    /// 按 id 取单条历史（D-29 B0-2：面板取字节/再复制的前置查询；无则 None）
    pub fn get(&self, id: &str) -> Result<Option<ShotItem>, AppError> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT id, created_ms, width, height, file, ocr_text FROM shots WHERE id = ?1",
            rusqlite::params![id],
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
        .optional()
        .map_err(|e| err("SCREENSHOT_STORE_004", e.to_string()))
    }

    /// 按 id 删除一条历史，返回被删的那行（`None` = 库里没有这个 id）。
    ///
    /// 交回整行而非只交 `file`：`file` 列可空（"只复制不保存"的那一行从来没有落过盘），
    /// 把"行不存在"与"行在但没有文件"压成同一个 `None`，前者与后者就分不开了——
    /// 用户看得见那一行，程序却说"没有这条记录"，等于当着人的面撒谎。
    /// SELECT 与 DELETE 同持一把锁：中间不能有第二个写者把行换掉。
    pub fn delete(&self, id: &str) -> Result<Option<ShotItem>, AppError> {
        let conn = self.conn.lock();
        let item = conn
            .query_row(
                "SELECT id, created_ms, width, height, file, ocr_text FROM shots WHERE id = ?1",
                rusqlite::params![id],
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
            .optional()
            .map_err(|e| err("SCREENSHOT_STORE_004", e.to_string()))?;
        let Some(item) = item else {
            return Ok(None);
        };
        conn.execute("DELETE FROM shots WHERE id = ?1", rusqlite::params![id])
            .map_err(|e| err("SCREENSHOT_STORE_003", e.to_string()))?;
        Ok(Some(item))
    }

    pub fn list(&self, q: &HistoryQuery) -> Result<Page<ShotItem>, AppError> {
        let conn = self.conn.lock();
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
        Ok(Page {
            items,
            total,
            page,
            size,
        })
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

    #[test]
    fn shot_store_get_roundtrip_and_missing() {
        // D-29 B0-2 回归：按 id 存取一致；缺失 id 返回 None 而非 Err（面板据此显示"记录不存在"）
        let (_d, s) = tmp_store("shot_get");
        s.insert(&item("g1", 10)).unwrap();
        s.set_ocr_text("g1", "取回文本").unwrap();
        let got = s.get("g1").unwrap().expect("g1 应存在");
        assert_eq!(got.id, "g1");
        assert_eq!(got.file.as_deref(), Some("/x/g1.png"));
        assert_eq!(got.ocr_text.as_deref(), Some("取回文本"));
        assert!(s.get("nope").unwrap().is_none());
    }

    #[test]
    fn reopen_preserves_history_and_ocr_backfill() {
        // M7：旧库 → 重开（CREATE IF NOT EXISTS 幂等迁移）→ 数据保留
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reopen.db");
        {
            let s = ShotStore::open(&path).unwrap();
            s.insert(&item("keep", 7)).unwrap();
            s.set_ocr_text("keep", "重开保留文本").unwrap();
        }
        let s = ShotStore::open(&path).unwrap();
        let page = s.list(&HistoryQuery { page: 1, size: 10 }).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].id, "keep");
        assert_eq!(page.items[0].file.as_deref(), Some("/x/keep.png"));
        assert_eq!(page.items[0].ocr_text.as_deref(), Some("重开保留文本"));
    }

    /// 旧库列是否真的缺 annotations（PRAGMA 直查，供迁移测的前置断言用）
    fn columns_of(path: &Path) -> Vec<String> {
        let conn = Connection::open(path).unwrap();
        let mut stmt = conn.prepare("PRAGMA table_info(shots)").unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-1）字面测试名优先于 rustc 命名惯例
    fn migration_reopen_legacyShotDb_addsAnnotationsColumnIdempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        // 合成 T-B4-1 之前的库形：六列、无 annotations，且已有一条历史
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE shots(
                    id TEXT PRIMARY KEY,
                    created_ms INTEGER NOT NULL,
                    width INTEGER NOT NULL,
                    height INTEGER NOT NULL,
                    file TEXT,
                    ocr_text TEXT
                 );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO shots(id, created_ms, width, height, file, ocr_text)
                     VALUES('old', 5, 10, 10, '/x/old.png', NULL)",
                [],
            )
            .unwrap();
        }
        assert!(
            !columns_of(&path).contains(&"annotations".to_string()),
            "前置：合成旧库确实没有 annotations 列，否则本测的空洞无过"
        );
        // 首开：补列成功 + 旧行读回空标注（NULL 语义）
        let s = ShotStore::open(&path).unwrap();
        assert!(columns_of(&path).contains(&"annotations".to_string()));
        assert_eq!(s.annotations_of("old").unwrap().len(), 0);
        s.insert(&item("new", 9)).unwrap();
        s.set_annotations("new", Some("[]")).unwrap();
        // 二次重开：不得重复 ALTER（SQLite 对重复补列直接报错），且两行俱在
        drop(s);
        let s2 = ShotStore::open(&path).unwrap();
        assert_eq!(
            columns_of(&path)
                .iter()
                .filter(|c| *c == "annotations")
                .count(),
            1
        );
        assert_eq!(
            s2.list(&HistoryQuery { page: 1, size: 10 }).unwrap().total,
            2
        );
        assert_eq!(s2.annotations_of("new").unwrap().len(), 0);
    }

    #[test]
    fn annotations_roundtrip_json_and_null_clear() {
        let (_d, s) = tmp_store("shot_ann");
        s.insert(&item("a", 1)).unwrap();
        let ann = Annotation {
            kind: "rect".into(),
            color: "#fff".into(),
            width: 2.0,
            points: vec![(0.0, 1.0), (2.0, 3.0)],
            text: None,
            seq: None,
            layer: 7,
            locked: true,
            fill: true,
            alpha: 0.5,
        };
        s.set_annotations("a", Some(&serde_json::to_string(&[ann]).unwrap()))
            .unwrap();
        let back = s.annotations_of("a").unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].layer, 7);
        assert!(back[0].locked);
        assert!(back[0].fill);
        assert_eq!(back[0].alpha, 0.5);
        // 显式写 NULL = 清空（不是残留旧 JSON）
        s.set_annotations("a", None).unwrap();
        assert!(s.annotations_of("a").unwrap().is_empty());
        // 不存在的 id：空表而非报错
        assert!(s.annotations_of("ghost").unwrap().is_empty());
    }

    #[test]
    #[allow(non_snake_case)] // 驼峰名与同批四枚任务书字面名同形，便于按名索引
    fn annotations_of_corruptJson_errors_not_empty() {
        // 坏 JSON 不能读成"用户没画标注"——磁盘损坏与空标注必须是两件事
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.db");
        let s = ShotStore::open(&path).unwrap();
        s.insert(&item("a", 1)).unwrap();
        s.set_annotations("a", Some("{ not json")).unwrap();
        assert_eq!(
            s.annotations_of("a").unwrap_err().code(),
            "SCREENSHOT_STORE_005"
        );
        drop(s);
        // 正对照：同库另写合法 JSON 后读回非空（否则上面的"拒"可能是永远读不到东西）
        let s = ShotStore::open(&path).unwrap();
        s.set_annotations("a", Some("[]")).unwrap();
        assert!(s.annotations_of("a").unwrap().is_empty());
        let raw = Connection::open(&path)
            .unwrap()
            .query_row("SELECT annotations FROM shots WHERE id='a'", [], |r| {
                r.get::<_, Option<String>>(0)
            })
            .unwrap();
        assert_eq!(raw.as_deref(), Some("[]"));
    }
}
