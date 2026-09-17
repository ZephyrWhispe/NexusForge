//! SY3 系统清理（docs/impl/06 SY3）：扫描 → 汇总 → 确认 → 执行。
//!
//! 白名单（docs/impl/06 SY 风险标注）：最近 24h 修改的文件默认跳过；
//! 仅处理内置目录清单内的文件（不碰系统文件）；执行可走回收站
//! （RecycleBinPort，误删可恢复——阶段三验收项）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SysError};

/// 白名单阈值：24h 内修改跳过
pub const RECENT_SKIP: Duration = Duration::from_secs(24 * 3600);

/// 清理目标（内置清单；need_admin 的目标需管理员运行）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanTarget {
    pub id: &'static str,
    pub label: &'static str,
    pub dir: String,
    /// 仅清理该扩展名（空 = 全部文件）；防误删用
    #[serde(default)]
    pub exts: Vec<String>,
    pub need_admin: bool,
    pub safe_default: bool,
    /// 目录不存在时静默跳过
    pub optional: bool,
}

/// 内置清理清单（临时目录 / 更新缓存 / 缩略图缓存；回收站清空走独立 IPC）
pub fn builtin_targets() -> Vec<CleanTarget> {
    let temp = std::env::var("TEMP").unwrap_or_default();
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    vec![
        CleanTarget {
            id: "user_temp",
            label: "用户临时文件（%TEMP%）",
            dir: temp,
            exts: vec![],
            need_admin: false,
            safe_default: true,
            optional: false,
        },
        CleanTarget {
            id: "windows_temp",
            label: "系统临时文件（C:\\Windows\\Temp，需管理员）",
            dir: r"C:\Windows\Temp".into(),
            exts: vec![],
            need_admin: true,
            safe_default: false,
            optional: true,
        },
        CleanTarget {
            id: "update_cache",
            label: "Windows 更新下载缓存（需管理员）",
            dir: r"C:\Windows\SoftwareDistribution\Download".into(),
            exts: vec![],
            need_admin: true,
            safe_default: false,
            optional: true,
        },
        CleanTarget {
            id: "thumb_cache",
            label: "缩略图/图标缓存（占用中文件自动跳过）",
            dir: format!(r"{local}\Microsoft\Windows\Explorer"),
            exts: vec!["db".into()],
            need_admin: false,
            safe_default: false,
            optional: true,
        },
    ]
}

/// 扫描结果（IPC DTO）
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CleanScanItem {
    pub target_id: String,
    pub label: String,
    pub need_admin: bool,
    pub safe_default: bool,
    /// 可清理文件数（白名单过滤后）
    pub files: u64,
    /// 可回收字节
    pub reclaim_bytes: u64,
    /// 白名单跳过数
    pub skipped_recent: u64,
    pub missing: bool,
}

/// 扫描单个目标（白名单：24h 内修改跳过；子目录递归）
pub fn scan_target(target: &CleanTarget, now_ms: i64) -> Result<CleanScanItem> {
    let dir = PathBuf::from(&target.dir);
    let mut item = CleanScanItem {
        target_id: target.id.to_string(),
        label: target.label.to_string(),
        need_admin: target.need_admin,
        safe_default: target.safe_default,
        files: 0,
        reclaim_bytes: 0,
        skipped_recent: 0,
        missing: !dir.exists(),
    };
    if item.missing {
        if target.optional {
            return Ok(item);
        }
        return Err(SysError::CleanTarget(target.dir.clone()));
    }
    scan_dir(&dir, target, now_ms, &mut item);
    Ok(item)
}

fn scan_dir(dir: &Path, target: &CleanTarget, now_ms: i64, item: &mut CleanScanItem) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            scan_dir(&path, target, now_ms, item);
        } else {
            // 扩展名过滤（目标声明时）
            if !target.exts.is_empty() {
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if !target.exts.iter().any(|e| e.eq_ignore_ascii_case(&ext)) {
                    continue;
                }
            }
            // 24h 白名单
            let recent = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .map(|ms| now_ms - ms < RECENT_SKIP.as_millis() as i64)
                .unwrap_or(false);
            if recent {
                item.skipped_recent += 1;
                continue;
            }
            item.files += 1;
            item.reclaim_bytes += meta.len();
        }
    }
}

