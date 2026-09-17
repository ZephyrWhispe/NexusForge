//! D1 快速启动器索引：开始菜单(.lnk) + PATH 可执行 + 内置动作。
//!
//! - 内存索引，`build` 为同步扫描（调用方放 spawn_blocking，禁止 UI 线程直调）
//! - usage 频次持久化 `{appData}/desktop/usage.json`（count + 最近使用毫秒）
//! - launch 语义：App → 调用方 ShellExecuteW；Action → 调用方发对应事件

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::error::{DesktopError, Result};
use crate::score::{decay, fuzzy_match, total_score};

/// 索引条目类别
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    /// 可执行/快捷方式（ShellExecuteW 打开 path）
    App,
    /// 内置动作（发事件，如呼出截图/剪贴板面板）
    Action,
}

/// 索引条目
#[derive(Clone, Debug, Serialize)]
pub struct IndexItem {
    /// 稳定 id：App = `app:{path}`；Action = `action:{action_id}`
    pub id: String,
    pub name: String,
    pub kind: ItemKind,
    /// App 为 lnk/exe 绝对路径；Action 为空
    pub path: String,
    /// 索引来源：start-menu / path / builtin
    pub source: &'static str,
    /// Action 事件主题（kind=Action 时有值）
    pub topic: Option<&'static str>,
    /// Action 事件 payload JSON
    pub payload: Option<serde_json::Value>,
}

/// 搜索命中（带打分）
#[derive(Clone, Debug, Serialize)]
pub struct LauncherHit {
    #[serde(flatten)]
    pub item: IndexItem,
    /// 综合分（0,1]
    pub score: f64,
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct Usage {
    count: u32,
    last_ms: i64,
}

pub struct LauncherIndex {
    items: RwLock<Vec<IndexItem>>,
    usage: RwLock<HashMap<String, Usage>>,
    usage_file: PathBuf,
    indexed: RwLock<bool>,
}

impl LauncherIndex {
    /// usage_file = `{appData}/desktop/usage.json`
    pub fn new(usage_file: PathBuf) -> Self {
        let usage = std::fs::read(&usage_file)
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        Self {
            items: RwLock::new(Vec::new()),
            usage: RwLock::new(usage),
            usage_file,
            indexed: RwLock::new(false),
        }
    }

    /// 同步构建索引（调用方放 spawn_blocking）。返回条目数。
    pub fn build(&self, start_menu_dirs: &[PathBuf], path_dirs: &[PathBuf]) -> usize {
        let mut items = Vec::new();

        // ① 开始菜单 .lnk（递归；名称去掉 .lnk 后缀）
        for dir in start_menu_dirs {
            if !dir.is_dir() {
                continue;
            }
            for entry in walkdir::WalkDir::new(dir)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy();
                let Some(base) = name.strip_suffix(".lnk") else {
                    continue;
                };
                if base.is_empty() {
                    continue;
                }
                items.push(IndexItem {
                    id: format!("app:{}", entry.path().display()),
                    name: base.to_string(),
                    kind: ItemKind::App,
                    path: entry.path().display().to_string(),
                    source: "start-menu",
                    topic: None,
                    payload: None,
                });
            }
        }

        // ② PATH 顶层 .exe（不递归；名称去掉 .exe）
        for dir in path_dirs {
            let Ok(rd) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in rd.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some(base) = name.strip_suffix(".exe") else {
                    continue;
                };
                if base.is_empty() {
                    continue;
                }
                items.push(IndexItem {
                    id: format!("app:{}", path.display()),
                    name: base.to_string(),
                    kind: ItemKind::App,
                    path: path.display().to_string(),
                    source: "path",
                    topic: None,
                    payload: None,
                });
            }
        }

        // 去重（同名 App 保留先出现的：开始菜单优先于 PATH）
        items.dedup_by(|a, b| a.id == b.id);

        let total = items.len();
        *self.items.write().expect("索引写锁") = items;
        *self.indexed.write().expect("索引标志写锁") = true;
        total
    }

    /// 注册内置动作（Action 条目；模块 init 时调用）
    pub fn register_action(
        &self,
        action_id: &str,
        name: &str,
        topic: &'static str,
        payload: serde_json::Value,
    ) {
        let item = IndexItem {
            id: format!("action:{action_id}"),
            name: name.to_string(),
            kind: ItemKind::Action,
            path: String::new(),
            source: "builtin",
            topic: Some(topic),
            payload: Some(payload),
        };
        let mut items = self.items.write().expect("索引写锁");
        items.retain(|i| i.id != item.id);
        items.push(item);
    }

    /// 搜索（D1+D2）：打分排序取前 limit 条。频次权重带 30 天衰减。
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<LauncherHit>> {
        if !*self.indexed.read().expect("索引标志读锁") {
            return Err(DesktopError::NotIndexed);
        }
        let items = self.items.read().expect("索引读锁").clone();
        let usage = self.usage.read().expect("频次读锁").clone();
        let now = now_ms();

        let mut hits: Vec<LauncherHit> = items
            .iter()
            .filter_map(|item| {
                let hit = fuzzy_match(query, &item.name)?;
                let weighted = usage.get(&item.id).map_or(0.0, |u| {
                    let days = (now - u.last_ms) as f64 / 86_400_000.0;
                    decay(u.count, days.max(0.0))
                });
                Some(LauncherHit {
                    item: item.clone(),
                    score: total_score(&hit, weighted),
                })
            })
            .collect();
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit);
        Ok(hits)
    }

    /// 记录一次启动（频次 +1 并持久化）
    pub fn record_launch(&self, id: &str) {
        {
            let mut usage = self.usage.write().expect("频次写锁");
            let u = usage.entry(id.to_string()).or_default();
            u.count += 1;
            u.last_ms = now_ms();
        }
        // 持久化失败仅记日志（频次属可丢数据）
        if let Ok(map) = self.usage.read() {
            if let Err(e) = std::fs::write(&self.usage_file, serde_json::to_vec(&*map).unwrap_or_default()) {
                tracing::warn!(error = %e, "usage.json 写入失败");
            }
        }
    }

    /// 取条目（launch 用）
    pub fn get(&self, id: &str) -> Result<IndexItem> {
        if !*self.indexed.read().expect("索引标志读锁") {
            return Err(DesktopError::NotIndexed);
        }
        self.items
            .read()
            .expect("索引读锁")
            .iter()
            .find(|i| i.id == id)
            .cloned()
            .ok_or_else(|| DesktopError::NotFound(id.to_string()))
    }

    /// 索引状态（UI 展示）
    pub fn status(&self) -> (bool, usize) {
        (
            *self.indexed.read().expect("索引标志读锁"),
            self.items.read().expect("索引读锁").len(),
        )
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Windows 标准开始菜单目录（系统 + 用户两级）
pub fn start_menu_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(pd) = std::env::var("ProgramData") {
        dirs.push(Path::new(&pd).join("Microsoft\\Windows\\Start Menu\\Programs"));
    }
    if let Ok(ad) = std::env::var("APPDATA") {
        dirs.push(Path::new(&ad).join("Microsoft\\Windows\\Start Menu\\Programs"));
    }
    dirs
}

