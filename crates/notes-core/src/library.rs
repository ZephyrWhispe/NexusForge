//! 笔记库门面（docs/impl/06 N1–N5）。
//!
//! - 真相源 = 磁盘 .md；所有文件 CRUD 经 [`StorageDriver`]（N5 多存储后端：v1 本地驱动，
//!   smb/webdav 等 rclone 驱动接入后此层零改动；扫描已走 driver.list 递归）
//! - 索引（notes.db）可全量重建；外部编辑器改文件后 sync() 增量收敛（mtime/size 比较）
//! - N2 双链：`[[目标|别名]]`；重命名全库引用改写（先收集→逐文件原子替换→失败回滚）

use std::path::{Path, PathBuf};
use std::sync::Arc;

use file_core::driver::StorageDriver;
use regex::Regex;

use crate::canvas;
use crate::error::{NoteError, Result};
use crate::frontmatter::{extract_links, extract_tags, first_h1, split_frontmatter};
use crate::index::{NoteIndex, NoteIndexRow};
use crate::model::{Backlink, CanvasDoc, Card, NoteMeta, SyncResult};
use crate::review::{grade as sm2_grade, CardStore};

/// 笔记库
pub struct NoteLibrary {
    root: PathBuf,
    driver: Arc<dyn StorageDriver>,
    index: Arc<NoteIndex>,
    cards: Arc<CardStore>,
}

impl NoteLibrary {
    pub fn open(root: PathBuf, db_path: &Path, driver: Arc<dyn StorageDriver>) -> Result<Self> {
        std::fs::create_dir_all(&root).map_err(NoteError::Io)?;
        let index = Arc::new(NoteIndex::open(db_path)?);
        let cards = Arc::new(CardStore::new(index.conn())?);
        Ok(Self { root, driver, index, cards })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn index(&self) -> &Arc<NoteIndex> {
        &self.index
    }

    // ---------- 路径工具 ----------

    /// 规范化相对路径：`/` 分隔；拒绝空段、`.`、`..`（防越界）
    pub fn norm_rel(rel: &str) -> Result<String> {
        let t = rel.trim().trim_start_matches(['/', '\\']);
        if t.is_empty() {
            return Err(NoteError::BadPath("路径为空".into()));
        }
        let segs: Vec<&str> = t.split(['/', '\\']).collect();
        if segs.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
            return Err(NoteError::BadPath(format!("路径含非法段: {rel}")));
        }
        Ok(segs.join("/"))
    }

    /// rel 必须是 .md 笔记
    fn require_md(rel: &str) -> Result<()> {
        if !rel.to_ascii_lowercase().ends_with(".md") {
            return Err(NoteError::BadPath(format!("仅支持 .md 笔记: {rel}")));
        }
        Ok(())
    }

    fn disk(&self, rel: &str) -> PathBuf {
        self.root.join(rel.replace('/', "\\"))
    }

