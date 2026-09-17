//! F7 批量重命名 DSL（docs/impl/05 F7）：
//! `{name}{ext}` 变量替换 + 序号补零 + 正则替换 + 大小写转换；预览前 20 条再应用。
//!
//! 模板变量：`{name}` 主名（正则/大小写处理后的 stem）、`{ext}` 带点扩展名、
//! `{n}` 序号、`{n:03}` 补零序号。规则非法（空模板结果）返回 [`FileError::Rule`]。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::FileError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseMode {
    #[default]
    None,
    Lower,
    Upper,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenameRule {
    /// 输出模板，如 `"{name}_{n:03}{ext}"`
    pub template: String,
    /// 对主名先做正则替换（None = 不处理）
    #[serde(default)]
    pub regex: Option<String>,
    #[serde(default)]
    pub replacement: String,
    #[serde(default)]
    pub case: CaseMode,
    /// `{n}` 起始序号（默认 1）
    #[serde(default = "default_start")]
    pub start: u32,
}

fn default_start() -> u32 {
    1
}

impl Default for RenameRule {
    fn default() -> Self {
        Self {
            template: "{name}{ext}".into(),
            regex: None,
            replacement: String::new(),
            case: CaseMode::None,
            start: 1,
        }
    }
}

/// 单条预览/执行计划
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenamePlan {
    pub from: PathBuf,
    pub to: PathBuf,
    /// 目标已存在或与其它计划目标重复
    pub conflict: bool,
}

/// 生成重命名计划（F7：预览阶段；names 为 dir 下的文件名子集，空 = 全部条目）
pub fn build_plan(dir: &Path, names: &[String], rule: &RenameRule) -> Result<Vec<RenamePlan>, FileError> {
    let re = rule
        .regex
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| regex::Regex::new(s))
        .transpose()
        .map_err(|e| FileError::Rule(e.to_string()))?;

    // 目标名单：显式指定或列目录（仅文件）
    let targets: Vec<String> = if names.is_empty() {
        crate::browse::list_dir(dir, crate::browse::SortKey::Name, true)?
            .into_iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.name)
            .collect()
    } else {
        names.to_vec()
    };

    let mut plans = Vec::with_capacity(targets.len());
    for (i, name) in targets.iter().enumerate() {
        let p = Path::new(name);
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = p
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();

        // ① 正则替换（作用于主名）
        let mut new_stem = match &re {
            Some(r) => r.replace_all(&stem, rule.replacement.as_str()).into_owned(),
            None => stem,
        };
        // ② 大小写
        match rule.case {
            CaseMode::Lower => new_stem = new_stem.to_lowercase(),
            CaseMode::Upper => new_stem = new_stem.to_uppercase(),
            CaseMode::None => {}
        }
        // ③ 模板替换
        let out = render(&rule.template, &new_stem, &ext, rule.start + i as u32)?;
        if out.is_empty() || out == *name {
            plans.push(RenamePlan {
                from: dir.join(name),
                to: dir.join(name),
                conflict: false,
            });
            continue;
        }
        if out.contains('/') || out.contains('\\') || out.contains("..") {
            return Err(FileError::Rule(format!("模板结果包含路径分隔符: {out}")));
        }
        plans.push(RenamePlan {
            from: dir.join(name),
            to: dir.join(&out),
            conflict: false,
        });
    }
    // ④ 冲突标注：目标已存在（非自身）或计划内重复（多次出现的目标全部标记）；
    //    模板结果与源相同（no-op）不计冲突
    let mut target_count: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for plan in &plans {
        if plan.from != plan.to {
            *target_count
                .entry(plan.to.to_string_lossy().to_lowercase())
                .or_default() += 1;
        }
    }
    for plan in &mut plans {
        if plan.from == plan.to {
            continue;
        }
        let to_s = plan.to.to_string_lossy().to_lowercase();
        let exists = plan.to.exists() && !same_file(&plan.from, &plan.to);
        let dup = target_count.get(&to_s).copied().unwrap_or(0) > 1;
        plan.conflict = exists || dup;
    }
    Ok(plans)
}

/// 应用计划：冲突条目跳过；返回成功条数
pub fn apply_plan(plans: &[RenamePlan]) -> Result<usize, FileError> {
    let mut ok = 0usize;
    for plan in plans {
        if plan.conflict || plan.from == plan.to {
            continue;
        }
        std::fs::rename(&plan.from, &plan.to)?;
        ok += 1;
    }
    Ok(ok)
}

