//! F3 冲突策略（docs/impl/05 F3）：同名目标 → 跳过 / 覆盖 / 重命名 / 询问。
//!
//! v1 交互简化：`Ask` 策略在入队前预扫描返回冲突清单，UI 逐条（或"应用到全部"）
//! 决议后以具体策略重新入队；`operation.conflict` 事件保留给未来的逐文件中断式询问。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 同名冲突处理策略（docs/impl/05 F3）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// 入队前预扫描，返回冲突目标给 UI 决议
    #[default]
    Ask,
    Skip,
    Overwrite,
    /// 目标重命名为 `name (2).ext` 序号
    Rename,
}

/// 单条预扫描决议结果
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictAction {
    Skip,
    Overwrite,
    /// 使用给定的新目标路径
    RenameTo,
}

/// 依据自动策略解析目标（Ask 返回 None 表示需要上层决议）。
/// Skip → None；Overwrite → 原目标；Rename → 第一个不冲突的 `name (n).ext`。
pub fn resolve_target(_src: &Path, dst: &Path, policy: ConflictPolicy) -> Option<PathBuf> {
    match policy {
        ConflictPolicy::Overwrite => Some(dst.to_path_buf()),
        ConflictPolicy::Skip => {
            if dst.exists() {
                None
            } else {
                Some(dst.to_path_buf())
            }
        }
        ConflictPolicy::Rename => Some(unique_target(dst)),
        ConflictPolicy::Ask => {
            if dst.exists() {
                None
            } else {
                Some(dst.to_path_buf())
            }
        }
    }
}

/// 目标已存在时生成不冲突的 `name (2).ext`（Windows 资源管理器风格）
pub fn unique_target(dst: &Path) -> PathBuf {
    if !dst.exists() {
        return dst.to_path_buf();
    }
    let parent = dst.parent().unwrap_or(Path::new("."));
    let stem = dst
        .file_stem()
        .map(|s| s.to_string_lossy().to_owned())
        .unwrap_or_default();
    let ext = dst
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 2..10_000u32 {
        let cand = parent.join(format!("{stem} ({n}){ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    // 兜底：时间戳后缀
    parent.join(format!(
        "{stem} ({}){ext}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default()
    ))
}

/// 预扫描：srcs 列表落到位 dst 时会冲突的目标（Ask 策略 UI 决议数据源）
#[derive(Clone, Debug, Serialize)]
pub struct ConflictItem {
    /// 源文件名
    pub name: String,
    pub dst: PathBuf,
}

pub fn scan_conflicts(srcs: &[PathBuf], dst_dir: &Path) -> Vec<ConflictItem> {
    srcs.iter()
        .filter_map(|src| {
            let name = src.file_name()?.to_string_lossy().into_owned();
            let target = dst_dir.join(&name);
            target.exists().then(|| ConflictItem { name, dst: target })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_file_conflict_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rename_policy_generates_sequenced_name() {
        let d = tmpdir("rename");
        std::fs::write(d.join("f.txt"), b"old").unwrap();
        let src = d.join("f.txt");
        let dst = d.join("g.txt");
        std::fs::write(&dst, b"x").unwrap();

        assert_eq!(
            resolve_target(&src, &dst, ConflictPolicy::Rename),
            Some(d.join("g (2).txt"))
        );
        assert_eq!(resolve_target(&src, &dst, ConflictPolicy::Overwrite), Some(dst.clone()));
        assert_eq!(resolve_target(&src, &dst, ConflictPolicy::Skip), None);
        assert_eq!(resolve_target(&src, &dst, ConflictPolicy::Ask), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_conflict_ask_passthrough() {
        let d = tmpdir("noconflict");
        let dst = d.join("new.txt");
        assert_eq!(resolve_target(&d.join("a"), &dst, ConflictPolicy::Ask), Some(dst.clone()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unique_target_skips_existing_sequence() {
        let d = tmpdir("seq");
        std::fs::write(d.join("a.txt"), b"1").unwrap();
        std::fs::write(d.join("a (2).txt"), b"2").unwrap();
        assert_eq!(unique_target(&d.join("a.txt")), d.join("a (3).txt"));
        // 无扩展名
        std::fs::write(d.join("b"), b"1").unwrap();
        assert_eq!(unique_target(&d.join("b")), d.join("b (2)"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn scan_conflicts_reports_existing_only() {
        let d = tmpdir("scan");
        std::fs::write(d.join("hit.txt"), b"").unwrap();
        let srcs = vec![d.join("hit.txt"), d.join("miss.txt")];
        let items = scan_conflicts(&srcs, &d);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "hit.txt");
        let _ = std::fs::remove_dir_all(&d);
    }
}
