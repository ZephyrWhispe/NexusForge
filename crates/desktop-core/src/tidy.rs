//! D3 桌面格子整理（v1 安全实现，docs/impl/05 D3）：
//! 不碰 Windows 桌面 ListView API —— 仅把桌面**普通文件**按扩展名归类，
//! 移入 `{桌面}/{分类名}/` 文件夹；快捷方式(.lnk/.url)与目录一律不动
//! （它们是软件入口，误移代价高）。移动映射落 manifest JSON，支持一键还原。
//!
//! 同名冲突跳过并记录（绝不覆盖用户文件）。
//!
//! T-B7-16 自定义分类映射（09 §7.2）：[`TidyMapping`] 经 desktop 段配置
//! `tidy_map` 键下发（null=内置六类，旧行为逐字不变）；命中自定义表则类名与
//! **目标夹均按表走**（目标夹须带盘符绝对路径——相对/UNC/引号在 validate 与
//! schema 双层拒存点名），未命中回落内置判定。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::error::{DesktopError, Result};

/// 分类中文名（文件夹名）
pub const CATEGORY_NAMES: &[&str] = &["文档", "图片", "压缩包", "音频", "视频", "其他"];

const DESKTOP_INI: &str = "desktop.ini";

/// 自定义分类映射（desktop 段 `tidy_map` 配置真源；JSON 形如
/// `{"categories": [["类名", "D:\\目标夹", ["ext1","ext2"]], …]}`）
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TidyMapping {
    /// (类名, 目标夹, 扩展名小写表)——声明序即路由优先序（先声明者先命中）
    pub categories: Vec<(String, String, Vec<String>)>,
}

impl TidyMapping {
    /// 写侧/派发侧共用校验：非法即 Err 点名（相对路径/UNC/引号尖括号/空名/
    /// 空扩展名 token/类名重复——目标夹必须带盘符绝对路径是红线：
    /// 整理=移动用户文件，落点含糊宁可拒存也不猜）
    pub fn validate(&self) -> std::result::Result<(), String> {
        let mut seen: Vec<&str> = Vec::new();
        for (name, folder, exts) in &self.categories {
            if name.trim().is_empty() {
                return Err("分类名不得为空".into());
            }
            if name.contains('/') || name.contains('\\') {
                return Err(format!("分类名不得含路径分隔符: {name}"));
            }
            if seen.contains(&name.as_str()) {
                return Err(format!("分类名重复: {name}"));
            }
            seen.push(name);
            if !is_drive_absolute(folder) {
                return Err(format!(
                    "目标夹必须是带盘符的绝对路径（相对路径与 UNC \\\\ 均拒）: {folder:?}"
                ));
            }
            if folder
                .chars()
                .any(|c| matches!(c, '"' | '\'' | '<' | '>' | '|' | '?'))
            {
                return Err(format!("目标夹不得含引号/尖括号/管道符: {folder:?}"));
            }
            if exts.is_empty() {
                return Err(format!("分类 {name} 未声明任何扩展名"));
            }
            for e in exts {
                let t = e.trim();
                if t.is_empty() || t.contains('.') || t.chars().any(char::is_whitespace) {
                    return Err(format!("非法扩展名 token {e:?}（分类 {name}，不带点不空）"));
                }
            }
        }
        Ok(())
    }
}

/// 带盘符绝对路径判定（`D:\…` / `D:/…`；相对、UNC、`C:` 裸盘符均 false）
fn is_drive_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 4 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/')
}

/// 分类判定：返回中文分类名
pub fn categorize(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "doc" | "docx" | "pdf" | "txt" | "md" | "xls" | "xlsx" | "ppt" | "pptx" | "csv" | "rtf"
        | "epub" => "文档",
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "svg" | "ico" | "heic" | "tif"
        | "tiff" => "图片",
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "cab" | "iso" => "压缩包",
        "mp3" | "wav" | "flac" | "ape" | "ogg" | "m4a" | "aac" | "wma" => "音频",
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" => "视频",
        _ => "其他",
    }
}

