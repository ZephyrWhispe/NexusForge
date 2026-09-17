//! F1 文件浏览服务（docs/impl/05 F1）：列目录 / 排序 / 面包屑 / 盘符枚举。
//!
//! 路径纪律：一律 [`std::path::PathBuf`]；>248 字符统一走 `\\?\` 前缀
//! （docs/impl/05 F 风险标注）。`\\?\` 仅用于 IO，展示层用原始路径。

use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::error::FileError;

/// 目录条目（IPC DTO，字段与前端 FileEntryDto 对齐——缺字段是静默失败重灾区）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    /// 原始路径（展示用，不带 \\?\ 前缀）
    pub path: PathBuf,
    pub is_dir: bool,
    /// 字节；目录为 0
    pub size: u64,
    /// 毫秒时间戳（前端 new Date(ms) 直用）
    pub modified_ms: i64,
    /// 小写扩展名（不含点，目录为空）
    pub ext: String,
    pub hidden: bool,
}

/// 排序键（目录恒排前面，与资源管理器一致）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Type,
}

/// 把绝对路径转为 `\\?\` 前缀长路径（仅 IO 层用）
pub fn to_long_path(p: &Path) -> PathBuf {
    let s = p.as_os_str().to_string_lossy();
    if s.starts_with(r"\\?\") || s.len() < 248 {
        return p.to_path_buf();
    }
    if let Some(rest) = s.strip_prefix(r"\\") {
        return PathBuf::from(format!(r"\\?\UNC\{rest}"));
    }
    PathBuf::from(format!(r"\\?\{s}"))
}

/// 去掉 `\\?\` / `\\?\UNC\` 前缀（展示与事件 payload 用）
pub fn display_path(p: &Path) -> PathBuf {
    let s = p.as_os_str().to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_owned());
    }
    p.to_path_buf()
}

fn entry_from_metadata(path: PathBuf, md: &std::fs::Metadata) -> FileEntry {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let is_dir = md.is_dir();
    let modified_ms = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let hidden = md
        .file_attributes()
        & 0x2 != 0; // FILE_ATTRIBUTE_HIDDEN
    FileEntry {
        name,
        path: display_path(&path),
        is_dir,
        size: if is_dir { 0 } else { md.len() },
        modified_ms,
        ext: if is_dir { String::new() } else { ext },
        hidden,
    }
}

/// 列目录（F1）。目录恒在前，其余按 sort_key / asc 排序。
pub fn list_dir(path: &Path, sort: SortKey, asc: bool) -> Result<Vec<FileEntry>, FileError> {
    let io_path = to_long_path(path);
    if !io_path.exists() {
        return Err(FileError::NotFound(
            display_path(path).to_string_lossy().into_owned(),
        ));
    }
    let rd = std::fs::read_dir(&io_path)?;
    let mut entries = Vec::new();
    for item in rd {
        let Ok(item) = item else { continue };
        let Ok(md) = item.metadata() else { continue };
        entries.push(entry_from_metadata(item.path(), &md));
    }
    let dir_rank = |e: &FileEntry| if e.is_dir { 0u8 } else { 1 };
    entries.sort_by(|a, b| {
        dir_rank(a).cmp(&dir_rank(b)).then_with(|| match sort {
            SortKey::Size => {
                if asc { a.size.cmp(&b.size) } else { b.size.cmp(&a.size) }
            }
            SortKey::Modified => {
                if asc {
                    a.modified_ms.cmp(&b.modified_ms)
                } else {
                    b.modified_ms.cmp(&a.modified_ms)
                }
            }
            // Name / Type 都按名称排（Type v1 简化为扩展名聚在名称序内）
            _ => {
                let (an, bn) = (&a.name.to_lowercase(), &b.name.to_lowercase());
                if asc { an.cmp(bn) } else { bn.cmp(an) }
            }
        })
    });
    Ok(entries)
}

/// 面包屑（F1）：`C:\a\b` → [("C:", C:\), ("a", C:\a), ("b", C:\a\b)]
pub fn breadcrumbs(path: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut acc = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => {
                acc.push(p.as_os_str());
                out.push((p.as_os_str().to_string_lossy().into_owned(), acc.clone()));
            }
            Component::RootDir => {
                acc.push(std::path::MAIN_SEPARATOR_STR);
                // 盘符根与 RootDir 合并为同一段（C: → C:\）
                if let Some(first) = out.first_mut() {
                    first.1 = acc.clone();
                }
            }
            Component::Normal(seg) => {
                acc.push(seg);
                out.push((seg.to_string_lossy().into_owned(), acc.clone()));
            }
            _ => {}
        }
    }
    out
}

/// 盘符信息（磁盘容量由 win-integration 未来补充，v1 为 0）
#[derive(Clone, Debug, Serialize)]
pub struct DriveInfo {
    pub letter: String,
    pub path: PathBuf,
    pub free_bytes: u64,
    pub total_bytes: u64,
}

/// 盘符枚举：A..Z 中实际存在的盘
pub fn drives() -> Vec<DriveInfo> {
    let mut out = Vec::new();
    for b in b'A'..=b'Z' {
        let root = PathBuf::from(format!(r"{}:\", b as char));
        if !root.is_dir() {
            continue;
        }
        out.push(DriveInfo {
            letter: (b as char).to_string(),
            path: root,
            free_bytes: 0,
            total_bytes: 0,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_file_browse_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("a.txt"), b"hello").unwrap();
        std::fs::write(d.join("b.log"), b"x".repeat(100)).unwrap();
        d
    }

    #[test]
    fn list_dirs_first_then_sorted() {
        let d = tmpdir("sort");
        let entries = list_dir(&d, SortKey::Name, true).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["sub", "a.txt", "b.log"]);
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].ext, "txt");
        assert_eq!(entries[1].size, 5);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn list_desc_order() {
        let d = tmpdir("desc");
        let entries = list_dir(&d, SortKey::Size, false).unwrap();
        let files: Vec<_> = entries.iter().filter(|e| !e.is_dir).collect();
        assert_eq!(files[0].name, "b.log"); // 100B 在前
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn list_missing_dir_errors() {
        assert!(matches!(
            list_dir(Path::new(r"C:\nf_nonexist_zz"), SortKey::Name, true),
            Err(FileError::NotFound(_))
        ));
    }

    #[test]
    fn breadcrumbs_split_drive_and_segments() {
        let crumb = breadcrumbs(Path::new(r"C:\a\b"));
        let strs: Vec<(String, String)> = crumb
            .iter()
            .map(|(n, p)| (n.clone(), p.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(strs[0].0, "C:");
        assert_eq!(strs[0].1, r"C:\");
        assert_eq!(strs[1].0, "a");
        assert_eq!(strs[1].1, r"C:\a");
        assert_eq!(strs[2].1, r"C:\a\b");
    }

    #[test]
    fn long_path_prefix_roundtrip() {
        let deep = format!(r"C:\{}", "x".repeat(260));
        let p = to_long_path(Path::new(&deep));
        assert!(p.as_os_str().to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(display_path(&p).to_string_lossy().len(), 263);
        // 短路径不加前缀
        assert_eq!(to_long_path(Path::new(r"C:\a")), PathBuf::from(r"C:\a"));
    }

    #[test]
    fn drives_nonempty_and_valid() {
        let ds = drives();
        assert!(!ds.is_empty(), "测试机至少有 C: 盘");
        assert!(ds.iter().any(|d| d.letter == "C"));
    }
}