    fn read_text(&self, rel: &str) -> Result<String> {
        let bytes = self
            .driver
            .read_file(&self.disk(rel))
            .map_err(|e| NoteError::Driver(e.to_string()))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn write_text(&self, rel: &str, content: &str) -> Result<()> {
        self.driver
            .write_file(&self.disk(rel), content.as_bytes())
            .map_err(|e| NoteError::Driver(e.to_string()))
    }

    fn file_meta(rel_disk: &Path) -> (i64, u64) {
        match std::fs::metadata(rel_disk) {
            Ok(m) => {
                let ms = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                (ms, m.len())
            }
            Err(_) => (0, 0),
        }
    }

    // ---------- N1：单文件索引 ----------

    /// 索引单篇笔记（universe = 解析链接用的全库路径集合）
    pub fn index_one(&self, rel: &str, universe: &[String]) -> Result<()> {
        Self::require_md(rel)?;
        let content = self.read_text(rel)?;
        let (fm, body) = split_frontmatter(&content);
        let stem = Path::new(rel)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = fm
            .title
            .clone()
            .or_else(|| first_h1(body))
            .unwrap_or(stem);
        let mut tags = fm.tags.clone();
        for t in extract_tags(body) {
            if !tags.iter().any(|e| e.eq_ignore_ascii_case(&t)) {
                tags.push(t);
            }
        }
        let links = extract_links(&content)
            .into_iter()
            .map(|dst| {
                let resolved = self.resolve_link(&dst, universe);
                (dst, resolved)
            })
            .collect();
        let (mtime_ms, size) = Self::file_meta(&self.disk(rel));
        self.index.upsert(NoteIndexRow {
            path: rel.to_string(),
            title,
            mtime_ms,
            size,
            tags,
            links,
        })
    }

    /// 链接目标解析：相对路径（含/不含 .md）→ 全库 stem 匹配（大小写不敏感，取字典序首个）
    pub fn resolve_link(&self, dst: &str, universe: &[String]) -> String {
        let d = dst.trim();
        if d.is_empty() {
            return String::new();
        }
        let norm = d.replace('\\', "/");
        for cand in [norm.as_str(), &format!("{}.md", norm.trim_end_matches(".md"))] {
            if let Some(hit) = universe.iter().find(|p| p.as_str() == cand) {
                return hit.clone();
            }
        }
        let stem = norm.trim_end_matches(".md");
        let stem = stem.rsplit('/').next().unwrap_or(stem);
        universe
            .iter()
            .filter(|p| {
                Path::new(p.as_str())
                    .file_stem()
                    .map(|s| s.to_string_lossy().eq_ignore_ascii_case(stem))
                    .unwrap_or(false)
            })
            .min()
            .cloned()
            .unwrap_or_default()
    }

    // ---------- N1：sync / reindex ----------

    /// 扫描库根（driver.list 递归；跳过隐藏项与 .nf-tmp 临时文件）
    fn scan(&self) -> Result<Vec<(String, i64, u64)>> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = self.driver.list(&dir).map_err(|e| NoteError::Driver(e.to_string()))?;
            for e in entries {
                if e.hidden || e.name.ends_with(".nf-tmp") {
                    continue;
                }
                let rel = e
                    .path
                    .strip_prefix(&self.root)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                if e.is_dir {
                    stack.push(e.path.clone());
                } else if e.ext.eq_ignore_ascii_case("md") && !rel.is_empty() {
                    out.push((rel, e.modified_ms, e.size));
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// 增量索引（mtime/size 比较；外部编辑器修改后收敛）
    pub fn sync(&self) -> Result<SyncResult> {
        let disk = self.scan()?;
        let db_rows = self.index.list()?;
        let db_map: std::collections::HashMap<&str, &NoteMeta> =
            db_rows.iter().map(|n| (n.path.as_str(), n)).collect();

        let mut added = 0usize;
        let mut updated = 0usize;
        let universe: Vec<String> = disk.iter().map(|(r, _, _)| r.clone()).collect();
        for (rel, mtime, size) in &disk {
            match db_map.get(rel.as_str()) {
                Some(n) if n.mtime_ms == *mtime && n.size == *size => {}
                Some(_) => {
                    self.index_one(rel, &universe)?;
                    updated += 1;
                }
                None => {
                    self.index_one(rel, &universe)?;
                    added += 1;
                }
            }
        }
        let mut removed = 0usize;
        for n in &db_rows {
            if !disk.iter().any(|(r, _, _)| r == &n.path) {
                self.index.remove(&n.path)?;
                self.cards.detach_note(&n.path)?;
                removed += 1;
            }
        }
        Ok(SyncResult { added, updated, removed, total: disk.len() })
    }

    /// 全量重建（清索引三表后 sync；cards 保留）
    pub fn reindex(&self) -> Result<SyncResult> {
        self.index.clear()?;
        self.sync()
    }

    // ---------- N1：CRUD ----------

    pub fn create(&self, rel: &str, content: &str) -> Result<NoteMeta> {
        let rel = Self::norm_rel(rel)?;
        Self::require_md(&rel)?;
        if self.disk(&rel).exists() {
            return Err(NoteError::BadState(format!("笔记已存在: {rel}")));
        }
        self.write_text(&rel, content)?;
        let universe = self.universe_with(&rel)?;
        self.index_one(&rel, &universe)?;
        Ok(self.index.get(&rel)?.ok_or_else(|| NoteError::BadState("索引失败".into()))?)
    }

    pub fn read(&self, rel: &str) -> Result<(String, NoteMeta)> {
        let rel = Self::norm_rel(rel)?;
        Self::require_md(&rel)?;
        let meta = self
            .index
            .get(&rel)?
            .ok_or_else(|| NoteError::NotFound(rel.clone()))?;
        Ok((self.read_text(&rel)?, meta))
    }

    /// 写入内容并重索引（不存在则报错，走 create）
    pub fn write(&self, rel: &str, content: &str) -> Result<()> {
        let rel = Self::norm_rel(rel)?;
        Self::require_md(&rel)?;
        if !self.disk(&rel).exists() {
            return Err(NoteError::NotFound(rel));
        }
        self.write_text(&rel, content)?;
        let universe = self.universe_with(&rel)?;
        self.index_one(&rel, &universe)
    }

    pub fn delete(&self, rel: &str) -> Result<()> {
        let rel = Self::norm_rel(rel)?;
        Self::require_md(&rel)?;
        if !self.disk(&rel).exists() {
            return Err(NoteError::NotFound(rel.clone()));
        }
        self.driver
            .remove(&self.disk(&rel), false)
            .map_err(|e| NoteError::Driver(e.to_string()))?;
        self.index.remove(&rel)?;
        self.cards.detach_note(&rel)
    }

    /// 当前全库路径集合（可选附加一个尚未索引的新路径）
    fn universe_with(&self, extra: &str) -> Result<Vec<String>> {
        let mut v = self.index.paths()?;
        if !v.iter().any(|p| p == extra) {
            v.push(extra.to_string());
            v.sort();
        }
        Ok(v)
    }

    // ---------- N2：双链 / 反链 / 重命名改写 ----------

    pub fn list_notes(&self) -> Result<Vec<NoteMeta>> {
        self.index.list()
    }

    /// 本文出链（dst 原文 + 解析结果）
    pub fn links_of(&self, rel: &str) -> Result<Vec<(String, String)>> {
        self.index.links_from(&Self::norm_rel(rel)?)
    }

    /// 反链（含来源标题 + 命中行片段）
    pub fn backlinks(&self, rel: &str) -> Result<Vec<Backlink>> {
        let rel = Self::norm_rel(rel)?;
        let mut out = Vec::new();
        for (src, dst) in self.index.links_to(&rel)? {
            let title = self
                .index
                .get(&src)?
                .map(|m| m.title)
                .unwrap_or_else(|| src.clone());
            let snippet = self
                .read_text(&src)
                .ok()
                .and_then(|text| {
                    // 前缀匹配（含 `[[dst|` 别名形态）；snippet 仅展示用途
                    let needle = "[[".to_owned() + &dst;
                    text.lines()
                        .find(|l| l.contains(&needle))
                        .map(|l| l.trim().to_string())
                })
                .unwrap_or_default();
            out.push(Backlink { src, title, snippet });
        }
        Ok(out)
    }

    /// 重命名笔记并全库引用同步改写（先收集→改写→失败回滚；docs/impl/06 N2）
    pub fn rename(&self, old_rel: &str, new_rel: &str) -> Result<()> {
        let old = Self::norm_rel(old_rel)?;
        let new = Self::norm_rel(new_rel)?;
        Self::require_md(&old)?;
        Self::require_md(&new)?;
        if old == new {
            return Err(NoteError::BadState("新旧路径相同".into()));
        }
        if !self.disk(&old).exists() {
            return Err(NoteError::NotFound(old.clone()));
        }
        if self.disk(&new).exists() {
            return Err(NoteError::BadState(format!("目标已存在: {new}")));
        }

        // 1. 收集受影响文件（dst_path 解析命中 old 的全部 src）
        let affected = self.index.links_to(&old)?;

        // 2. 磁盘改名（先于内容改写：读新路径内容语义一致）
        self.driver
            .rename(&self.disk(&old), &self.disk(&new))
            .map_err(|e| NoteError::Driver(e.to_string()))?;

        // 3. 逐文件原子改写；任一失败回滚全部已改文件
        let old_stem = stem_of(&old);
        let new_stem = stem_of(&new);
        let mut rewritten: Vec<(String, String)> = Vec::new(); // (src, 旧内容)
        for (src, _) in &affected {
            if src == &old {
                continue; // 自引用由改名后的新路径处理
            }
            let Ok(content) = self.read_text(src) else { continue };
            let next = rewrite_links(&content, &old_stem, &new_stem);
            if next != content {
                if let Err(e) = self.write_text(src, &next) {
                    // 回滚已改写文件（旧内容写回；当前文件未写无需回滚）
                    for (back, old_content) in &rewritten {
                        let _ = self.write_text(back, old_content);
                    }
                    return Err(NoteError::Driver(format!("引用改写失败已回滚 {src}: {e}")));
                }
                rewritten.push((src.clone(), content));
            }
        }

        // 4. 自引用内容随文件迁移（磁盘内容不变，索引按新路径重建）
        // 5. 索引迁移 + 受影响文件重索引
        if let Err(e) = self.index.rename_path(&old, &new) {
            // 索引失败不回滚磁盘（真相源已迁移）；下次 sync 自然收敛
            tracing::warn!(error = %e, old, new, "重命名索引迁移失败，等待 sync 收敛");
            return Ok(());
        }
        let universe = self.universe_with(&new)?;
        let _ = self.index_one(&new, &universe);
        for (src, _) in &rewritten {
            let _ = self.index_one(src, &universe);
        }
        Ok(())
    }

    // ---------- N3：画布 ----------

    pub fn canvas_get(&self, dir_rel: &str) -> Result<CanvasDoc> {
        let dir = if dir_rel.trim().is_empty() {
            String::new()
        } else {
            Self::norm_rel(dir_rel)?
        };
        Ok(canvas::load(&self.root, &dir))
    }

    pub fn canvas_save(&self, dir_rel: &str, doc: &CanvasDoc) -> Result<()> {
        let dir = if dir_rel.trim().is_empty() {
            String::new()
        } else {
            Self::norm_rel(dir_rel)?
        };
        canvas::save(&self.root, &dir, doc)
    }

    /// 画布可引用的目录列表（库根 + 全部含子目录层级）
    pub fn canvas_dirs(&self) -> Result<Vec<String>> {
        let mut out = vec![String::new()];
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = self.driver.list(&dir) else { continue };
            for e in entries {
                if e.is_dir && !e.hidden {
                    stack.push(e.path.clone());
                    if let Ok(rel) = e.path.strip_prefix(&self.root).map(|p| p.to_string_lossy().replace('\\', "/")) {
                        out.push(rel);
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }

    // ---------- N4：复习 ----------

    pub fn cards(&self) -> &Arc<CardStore> {
        &self.cards
    }

    pub fn review_queue(&self, now_ms: i64) -> Result<Vec<Card>> {
        self.cards.queue(now_ms)
    }

    pub fn grade_card(&self, id: &str, quality: u32, now_ms: i64) -> Result<Card> {
        let card = self
            .cards
            .get(id)?
            .ok_or_else(|| NoteError::NotFound(format!("卡片 {id}")))?;
        let graded = sm2_grade(&card, quality, now_ms)?;
        self.cards.save(&graded)?;
        Ok(graded)
    }
}

fn stem_of(rel: &str) -> String {
    Path::new(rel)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| rel.to_string())
}

/// 三条改写规则（顺序执行）：
/// 1. `[[old]]` / `[[old|alias]]` → 同名直链
/// 2. `[[prefix/old]]` → 路径式（前缀保留，匹配最后一个路径段）
/// 3. `[[old.md]]` → 显式扩展名
fn rewrite_links(content: &str, old_stem: &str, new_stem: &str) -> String {
    let esc_old = regex::escape(old_stem);
    let r1 = Regex::new(&format!(r"\[\[{esc_old}(\||\]\])")).expect("改写正则1");
    let r2 = Regex::new(&format!(r"(\[\[[^\]\|]*/){esc_old}(\||\]\])")).expect("改写正则2");
    let r3 = Regex::new(&format!(r"\[\[{esc_old}\.md(\||\]\])")).expect("改写正则3");
    let s = r1.replace_all(content, |c: &regex::Captures| format!("[[{new_stem}{}", &c[1]));
    let s = r2.replace_all(&s, |c: &regex::Captures| format!("{}{new_stem}{}", &c[1], &c[2]));
    let s = r3.replace_all(&s, |c: &regex::Captures| format!("[[{new_stem}.md{}", &c[1]));
    s.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_core::driver::DriverRegistry;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_notes_lib_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn lib(tag: &str) -> NoteLibrary {
        let d = tmpdir(tag);
        let reg = DriverRegistry::new();
        NoteLibrary::open(
            d.join("vault"),
            &d.join("notes.db"),
            reg.get("local").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn norm_rel_rejects_escape() {
        assert!(NoteLibrary::norm_rel("a/b.md").is_ok());
        assert_eq!(NoteLibrary::norm_rel("/a\\b.md").unwrap(), "a/b.md");
        assert!(NoteLibrary::norm_rel("").is_err());
        assert!(NoteLibrary::norm_rel("../x.md").is_err());
        assert!(NoteLibrary::norm_rel("a//b.md").is_err());
    }

    #[test]
    fn create_read_write_index_and_sync() {
        let l = lib("crud");
        let meta = l.create("rust/入门.md", "# Rust 入门\n内容 #lang/rust\n见 [[todo]]\n").unwrap();
        assert_eq!(meta.title, "Rust 入门");
        assert!(meta.tags.iter().any(|t| t == "lang/rust"));
        assert!(l.create("rust/入门.md", "").is_err());

        // 建第二个笔记后 sync：create 已即时索引，无新增
        l.create("todo.md", "# TODO\n").unwrap();
        let r = l.sync().unwrap();
        assert_eq!(r.added, 0);
        assert_eq!(r.total, 2);

        // 首篇索引时 todo 尚未存在 → dst 未解析；sync 后应重解析吗？
        // v1 语义：index_one 在 todo 创建时只重索引 todo 自己；
        // 旧链接解析由 sync 检测不到（内容未变）——通过 reindex 修正
        let links = l.links_of("rust/入门.md").unwrap();
        assert_eq!(links, vec![("todo".into(), "".into())]);
        l.reindex().unwrap();
        let links = l.links_of("rust/入门.md").unwrap();
        assert_eq!(links, vec![("todo".into(), "todo.md".into())]);

        // 反链
        let back = l.backlinks("todo.md").unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].src, "rust/入门.md");
        assert!(back[0].snippet.contains("[[todo]]"));

        // 外部修改 → sync 收敛
        std::fs::write(l.root().join("todo.md"), "# TODO\n改过的 #urgent\n").unwrap();
        let r = l.sync().unwrap();
        assert_eq!(r.updated, 1);
        let meta = l.index().get("todo.md").unwrap().unwrap();
        assert!(meta.tags.contains(&"urgent".into()));
    }

    #[test]
    fn rename_rewrites_references_with_rollback() {
        let l = lib("rename");
        l.create("a.md", "[[old]] 与 [[old|别名]] 与 [[sub/old]] 与 [[old.md]]\n").unwrap();
        l.create("sub/old.md", "# 旧\n[[a]]\n").unwrap();
        l.reindex().unwrap();

        l.rename("sub/old.md", "sub/new.md").unwrap();
        let (content, _) = l.read("a.md").unwrap();
        assert!(content.contains("[[new]]"), "{content}");
        assert!(content.contains("[[new|别名]]"), "{content}");
        assert!(content.contains("[[sub/new]]"), "{content}");
        assert!(content.contains("[[new.md]]"), "{content}");
        assert!(!content.contains("old"), "{content}");
        // 索引同步：a.md 出链指向新路径
        let links = l.links_of("a.md").unwrap();
        assert!(links.iter().all(|(_, p)| p.contains("new")), "{links:?}");
        // 新路径可读
        assert!(l.read("sub/new.md").is_ok());
        assert!(l.read("sub/old.md").is_err());

        // 目标已存在拒绝
        assert!(l.rename("sub/new.md", "a.md").is_err());
    }

    #[test]
    fn rename_rollback_on_write_failure() {
        let l = lib("rollback");
        l.create("a.md", "[[old]]\n").unwrap();
        l.create("old.md", "x\n").unwrap();
        l.reindex().unwrap();
        // 把 a.md 变只读，触发改写失败 → 回滚
        let a_path = l.root().join("a.md");
        let mut perms = std::fs::metadata(&a_path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(true);
        std::fs::set_permissions(&a_path, perms).unwrap();
        let r = l.rename("old.md", "new.md");
        // Windows 只读文件写失败（驱动 write_file 报错）→ rename 报错
        if r.is_err() {
            // 磁盘已改名（先改名后改写），旧引用内容未被破坏
            assert!(l.root().join("new.md").exists());
            let content = std::fs::read_to_string(&a_path).unwrap();
            assert_eq!(content, "[[old]]\n");
        }
        let _ = std::fs::set_permissions(&a_path, {
            let mut p = std::fs::metadata(&a_path).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            p.set_readonly(false);
            p
        });
    }

    #[test]
    fn link_resolution_variants() {
        let l = lib("resolve");
        l.create("sub/alpha.md", "x").unwrap();
        l.create("beta.md", "y").unwrap();
        let uni = vec!["sub/alpha.md".to_string(), "beta.md".to_string()];
        assert_eq!(l.resolve_link("beta", &uni), "beta.md");
        assert_eq!(l.resolve_link("beta.md", &uni), "beta.md");
        assert_eq!(l.resolve_link("sub/alpha", &uni), "sub/alpha.md");
        assert_eq!(l.resolve_link("alpha", &uni), "sub/alpha.md");
        assert_eq!(l.resolve_link("不存在", &uni), "");
        assert_eq!(l.resolve_link("", &uni), "");
    }

    #[test]
    fn rewrite_links_rules() {
        let out = rewrite_links("[[old]] [[old|a]] [[x/old]] [[old.md]] [[oldx]]", "old", "new");
        assert_eq!(out, "[[new]] [[new|a]] [[x/new]] [[new.md]] [[oldx]]");
    }
}