/// 自定义表判定（大小写不敏感）：Some=命中表内类名；None=表缺失（含 map=None
/// 恒走内置六类的旧行为面）或未声明该扩展名——调用方回落 [`categorize`]
pub fn categorize_with(ext: &str, map: Option<&TidyMapping>) -> Option<String> {
    let lower = ext.to_ascii_lowercase();
    map?.categories
        .iter()
        .find(|(_, _, exts)| exts.iter().any(|e| e.to_ascii_lowercase() == lower))
        .map(|(name, _, _)| name.clone())
}

/// 完整判定链：自定义表优先，未命中回落内置——map=None 时逐字等于 categorize
pub fn resolve_category(ext: &str, map: Option<&TidyMapping>) -> String {
    categorize_with(ext, map).unwrap_or_else(|| categorize(ext).to_string())
}

/// 桌面待整理文件
#[derive(Clone, Debug, Serialize)]
pub struct DesktopItem {
    pub name: String,
    pub path: String,
    /// 分类名（内置 CATEGORY_NAMES 之一，或 tidy_map 自定义类名）
    pub category: String,
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
    /// COR-19：0 = 预登记（尚未完成）；>0 = 提交时间。崩溃后 restore 仍可按
    /// from→to 反向恢复已移动部分（serde default 旧文件零迁移）
    #[serde(default)]
    pub committed: bool,
}

pub struct TidyPlanner {
    /// manifest 存放路径（{appData}/desktop/tidy_manifest.json）
    manifest_path: PathBuf,
    /// 自定义分类映射活引用（模块持真源；apply_config 覆写=不重启生效，
    /// T-B7-16 `tidyMap_runtimeApply_noRestart` 经 Arc::ptr_eq 钉派发）
    tidy_map: Arc<RwLock<Option<TidyMapping>>>,
}

impl TidyPlanner {
    pub fn new(manifest_path: PathBuf, tidy_map: Arc<RwLock<Option<TidyMapping>>>) -> Self {
        Self {
            manifest_path,
            tidy_map,
        }
    }

    /// 扫描桌面普通文件生成计划（.lnk/.url/目录/隐藏系统文件不动）
    pub fn plan(&self, desktop: &Path) -> Result<TidyPlan> {
        let map = self.tidy_map.read().clone();
        let items = scan_desktop(desktop, map.as_ref())?;
        let mut by_cat: BTreeMap<String, Vec<DesktopItem>> = BTreeMap::new();
        for item in items {
            by_cat.entry(item.category.clone()).or_default().push(item);
        }
        let groups = by_cat.into_iter().collect::<Vec<_>>();
        let total = groups.iter().map(|(_, v)| v.len()).sum();
        Ok(TidyPlan { groups, total })
    }

    /// 执行整理：建分类文件夹 + 移动；manifest 落盘后才算成功。
    /// 目标夹解析：tidy_map 命中类名 ⇒ 表内绝对路径；否则 `{桌面}/{分类名}`（旧行为）。
    /// 返回 (移动数, 跳过数)。
    ///
    /// COR-19：manifest **先写"预登记"再移动**——旧顺序（先移后写）在 manifest
    /// 写失败时文件已散落而 restore() 的唯一依据不存在，用户看到失败却无法还原。
    pub fn apply(&self, desktop: &Path) -> Result<(usize, usize)> {
        let plan = self.plan(desktop)?;
        if plan.total == 0 {
            return Ok((0, 0));
        }
        let map = self.tidy_map.read().clone();

        // COR-19 ①：预登记 manifest（committed = false 表示尚未完成，崩溃后
        // restore() 仍可按 from→to 反向恢复已移动部分）
        let pre = TidyManifest {
            moves: plan
                .groups
                .iter()
                .flat_map(|(cat, items)| items.iter().map(move |it| (cat.clone(), it.clone())))
                .map(|(cat, it)| {
                    let target_dir = map
                        .as_ref()
                        .and_then(|m| {
                            m.categories
                                .iter()
                                .find(|(n, _, _)| n == &cat)
                                .map(|(_, folder, _)| PathBuf::from(folder))
                        })
                        .unwrap_or_else(|| desktop.join(&cat));
                    MovedEntry {
                        from: it.path.clone(),
                        to: target_dir.join(&it.name).display().to_string(),
                    }
                })
                .collect(),
            applied_ms: 0,
            committed: false,
        };
        self.write_manifest(&pre)?;

        let mut moves = Vec::new();
        let mut skipped = 0usize;
        for (category, items) in &plan.groups {
            let target_dir = map
                .as_ref()
                .and_then(|m| {
                    m.categories
                        .iter()
                        .find(|(n, _, _)| n == category)
                        .map(|(_, folder, _)| PathBuf::from(folder))
                })
                .unwrap_or_else(|| desktop.join(category));
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
            // 全部跳过：预登记 manifest 作废（清掉，避免 restore 拿到空计划）
            let _ = std::fs::remove_file(&self.manifest_path);
            return Ok((0, skipped));
        }
        // COR-19 ②：提交态 manifest（applied_ms 落定）
        let manifest = TidyManifest {
            moves,
            applied_ms: now_ms(),
            committed: true,
        };
        self.write_manifest(&manifest)?;
        Ok((manifest.moves.len(), skipped))
    }

