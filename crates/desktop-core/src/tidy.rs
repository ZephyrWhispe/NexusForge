//! D3 桌面格子整理（v1 安全实现，docs/impl/05 D3）：
//! 不碰 Windows 桌面 ListView API —— 仅把桌面**普通文件**按扩展名归类，
//! 移入 `{桌面}/{分类名}/` 文件夹；快捷方式(.lnk/.url)与目录一律不动
//! （它们是软件入口，误移代价高）。移动映射落 manifest JSON，支持一键还原。
//!
//! 同名冲突跳过并记录（绝不覆盖用户文件）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{DesktopError, Result};

/// 分类中文名（文件夹名）
pub const CATEGORY_NAMES: &[&str] = &["文档", "图片", "压缩包", "音频", "视频", "其他"];

const DESKTOP_INI: &str = "desktop.ini";

/// 桌面待整理文件
#[derive(Clone, Debug, Serialize)]
pub struct DesktopItem {
    pub name: String,
    pub path: String,
    /// 分类名（CATEGORY_NAMES 之一）
    pub category: &'static str,
}

/// 整理计划（UI 预览）
#[derive(Clone, Debug, Serialize, Default)]
pub struct TidyPlan {
    pub groups: Vec<(String, Vec<DesktopItem>)>,
    pub total: usize,
}

/// 移动记录（manifest 持久化 + 还原依据）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MovedEntry {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct TidyManifest {
    pub moves: Vec<MovedEntry>,
    pub applied_ms: i64,
}

/// 分类判定：返回中文分类名
pub fn categorize(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "doc" | "docx" | "pdf" | "txt" | "md" | "xls" | "xlsx" | "ppt" | "pptx" | "csv"
        | "rtf" | "epub" => "文档",
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "svg" | "ico" | "heic" | "tif"
        | "tiff" => "图片",
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "cab" | "iso" => "压缩包",
        "mp3" | "wav" | "flac" | "ape" | "ogg" | "m4a" | "aac" | "wma" => "音频",
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" => "视频",
        _ => "其他",
    }
}

pub struct TidyPlanner {
    /// manifest 存放路径（{appData}/desktop/tidy_manifest.json）
    manifest_path: PathBuf,
}

impl TidyPlanner {
    pub fn new(manifest_path: PathBuf) -> Self {
        Self { manifest_path }
    }

    /// 扫描桌面普通文件生成计划（.lnk/.url/目录/隐藏系统文件不动）
    pub fn plan(&self, desktop: &Path) -> Result<TidyPlan> {
        let items = scan_desktop(desktop)?;
        let mut by_cat: BTreeMap<&'static str, Vec<DesktopItem>> = BTreeMap::new();
        for item in items {
            by_cat.entry(item.category).or_default().push(item);
        }
        let groups = by_cat
            .into_iter()
            .map(|(c, v)| (c.to_string(), v))
            .collect::<Vec<_>>();
        let total = groups.iter().map(|(_, v)| v.len()).sum();
        Ok(TidyPlan { groups, total })
    }