/// 模板渲染：{name} {ext} {n} {n:0N}
fn render(template: &str, name: &str, ext: &str, n: u32) -> Result<String, FileError> {
    let mut out = String::with_capacity(template.len() + 8);
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = template[i..].find('}') {
                let token = &template[i + 1..i + end];
                match token {
                    "name" => out.push_str(name),
                    "ext" => out.push_str(ext),
                    "n" => out.push_str(&n.to_string()),
                    t if t.starts_with("n:") => {
                        let width: usize = t[2..]
                            .parse()
                            .map_err(|_| FileError::Rule(format!("补零宽度非法: {t}")))?;
                        if width > 10 {
                            return Err(FileError::Rule(format!("补零宽度过大: {t}")));
                        }
                        out.push_str(&format!("{n:0width$}", width = width));
                    }
                    other => return Err(FileError::Rule(format!("未知变量: {{{other}}}"))),
                }
                i += end + 1;
                continue;
            }
            return Err(FileError::Rule("模板缺少闭合大括号".into()));
        }
        // 逐字符 push（UTF-8 安全）
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&template[i..i + ch_len]);
        i += ch_len;
    }
    Ok(out)
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_file_rename_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn rule(template: &str) -> RenameRule {
        RenameRule { template: template.into(), ..Default::default() }
    }

    #[test]
    fn template_variables_and_padding() {
        let d = tmpdir("tpl");
        std::fs::write(d.join("photo.jpg"), b"").unwrap();
        std::fs::write(d.join("notes.txt"), b"").unwrap();
        let plans = build_plan(&d, &[], &rule("{name}_{n:03}{ext}")).unwrap();
        // 列目录序：notes.txt < photo.jpg（字母序）
        assert_eq!(plans[0].to.file_name().unwrap(), "notes_001.txt");
        assert_eq!(plans[1].to.file_name().unwrap(), "photo_002.jpg");
        assert!(plans.iter().all(|p| !p.conflict));
        let n = apply_plan(&plans).unwrap();
        assert_eq!(n, 2);
        assert!(d.join("notes_001.txt").exists());
        assert!(d.join("photo_002.jpg").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn regex_replace_and_case() {
        let d = tmpdir("re");
        std::fs::write(d.join("IMG-0001.jpg"), b"").unwrap();
        std::fs::write(d.join("IMG-0002.jpg"), b"").unwrap();
        let r = RenameRule {
            template: "{name}{ext}".into(),
            regex: Some(r"IMG-(\d+)".into()),
            replacement: "shot_$1".into(),
            case: CaseMode::Lower,
            ..Default::default()
        };
        let plans = build_plan(&d, &[], &r).unwrap();
        assert_eq!(plans[0].to.file_name().unwrap(), "shot_0001.jpg");
        assert_eq!(plans[1].to.file_name().unwrap(), "shot_0002.jpg");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn conflicts_flagged_target_exists_and_duplicates() {
        let d = tmpdir("conf");
        std::fs::write(d.join("a.txt"), b"").unwrap();
        std::fs::write(d.join("b.txt"), b"").unwrap();
        std::fs::write(d.join("taken.txt"), b"").unwrap();
        // a.txt → taken.txt（已存在 → conflict）；b.txt、taken.txt → 自身（no-op）
        let plans = build_plan(
            &d,
            &["a.txt".into(), "b.txt".into(), "taken.txt".into()],
            &rule("taken.txt"),
        )
        .unwrap();
        assert!(plans[0].conflict);
        // taken.txt 模板结果与自身相同 → no-op 不冲突
        assert!(!plans[2].conflict && plans[2].from == plans[2].to);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn duplicate_targets_within_plan_conflict() {
        let d = tmpdir("dup");
        std::fs::write(d.join("a1.txt"), b"").unwrap();
        std::fs::write(d.join("a2.txt"), b"").unwrap();
        // 模板丢掉序号 → 两个源映射同一目标
        let plans = build_plan(&d, &[], &rule("same{ext}")).unwrap();
        assert!(plans[0].conflict && plans[1].conflict);
        let n = apply_plan(&plans).unwrap();
        assert_eq!(n, 0, "冲突计划全部跳过");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn invalid_template_errors() {
        let d = tmpdir("bad");
        std::fs::write(d.join("x.txt"), b"").unwrap();
        assert!(build_plan(&d, &[], &rule("{unknown}")).is_err());
        assert!(build_plan(&d, &[], &rule("{n:99}")).is_err());
        assert!(build_plan(&d, &[], &rule("{name")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn chinese_names_survive() {
        let d = tmpdir("zh");
        std::fs::write(d.join("照片-1.jpg"), b"").unwrap();
        let plans = build_plan(&d, &[], &rule("旅行-{name}{ext}")).unwrap();
        assert_eq!(plans[0].to.file_name().unwrap(), "旅行-照片-1.jpg");
        let _ = std::fs::remove_dir_all(&d);
    }
}