    /// manifest 唯一写点（预登记与提交态共用）
    fn write_manifest(&self, manifest: &TidyManifest) -> Result<()> {
        if let Some(parent) = self.manifest_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DesktopError::Tidy(format!("manifest 目录创建失败: {e}")))?;
        }
        let tmp = self.manifest_path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(manifest)?)
            .map_err(|e| DesktopError::Tidy(format!("manifest 写入失败: {e}")))?;
        std::fs::rename(&tmp, &self.manifest_path)
            .map_err(|e| DesktopError::Tidy(format!("manifest 写入失败: {e}")))?;
        Ok(())
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
            let m = TidyManifest {
                moves: remaining,
                applied_ms: manifest.applied_ms,
                committed: manifest.committed,
            };
            std::fs::write(&self.manifest_path, serde_json::to_vec(&m)?)?;
        }
        Ok(restored)
    }

    /// 映射 Arc 引用（T-B7-16 派发证明用：Arc::ptr_eq 钉"planner 与模块真源同锁"）
    pub fn map_ref(&self) -> &Arc<RwLock<Option<TidyMapping>>> {
        &self.tidy_map
    }

    /// 是否有待还原的整理记录
    pub fn has_manifest(&self) -> bool {
        self.manifest_path.exists()
    }
}

/// 扫描桌面普通文件（排除快捷方式/目录/隐藏/desktop.ini）；
/// map=Some 时自定义表优先路由，未命中回落内置六类
fn scan_desktop(desktop: &Path, map: Option<&TidyMapping>) -> Result<Vec<DesktopItem>> {
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
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        items.push(DesktopItem {
            name: name.to_string(),
            path: path.display().to_string(),
            category: resolve_category(ext, map),
        });
    }
    Ok(items)
}