    /// 执行整理：建分类文件夹 + 移动；manifest 落盘后才算成功。
    /// 返回 (移动数, 跳过数)。
    pub fn apply(&self, desktop: &Path) -> Result<(usize, usize)> {
        let plan = self.plan(desktop)?;
        if plan.total == 0 {
            return Ok((0, 0));
        }

        let mut moves = Vec::new();
        let mut skipped = 0usize;
        for (category, items) in &plan.groups {
            let target_dir = desktop.join(category);
            std::fs::create_dir_all(&target_dir)
                .map_err(|e| DesktopError::Tidy(format!("创建 {category} 文件夹失败: {e}")))?;
            for item in items {
                let from = PathBuf::from(&item.path);
                let to = target_dir.join(&item.name);
                if to.exists() {
                    skipped += 1; // 同名冲突：绝不覆盖
                    continue;
                }
                match std::fs::rename(&from, &to) {
                    Ok(()) => moves.push(MovedEntry {
                        from: from.display().to_string(),
                        to: to.display().to_string(),
                    }),
                    // 跨盘/占用等失败：跳过该文件继续（不中断整体整理）
                    Err(_) => skipped += 1,
                }
            }
        }

        if moves.is_empty() {
            return Ok((0, skipped));
        }
        let manifest = TidyManifest {
            moves,
            applied_ms: now_ms(),
        };
        if let Some(parent) = self.manifest_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DesktopError::Tidy(format!("manifest 目录创建失败: {e}")))?;
        }
        std::fs::write(&self.manifest_path, serde_json::to_vec(&manifest)?)
            .map_err(|e| DesktopError::Tidy(format!("manifest 写入失败: {e}")))?;
        Ok((manifest.moves.len(), skipped))
    }

    /// 还原上次整理：按 manifest 反向移动（目标被占用/已删的跳过）。
    /// 返回还原条数；全部还原成功后删除 manifest。
    pub fn restore(&self) -> Result<usize> {
        let raw = std::fs::read(&self.manifest_path)
            .map_err(|e| DesktopError::Tidy(format!("读取整理记录失败: {e}")))?;
        let manifest: TidyManifest = serde_json::from_slice(&raw)?;
        let mut restored = 0usize;
        let mut remaining = Vec::new();
        for m in &manifest.moves {
            let from = PathBuf::from(&m.to); // 反向：现位置 → 原位置
            let to = PathBuf::from(&m.from);
            if !from.exists() {
                continue; // 用户已删/已再移动，跳过
            }
            if to.exists() {
                remaining.push(m.clone()); // 原位被占，保留记录
                continue;
            }
            if std::fs::rename(&from, &to).is_ok() {
                restored += 1;
            } else {
                remaining.push(m.clone());
            }
        }
        if remaining.is_empty() {
            let _ = std::fs::remove_file(&self.manifest_path);
        } else {
            let m = TidyManifest { moves: remaining, applied_ms: manifest.applied_ms };
            std::fs::write(&self.manifest_path, serde_json::to_vec(&m)?)?;
        }
        Ok(restored)
    }

    /// 是否有待还原的整理记录
    pub fn has_manifest(&self) -> bool {
        self.manifest_path.exists()
    }
}

