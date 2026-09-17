//! F5 全局文件搜索（docs/impl/05 F5）：USN/MFT 索引优先，无权限降级目录遍历。
//!
//! - 有 [`UsnIndexPort`]（win-integration FSCTL_ENUM_USN_DATA，需管理员）→ 直接查索引
//! - 端口缺失或返回权限错误 → 降级 walkdir 遍历（默认根：用户主目录，限深 6）并在
//!   返回值上标注 `degraded: true`（UI 显示"索引受限"）

use std::path::PathBuf;

use serde::Serialize;

use host_core::ports::UsnIndexPort;

use crate::browse::{display_path, to_long_path};
use crate::error::FileError;

/// 搜索命中（复用 host_core::ports::FileHit{path, score}）
#[derive(Clone, Debug, Serialize)]
pub struct SearchResult {
    pub hits: Vec<host_core::ports::FileHit>,
    /// true = USN 不可用走了目录遍历（结果可能不全）
    pub degraded: bool,
}

#[derive(Clone, Debug)]
pub struct SearchOpts {
    pub query: String,
    pub limit: u32,
    /// 降级遍历的根目录（None = 用户主目录）
    pub root: Option<PathBuf>,
    /// 降级遍历最大深度
    pub max_depth: usize,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            query: String::new(),
            limit: 50,
            root: None,
            max_depth: 6,
        }
    }
}

pub fn search(usn: Option<&dyn UsnIndexPort>, opts: &SearchOpts) -> Result<SearchResult, FileError> {
    let q = opts.query.trim().to_owned();
    if q.is_empty() {
        return Ok(SearchResult { hits: vec![], degraded: false });
    }
    if let Some(port) = usn {
        match port.search(&q, opts.limit) {
            Ok(hits) => return Ok(SearchResult { hits, degraded: false }),
            // 无管理员权限等场景：降级遍历（docs/impl/05 F 风险标注）
            Err(e) => tracing::warn!(error = %e, "USN 索引查询失败，降级目录遍历"),
        }
    }
    Ok(SearchResult { hits: walk_search(&q, opts)?, degraded: true })
}

fn default_root() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\"))
}

/// 忽略的目录名（降级遍历提速 + 避免系统目录噪音）
fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        "Windows" | "AppData" | "$Recycle.Bin" | "System Volume Information"
            | "node_modules" | ".git" | "target" | "dist" | "$WINDOWS.~BT"
    )
}

fn score(name_lower: &str, q_lower: &str) -> f32 {
    if name_lower == q_lower {
        1.0
    } else if name_lower.starts_with(q_lower) {
        0.9 - (name_lower.len() as f32 / 1000.0).min(0.1)
    } else if let Some(pos) = name_lower.find(q_lower) {
        0.6 - (pos as f32 / 1000.0).min(0.1)
    } else {
        0.0
    }
}

fn walk_search(query: &str, opts: &SearchOpts) -> Result<Vec<host_core::ports::FileHit>, FileError> {
    let root = opts
        .root
        .clone()
        .unwrap_or_else(default_root);
    let q_lower = query.to_lowercase();
    let mut hits: Vec<host_core::ports::FileHit> = Vec::new();
    let walker = walkdir::WalkDir::new(to_long_path(&root))
        .max_depth(opts.max_depth)
        .follow_links(false)
        .into_iter()
        // 剪枝：跳过噪音目录子树（node_modules/AppData 等）
        .filter_entry(|e| {
            !(e.file_type().is_dir() && e.depth() > 0 && skip_dir(&e.file_name().to_string_lossy()))
        });
    for entry in walker.filter_map(|e| e.ok()) {
        if hits.len() >= opts.limit as usize {
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        let s = score(&name.to_lowercase(), &q_lower);
        if s > 0.0 {
            hits.push(host_core::ports::FileHit {
                path: display_path(entry.path()),
                score: s,
            });
        }
    }
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(opts.limit as usize);
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_file_search_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("node_modules")).unwrap();
        std::fs::write(d.join("Report-2026.txt"), b"").unwrap();
        std::fs::write(d.join("report_final.txt"), b"").unwrap();
        std::fs::write(d.join("other.md"), b"").unwrap();
        std::fs::write(d.join("node_modules/ignored.txt"), b"").unwrap();
        d
    }

    #[test]
    fn walk_search_scores_prefix_over_contains_and_skips_noise() {
        let d = tmpdir("walk");
        let opts = SearchOpts {
            query: "report".into(),
            limit: 10,
            root: Some(d.clone()),
            max_depth: 6,
        };
        let r = search(None, &opts).unwrap();
        assert!(r.degraded, "无 USN 端口应标注降级");
        let mut names: Vec<_> = r.hits.iter().map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, vec!["Report-2026.txt", "report_final.txt"]);
        // node_modules 内命中被跳过
        assert!(!names.iter().any(|n| n == "ignored.txt"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn empty_query_returns_empty() {
        let r = search(None, &SearchOpts::default()).unwrap();
        assert!(r.hits.is_empty());
        assert!(!r.degraded);
    }

    #[test]
    fn usn_port_takes_priority() {
        struct FakeUsn;
        impl UsnIndexPort for FakeUsn {
            fn search(&self, _q: &str, _limit: u32) -> Result<Vec<host_core::ports::FileHit>, host_core::error::AppError> {
                Ok(vec![host_core::ports::FileHit { path: PathBuf::from("C:\\fake.txt"), score: 1.0 }])
            }
        }
        let r = search(Some(&FakeUsn), &SearchOpts { query: "fake".into(), ..Default::default() }).unwrap();
        assert!(!r.degraded);
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].path, PathBuf::from("C:\\fake.txt"));
    }
}