use host_core::util::now_ms;

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        // pid 会被 Windows 复用：纳秒盐 + 先清场（同 sys-core 夹具纪律）
        let salt = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let d = std::env::temp_dir().join(format!(
            "nf_desktop_tidy_{tag}_{}_{salt}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn no_map() -> Arc<RwLock<Option<TidyMapping>>> {
        Arc::new(RwLock::new(None))
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
        let planner = TidyPlanner::new(tmpdir("plan-manifest").join("m.json"), no_map());
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
        let planner = TidyPlanner::new(mdir.join("m.json"), no_map());

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
        let planner = TidyPlanner::new(mdir.join("m.json"), no_map());
        let (moved, skipped) = planner.apply(&desktop).unwrap();
        assert_eq!(moved, 5);
        assert_eq!(skipped, 1);
        // 原文件未被覆盖
        assert_eq!(
            std::fs::read(desktop.join("文档").join("报告.docx")).unwrap(),
            b"existing"
        );
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
        let planner = TidyPlanner::new(mdir.join("m.json"), no_map());
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

    fn map_of(pairs: Vec<(&str, &str, &[&str])>) -> TidyMapping {
        TidyMapping {
            categories: pairs
                .into_iter()
                .map(|(n, f, e)| {
                    (
                        n.to_string(),
                        f.to_string(),
                        e.iter().map(|s| s.to_string()).collect(),
                    )
                })
                .collect(),
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-16）字面测试名优先于 rustc 命名惯例
    fn categorizeWith_customMap_routesExtension() {
        let map = map_of(vec![
            ("设计稿", "D:\\Design", &["psd", "sketch"]),
            ("文档2", "E:\\Docs", &["md"]),
        ]);
        assert_eq!(
            categorize_with("PSD", Some(&map)).as_deref(),
            Some("设计稿")
        );
        assert_eq!(
            categorize_with("sketch", Some(&map)).as_deref(),
            Some("设计稿"),
            "大小写不敏感"
        );
        assert_eq!(
            categorize_with("md", Some(&map)).as_deref(),
            Some("文档2"),
            "表内声明即自定义优先（覆盖内置「文档」）"
        );
        assert_eq!(
            categorize_with("pdf", Some(&map)),
            None,
            "未声明 ⇒ 无自定义意见"
        );
        // 完整链：自定义表未命中的扩展名回落内置
        assert_eq!(resolve_category("pdf", Some(&map)), "文档");
        assert_eq!(resolve_category("xyz", Some(&map)), "其他");
    }

    #[test]
    #[allow(non_snake_case)]
    fn categorizeWith_noneEqualsBuiltinSix() {
        // 正对照逐字：map=None 六类每臂恒内置——自定义面零扰动
        for (ext, want) in [
            ("pdf", "文档"),
            ("JPG", "图片"),
            ("7z", "压缩包"),
            ("flac", "音频"),
            ("MKV", "视频"),
            ("xyz", "其他"),
        ] {
            assert_eq!(
                categorize_with(ext, None),
                None,
                "{ext}：map=None 不得产出路由"
            );
            assert_eq!(resolve_category(ext, None), want, "{ext}：内置逐字不变");
        }
    }

    #[test]
    #[allow(non_snake_case)]
    fn tidyMap_illegalTargetFolder_rejected() {
        // 相对臂
        let e = map_of(vec![("文档", "Docs\\sub", &["pdf"])])
            .validate()
            .unwrap_err();
        assert!(
            e.contains("Docs") && e.contains("绝对路径"),
            "相对路径点名: {e}"
        );
        // UNC 臂
        let e = map_of(vec![("文档", "\\\\srv\\share", &["pdf"])])
            .validate()
            .unwrap_err();
        assert!(e.contains("绝对路径"), "UNC 点名: {e}");
        // 绝对但含引号
        let e = map_of(vec![("文档", "D:\\a\"b", &["pdf"])])
            .validate()
            .unwrap_err();
        assert!(e.contains("引号"), "引号点名: {e}");
        // 空扩展名 token / 带点扩展名
        assert!(map_of(vec![("文档", "D:\\d", &[""])])
            .validate()
            .unwrap_err()
            .contains("扩展名"));
        assert!(map_of(vec![("文档", "D:\\d", &[".pdf"])])
            .validate()
            .unwrap_err()
            .contains("扩展名"));
        // 类名重复（目标夹反演必须唯一）
        let e = map_of(vec![
            ("文档", "D:\\a", &["pdf"]),
            ("文档", "D:\\b", &["txt"]),
        ])
        .validate()
        .unwrap_err();
        assert!(e.contains("重复"), "重复类名点名: {e}");
        // 合法表放行
        assert!(map_of(vec![("设计稿", "D:\\Design", &["psd"])])
            .validate()
            .is_ok());
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-16）字面测试名优先于 rustc 命名惯例
    fn apply_routesCustomCategory_toDeclaredFolder() {
        let desktop = setup_desktop("custom-apply");
        let mdir = tmpdir("custom-apply-manifest");
        let out = tmpdir("custom-apply-target");
        let folder = out.join("设计归档").display().to_string();
        let map = Arc::new(RwLock::new(Some(TidyMapping {
            categories: vec![("未知".to_string(), folder.clone(), vec!["xyz".to_string()])],
        })));
        let planner = TidyPlanner::new(mdir.join("m.json"), map);
        let (moved, skipped) = planner.apply(&desktop).unwrap();
        assert_eq!((moved, skipped), (6, 0));
        // 自定义类收 xyz 且落表内目标夹（非 {桌面}/未知）
        assert!(PathBuf::from(&folder).join("unknown.xyz").is_file());
        assert!(!desktop.join("未知").exists());
        // 内置类仍走 {桌面}/{类名} 旧路径
        assert!(desktop.join("文档").join("报告.docx").is_file());
        // manifest 形状不变 ⇒ 还原照样回原位
        assert_eq!(planner.restore().unwrap(), 6);
        assert!(desktop.join("unknown.xyz").is_file());
    }
}