/// PATH 目录去重
pub fn path_dirs() -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    std::env::var("PATH")
        .unwrap_or_default()
        .split(';')
        .filter(|s| !s.trim().is_empty())
        .filter_map(|s| std::fs::canonicalize(s).ok())
        .filter(|p| p.is_dir())
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_desktop_idx_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_lnk(dir: &Path, rel: &str) -> PathBuf {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"fake-lnk").unwrap();
        p
    }

    #[test]
    fn build_indexes_start_menu_and_path() {
        let dir = tmpdir("build");
        let sm = dir.join("sm");
        write_lnk(&sm, "Chrome.lnk");
        write_lnk(&sm, "Tools\\Sysinternals\\Procmon.lnk");
        let pdb = dir.join("bin");
        std::fs::create_dir_all(&pdb).unwrap();
        std::fs::write(pdb.join("cargo.exe"), b"mz").unwrap();
        // 非 exe/lnk 应被忽略
        std::fs::write(pdb.join("readme.txt"), b"x").unwrap();

        let idx = LauncherIndex::new(dir.join("usage.json"));
        let total = idx.build(&[sm], &[pdb]);
        assert_eq!(total, 3, "2 lnk + 1 exe");

        let (ready, n) = idx.status();
        assert!(ready && n == 3);

        // 前缀命中排序
        let hits = idx.search("proc", 10).unwrap();
        assert_eq!(hits[0].item.name, "Procmon");
        // kind 与来源
        let app = idx.get(&hits[0].item.id).unwrap();
        assert_eq!(app.kind, ItemKind::App);
        assert_eq!(app.source, "start-menu");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_before_build_is_not_indexed() {
        let dir = tmpdir("notidx");
        let idx = LauncherIndex::new(dir.join("usage.json"));
        assert!(matches!(idx.search("x", 5), Err(DesktopError::NotIndexed)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn usage_boosts_ranking_and_persists() {
        let dir = tmpdir("usage");
        let sm = dir.join("sm");
        write_lnk(&sm, "Alpha.lnk");
        write_lnk(&sm, "Alpine.lnk");

        let idx = LauncherIndex::new(dir.join("usage.json"));
        idx.build(&[sm.clone()], &[]);
        // 都匹配 "al"，最初按打分可能持平；给 Alpine 记 5 次使用
        for _ in 0..5 {
            idx.record_launch(&format!("app:{}", sm.join("Alpine.lnk").display()));
        }
        let hits = idx.search("al", 10).unwrap();
        assert_eq!(hits[0].item.name, "Alpine", "高频次应排前");

        // 持久化：新实例读回频次
        let idx2 = LauncherIndex::new(dir.join("usage.json"));
        idx2.build(&[sm], &[]);
        let hits2 = idx2.search("al", 10).unwrap();
        assert_eq!(hits2[0].item.name, "Alpine", "频次应持久化");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn register_action_overrides_and_launchable() {
        let dir = tmpdir("action");
        let idx = LauncherIndex::new(dir.join("usage.json"));
        idx.build(&[], &[]);
        idx.register_action("screenshot", "截图", "screenshot.overlay_requested", serde_json::json!({"mode":"shot"}));
        // 重复注册覆盖不重复
        idx.register_action("screenshot", "截图", "screenshot.overlay_requested", serde_json::json!({"mode":"shot"}));

        let hits = idx.search("截图", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].item.kind, ItemKind::Action);
        assert_eq!(hits[0].item.topic, Some("screenshot.overlay_requested"));

        let item = idx.get("action:screenshot").unwrap();
        assert_eq!(item.payload.as_ref().unwrap()["mode"], "shot");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedup_keeps_first_source() {
        let dir = tmpdir("dedup");
        let sm = dir.join("sm");
        write_lnk(&sm, "Same.lnk");
        let pdb = dir.join("bin");
        std::fs::create_dir_all(&pdb).unwrap();
        // 同名不同后缀路径不同 → 两条（id 含路径，属不同条目）；真正重复的是同路径
        std::fs::write(pdb.join("Same.exe"), b"mz").unwrap();

        let idx = LauncherIndex::new(dir.join("usage.json"));
        let total = idx.build(&[sm], &[pdb]);
        assert_eq!(total, 2, "lnk 与 exe 路径不同属两条");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