/// 执行清理：collect → 删除（recycle=true 走回收站）；返回 (删除数, 字节)
pub fn execute_target(
    target: &CleanTarget,
    now_ms: i64,
    recycle: bool,
    recycle_delete: &dyn Fn(&[PathBuf]) -> Result<u32>,
) -> Result<(u64, u64)> {
    let dir = PathBuf::from(&target.dir);
    if !dir.exists() {
        return Err(SysError::CleanTarget(target.dir.clone()));
    }
    let mut scan = CleanScanItem { files: 0, reclaim_bytes: 0, ..Default::default() };
    scan_dir(&dir, target, now_ms, &mut scan);
    // 再次收集具体文件路径（与扫描同白名单）
    let mut paths = Vec::new();
    collect_files(&dir, target, now_ms, &mut paths);
    let bytes = scan.reclaim_bytes;
    if paths.is_empty() {
        return Ok((0, 0));
    }
    if recycle {
        recycle_delete(&paths)?;
    } else {
        for p in &paths {
            let _ = std::fs::remove_file(p); // 占用/权限错误静默跳过（更新缓存被进程锁定的文件）
        }
    }
    Ok((paths.len() as u64, bytes))
}

fn collect_files(dir: &Path, target: &CleanTarget, now_ms: i64, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            collect_files(&path, target, now_ms, out);
        } else {
            if !target.exts.is_empty() {
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if !target.exts.iter().any(|e| e.eq_ignore_ascii_case(&ext)) {
                    continue;
                }
            }
            let recent = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .map(|ms| now_ms - ms < RECENT_SKIP.as_millis() as i64)
                .unwrap_or(false);
            if !recent {
                out.push(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_sys_clean_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    fn touch(path: &Path, size: usize) {
        std::fs::write(path, vec![0u8; size]).unwrap();
    }

    #[test]
    fn scan_respects_whitelist_and_exts() {
        let d = tmpdir("scan");
        let t = CleanTarget {
            id: "test",
            label: "测试",
            dir: d.to_string_lossy().into_owned(),
            exts: vec!["tmp".into()],
            need_admin: false,
            safe_default: true,
            optional: false,
        };
        touch(&d.join("a.tmp"), 100);
        touch(&d.join("b.log"), 500); // 扩展名过滤
        std::fs::create_dir(d.join("sub")).unwrap();
        touch(&d.join("sub/c.tmp"), 200);
        // 24h 白名单：旧文件计入
        let old = d.join("old.tmp");
        touch(&old, 50);
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 3600);
        let f = std::fs::File::options().write(true).open(&old).unwrap();
        f.set_modified(past).unwrap();

        let item = scan_target(&t, now()).unwrap();
        // a.tmp 与 sub/c.tmp 刚创建 → 24h 白名单跳过；仅 old.tmp 计入
        assert_eq!(item.files, 1);
        assert_eq!(item.reclaim_bytes, 50);
        assert_eq!(item.skipped_recent, 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn recent_files_skipped() {
        let d = tmpdir("recent");
        let t = CleanTarget {
            id: "test",
            label: "测试",
            dir: d.to_string_lossy().into_owned(),
            exts: vec![],
            need_admin: false,
            safe_default: true,
            optional: false,
        };
        touch(&d.join("fresh.txt"), 10);
        let item = scan_target(&t, now()).unwrap();
        assert_eq!(item.files, 0);
        assert_eq!(item.skipped_recent, 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn execute_deletes_collected_files() {
        let d = tmpdir("exec");
        let t = CleanTarget {
            id: "test",
            label: "测试",
            dir: d.to_string_lossy().into_owned(),
            exts: vec![],
            need_admin: false,
            safe_default: true,
            optional: false,
        };
        // 旧文件（可删）+ 新文件（白名单保护）
        let old = d.join("old.log");
        touch(&old, 64);
        let f = std::fs::File::options().write(true).open(&old).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 3600))
            .unwrap();
        touch(&d.join("fresh.log"), 32);

        let (deleted, _bytes) = execute_target(&t, now(), false, &|_| Ok(0)).unwrap();
        assert_eq!(deleted, 1);
        assert!(!old.exists());
        assert!(d.join("fresh.log").exists()); // 白名单保护
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_target_errors_unless_optional() {
        let t = CleanTarget {
            id: "missing",
            label: "不存在",
            dir: r"X:\nf_nonexistent_dir".into(),
            exts: vec![],
            need_admin: false,
            safe_default: false,
            optional: false,
        };
        assert!(scan_target(&t, now()).is_err());
        let t2 = CleanTarget { optional: true, ..t };
        assert!(scan_target(&t2, now()).unwrap().missing);
    }
}