/// 扫描桌面普通文件（排除快捷方式/目录/隐藏/desktop.ini）
fn scan_desktop(desktop: &Path) -> Result<Vec<DesktopItem>> {
    let rd = std::fs::read_dir(desktop)
        .map_err(|e| DesktopError::Tidy(format!("读取桌面目录失败: {e}")))?;
    let mut items = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue; // 目录与 lnk 目标不动（lnk 本身也是文件但按后缀排除）
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.eq_ignore_ascii_case(DESKTOP_INI) || name.starts_with('.') {
            continue; // 系统文件 / 隐藏
        }
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".lnk") || lower.ends_with(".url") {
            continue; // 快捷方式是软件入口，v1 一律不动
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        items.push(DesktopItem {
            name: name.to_string(),
            path: path.display().to_string(),
            category: categorize(ext),
        });
    }
    Ok(items)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_desktop_tidy_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn setup_desktop(tag: &str) -> PathBuf {
        let d = tmpdir(tag);
        std::fs::write(d.join("报告.docx"), b"x").unwrap();
        std::fs::write(d.join("photo.JPG"), b"x").unwrap();
        std::fs::write(d.join("backup.zip"), b"x").unwrap();
        std::fs::write(d.join("song.mp3"), b"x").unwrap();
        std::fs::write(d.join("clip.mp4"), b"x").unwrap();
        std::fs::write(d.join("unknown.xyz"), b"x").unwrap();
        // 不应被动的
        std::fs::write(d.join("Chrome.lnk"), b"x").unwrap();
        std::fs::write(d.join(DESKTOP_INI), b"x").unwrap();
        std::fs::create_dir_all(d.join("已有文件夹")).unwrap();
        d
    }

    #[test]
    fn categorize_by_extension() {
        assert_eq!(categorize("pdf"), "文档");
        assert_eq!(categorize("JPG"), "图片");
        assert_eq!(categorize("7z"), "压缩包");
        assert_eq!(categorize("flac"), "音频");
        assert_eq!(categorize("mkv"), "视频");
        assert_eq!(categorize("xyz"), "其他");
    }

    #[test]
    fn plan_scans_and_groups() {
        let desktop = setup_desktop("plan");
        let planner = TidyPlanner::new(tmpdir("plan-manifest").join("m.json"));
        let plan = planner.plan(&desktop).unwrap();
        assert_eq!(plan.total, 6, "6 个普通文件（lnk/ini/目录排除）");
        let cats: Vec<&str> = plan.groups.iter().map(|(c, _)| c.as_str()).collect();
        assert!(cats.contains(&"文档") && cats.contains(&"图片") && cats.contains(&"其他"));
        // lnk 不在计划里
        assert!(plan
            .groups
            .iter()
            .all(|(_, v)| v.iter().all(|i| !i.name.ends_with(".lnk"))));
    }

    #[test]
    fn apply_moves_and_manifest_enables_restore() {
        let desktop = setup_desktop("apply");
        let mdir = tmpdir("apply-manifest");
        let planner = TidyPlanner::new(mdir.join("m.json"));

        let (moved, skipped) = planner.apply(&desktop).unwrap();
        assert_eq!((moved, skipped), (6, 0));
        assert!(planner.has_manifest());
        // 文件已入分类夹
        assert!(desktop.join("文档").join("报告.docx").is_file());
        assert!(desktop.join("图片").join("photo.JPG").is_file());
        assert!(!desktop.join("报告.docx").exists());
        // 快捷方式仍在桌面原位
        assert!(desktop.join("Chrome.lnk").is_file());

        // 还原
        let restored = planner.restore().unwrap();
        assert_eq!(restored, 6);
        assert!(desktop.join("报告.docx").is_file());
        assert!(!desktop.join("文档").join("报告.docx").exists());
        assert!(!planner.has_manifest(), "全部还原后 manifest 删除");
    }

    #[test]
    fn apply_skips_conflicts_never_overwrites() {
        let desktop = setup_desktop("conflict");
        // 预先创建同名目标文件
        std::fs::create_dir_all(desktop.join("文档")).unwrap();
        std::fs::write(desktop.join("文档").join("报告.docx"), b"existing").unwrap();

        let mdir = tmpdir("conflict-manifest");
        let planner = TidyPlanner::new(mdir.join("m.json"));
        let (moved, skipped) = planner.apply(&desktop).unwrap();
        assert_eq!(moved, 5);
        assert_eq!(skipped, 1);
        // 原文件未被覆盖
        assert_eq!(std::fs::read(desktop.join("文档").join("报告.docx")).unwrap(), b"existing");
        assert!(desktop.join("报告.docx").is_file(), "桌面原文件保留");
        // manifest 只含成功条目
        let manifest: TidyManifest =
            serde_json::from_slice(&std::fs::read(mdir.join("m.json")).unwrap()).unwrap();
        assert_eq!(manifest.moves.len(), 5);
    }

    #[test]
    fn restore_keeps_manifest_when_partial() {
        let desktop = setup_desktop("partial");
        let mdir = tmpdir("partial-manifest");
        let planner = TidyPlanner::new(mdir.join("m.json"));
        planner.apply(&desktop).unwrap();

        // 场景 A：原位被占（用户在桌面新建了同名文件）→ 该条目跳过且保留记录
        std::fs::write(desktop.join("报告.docx"), b"new").unwrap();
        let restored = planner.restore().unwrap();
        assert_eq!(restored, 5, "被占条目跳过，其余还原");
        assert!(planner.has_manifest(), "存在未还原条目时 manifest 保留");
        assert_eq!(
            std::fs::read(desktop.join("报告.docx")).unwrap(),
            b"new",
            "绝不覆盖用户新文件"
        );

        // 场景 B：占用解除后再还原 → 清空 manifest
        std::fs::remove_file(desktop.join("报告.docx")).unwrap();
        let restored2 = planner.restore().unwrap();
        assert_eq!(restored2, 1);
        assert!(!planner.has_manifest());
        assert!(desktop.join("报告.docx").is_file());
    }
}
