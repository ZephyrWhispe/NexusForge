//! F3 冲突策略（docs/impl/05 F3）：同名目标 → 跳过 / 覆盖 / 重命名 / 询问。
//!
//! v1 交互简化：`Ask` 策略在入队前预扫描返回冲突清单，UI 逐条（或"应用到全部"）
//! 决议后以具体策略重新入队；`operation.conflict` 事件保留给未来的逐文件中断式询问。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::FileError;

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

/// 依据自动策略解析目标。
///
/// T-B6-7 裁决路线 (ii)（09 §6.2）：旧逐单决议枚举（Action 后缀那位）全仓零消费者，
/// 已删除；`Ask` 在执行臂**真报错**——预扫描之后新出现的冲突绝不再"与 Skip
/// 同体静默跳过"。判据：Ask 臂返回 `Err` 且点名文件，Skip 臂返回 `Ok(None)`。
/// Skip → `Ok(None)`；Overwrite → 原目标；Rename → 第一个不冲突的 `name (n).ext`。
pub fn resolve_target(
    _src: &Path,
    dst: &Path,
    policy: ConflictPolicy,
) -> Result<Option<PathBuf>, FileError> {
    match policy {
        ConflictPolicy::Overwrite => Ok(Some(dst.to_path_buf())),
        ConflictPolicy::Skip => {
            if dst.exists() {
                Ok(None)
            } else {
                Ok(Some(dst.to_path_buf()))
            }
        }
        ConflictPolicy::Rename => Ok(Some(unique_target(dst))),
        ConflictPolicy::Ask => {
            if dst.exists() {
                Err(FileError::BadState(format!(
                    "Ask 冲突未经决议不得执行: {} ——预扫描后新出现的同名目标，须逐条决议后重新入队（本操作未计入完成）",
                    dst.display()
                )))
            } else {
                Ok(Some(dst.to_path_buf()))
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
        .map(|s| s.to_string_lossy().into_owned())
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

/// 预扫描冲突条目（Ask 策略 UI 决议数据源）
#[derive(Clone, Debug, Serialize)]
pub struct ConflictItem {
    /// 源文件名
    pub name: String,
    pub dst: PathBuf,
}

/// 纯冲突引擎（T-B6-2 承重：本地 `scan_conflicts` 与远端预扫描共读一份，
/// 禁第二份冲突引擎）：从 src_names 中挑出与目标桶 `existing`（该目录已有
/// 名字集合）冲突者，落到桶键 dst 上。本地事实源=fs 探测，远端事实源=列表
/// 返回——只有事实源不同，裁决算法在此唯一。
pub fn conflict_pairs(
    src_names: &[String],
    existing: &HashSet<String>,
    dst: &str,
) -> Vec<ConflictItem> {
    src_names
        .iter()
        .filter(|name| existing.contains(*name))
        .map(|name| ConflictItem {
            name: name.clone(),
            dst: Path::new(dst).join(name),
        })
        .collect()
}

/// 同名冲突时的唯一名（Windows 资源管理器风格 `name (2).ext`）——纯集合版：
/// 本地探测 fs（[`unique_target`]），远端探测目录列表集合；序号算法两处共读。
pub fn unique_name(name: &str, existing: &HashSet<String>) -> String {
    if !existing.contains(name) {
        return name.to_string();
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 2..10_000u32 {
        let cand = format!("{stem} ({n}){ext}");
        if !existing.contains(&cand) {
            return cand;
        }
    }
    // 兜底：时间戳后缀（与 unique_target 同形）
    format!(
        "{stem} ({}){ext}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default()
    )
}

pub fn scan_conflicts(srcs: &[PathBuf], dst_dir: &Path) -> Vec<ConflictItem> {
    // 本地事实源逐字保留 exists() 语义（Windows 大小写不敏感由 fs 裁决）；
    // 冲突判定与目标拼接交回唯一引擎
    let names: Vec<String> = srcs
        .iter()
        .filter_map(|src| src.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    let existing: HashSet<String> = names
        .iter()
        .filter(|name| dst_dir.join(name).exists())
        .cloned()
        .collect();
    conflict_pairs(&names, &existing, &dst_dir.display().to_string())
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

        // FileError 不含 PartialEq（Io 臂包 io::Error）⇒ 断言走 unwrap：
        // Err 意外时 unwrap 直接 panic 点名，与原 `== Ok(...)` 同强度
        assert_eq!(
            resolve_target(&src, &dst, ConflictPolicy::Rename).unwrap(),
            Some(d.join("g (2).txt"))
        );
        assert_eq!(
            resolve_target(&src, &dst, ConflictPolicy::Overwrite).unwrap(),
            Some(dst.clone())
        );
        assert_eq!(
            resolve_target(&src, &dst, ConflictPolicy::Skip).unwrap(),
            None
        );
        assert!(
            matches!(
                resolve_target(&src, &dst, ConflictPolicy::Ask),
                Err(FileError::BadState(m)) if m.contains("Ask 冲突未经决议")
            ),
            "Ask 臂必须真报错，不再与 Skip 同体"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_conflict_ask_passthrough() {
        let d = tmpdir("noconflict");
        let dst = d.join("new.txt");
        assert_eq!(
            resolve_target(&d.join("a"), &dst, ConflictPolicy::Ask).unwrap(),
            Some(dst.clone())
        );
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

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-2）字面测试名优先于 rustc 命名惯例
    fn conflictPairs_localAndRemoteEnginesAgree() {
        let d = tmpdir("agree");
        std::fs::write(d.join("hit.txt"), b"").unwrap();
        std::fs::write(d.join("hit2.txt"), b"").unwrap();
        let src_names: Vec<String> = vec![
            "hit.txt".to_string(),
            "miss.txt".to_string(),
            "hit2.txt".to_string(),
        ];
        let srcs: Vec<PathBuf> = src_names.iter().map(|n| d.join(n)).collect();
        // 两路预扫描：本地（fs 事实源）vs 远端形状（列表集合事实源）
        let local = scan_conflicts(&srcs, &d);
        let existing: HashSet<String> = src_names
            .iter()
            .filter(|n| d.join(n).exists())
            .cloned()
            .collect();
        let remote = conflict_pairs(&src_names, &existing, "/remote/dir");
        let names = |v: &[ConflictItem]| v.iter().map(|i| i.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&local), names(&remote));
        assert_eq!(
            names(&local),
            vec!["hit.txt".to_string(), "hit2.txt".to_string()]
        );
        // 唯一名两引擎逐字同序：本地 unique_target（探测 fs）vs 纯 unique_name（探测集合）
        std::fs::write(d.join("a.txt"), b"1").unwrap();
        std::fs::write(d.join("a (2).txt"), b"2").unwrap();
        let mut ex2: HashSet<String> = HashSet::new();
        ex2.insert("a.txt".to_string());
        ex2.insert("a (2).txt".to_string());
        assert_eq!(
            unique_target(&d.join("a.txt")).file_name().unwrap(),
            Path::new(&unique_name("a.txt", &ex2)).file_name().unwrap()
        );
        assert_eq!(unique_name("a.txt", &ex2), "a (3).txt");
        // 远端臂零 fs 参与：空集合直通原名（无冲突即不改名）
        assert_eq!(unique_name("报告.md", &HashSet::new()), "报告.md");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-7）字面测试名优先于 rustc 命名惯例
    fn conflictAskArm_noLongerSharesCodeWithSkip() {
        // 编译期形状证据（裁决路线 (ii)）：行为双臂
        let d = tmpdir("askarm");
        let conflict = d.join("dup.txt");
        std::fs::write(&conflict, b"x").unwrap();
        let clean = d.join("new.txt");
        // Ask 有冲突 ⇒ Err；Skip 有冲突 ⇒ Ok(None)。两臂必须给出不同裁决——
        // 同体时二者返回值相同，本测当场判红。
        let ask = resolve_target(&d.join("s"), &conflict, ConflictPolicy::Ask);
        let skip = resolve_target(&d.join("s"), &conflict, ConflictPolicy::Skip);
        assert!(ask.is_err(), "Ask 冲突臂须 Err（修前与 Skip 同体静默跳过）");
        assert_eq!(skip.unwrap(), None, "Skip 正对照：仍静默跳＝语义不变");
        assert!(
            ask.unwrap_err()
                .to_string()
                .contains(conflict.file_name().unwrap().to_string_lossy().as_ref()),
            "错误消息须点名冲突文件"
        );
        assert_eq!(
            resolve_target(&d.join("s"), &clean, ConflictPolicy::Ask).unwrap(),
            Some(clean),
            "Ask 无冲突直通（预扫描干净即照常执行）"
        );
        // grep 面：源文里 Ask 臂含 Err 返回且死决议枚举（同名三词）已整名退役
        let src = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/conflict.rs"),
        )
        .unwrap();
        let ask_arm = src
            .split("ConflictPolicy::Ask =>")
            .nth(1)
            .unwrap()
            .split('}')
            .next()
            .unwrap();
        assert!(ask_arm.contains("Err("), "Ask 臂源码须含 Err 返回");
        assert!(
            // 判据串自身即命中点（本文件自扫）⇒ 字面量拆两段拼接，防"扫到自己"假红
            !src.contains(&["Conflict", "Action"].concat()),
            "死决议枚举不得残留任何一处（只减不增判据）"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
