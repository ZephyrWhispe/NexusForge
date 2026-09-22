//! OCR 合并文本导出（T-B4-12）：写面只有 `{app_data}/export/` 一个目录。
//!
//! 两条纪律：
//! - **白名单外一律拒并点名原值**——`.sh` 落进导出目录等于给用户留一个"看起来是文本"的执行位；
//!   目录入参来自命令层的 `HostState.app_data_dir`（前端零目录入参），所以这条限制是结构性的，
//!   不靠调用方自觉。
//! - **先 `.tmp` 后 rename，失败即清 tmp**——中途失败留下的必须是 `.tmp`（或什么都没有），
//!   不能是一份"看起来完整"的半个导出（与 B3 `export_backup` 同规约）。

use std::path::{Path, PathBuf};

use host_core::error::AppError;
use host_core::util::now_ms;

/// 可导出的文本格式（前端两个导出钮逐一对应）
pub const EXPORT_FORMATS: &[&str] = &["txt", "md"];

/// 导出文件名（`seq` 是同毫秒内的递增序号：0 号即任务书字面的 `ocr-{ts_ms}.{format}`，
/// 撞名才加 `_1`、`_2`——两次同毫秒调用必得两个文件，而不是后者静默盖掉前者）
pub fn export_file_name(ts_ms: i64, seq: u32, format: &str) -> String {
    if seq == 0 {
        format!("ocr-{ts_ms}.{format}")
    } else {
        format!("ocr-{ts_ms}_{seq}.{format}")
    }
}

fn check_format(format: &str) -> Result<(), AppError> {
    if EXPORT_FORMATS.contains(&format) {
        return Ok(());
    }
    Err(AppError::module(
        "OCR_EXPORT_001",
        format!("不支持的导出格式：{format:?}"),
        Some(&format!(
            "白名单只有 {}：扩展名即落盘文件名的一部分，白名单外不开写口",
            EXPORT_FORMATS.join(" | ")
        )),
    ))
}

/// 合并文本落盘，返回最终路径（纯 IO，不碰模块运行态：文本由前端 `mergeOcrTexts` 交来）
pub fn write_export(app_data: &Path, text: &str, format: &str) -> Result<PathBuf, AppError> {
    check_format(format)?;
    if text.trim().is_empty() {
        return Err(AppError::module(
            "OCR_EXPORT_002",
            "导出文本为空，未生成文件",
            Some("先识别至少一张能出字的图片，再点导出"),
        ));
    }
    let dir = app_data.join("export");
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::module("OCR_EXPORT_003", format!("建导出目录失败: {e}"), None))?;
    let ts = now_ms();
    let mut seq = 0u32;
    let path = loop {
        let candidate = dir.join(export_file_name(ts, seq, format));
        if !candidate.try_exists().unwrap_or(false) {
            break candidate;
        }
        seq += 1;
    };
    let tmp = path.with_extension(format!("{}.{}.tmp", format, ts));
    write_then_rename(&tmp, &path, text.as_bytes())?;
    Ok(path)
}

/// 写 tmp → 改名到位；任一步失败都把 tmp 清掉（半成品不留在目录里冒充导出结果）
fn write_then_rename(tmp: &Path, path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    if let Err(e) = std::fs::write(tmp, bytes) {
        let _ = std::fs::remove_file(tmp);
        return Err(AppError::module(
            "OCR_EXPORT_003",
            format!("写导出文件失败: {e}"),
            None,
        ));
    }
    if let Err(e) = std::fs::rename(tmp, path) {
        let _ = std::fs::remove_file(tmp);
        return Err(AppError::module(
            "OCR_EXPORT_003",
            format!("导出文件改名落位失败: {e}"),
            None,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn err_code(e: &AppError) -> String {
        match e {
            AppError::Module { code, .. } => code.clone(),
            other => other.code().to_owned(),
        }
    }

    fn err_msg(e: &AppError) -> String {
        match e {
            AppError::Module { message, .. } => message.clone(),
            other => other.to_string(),
        }
    }

    fn err_hint(e: &AppError) -> String {
        match e {
            AppError::Module { hint, .. } => hint.clone().unwrap_or_default(),
            other => other.to_string(),
        }
    }

    /// 导出目录之外的文件清单（用于证明写面没漏到别处）
    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-12）字面测试名优先于 rustc 命名惯例
    fn ocrExport_formatWhitelist_rejectsOtherExtensions() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["sh", "", "txt /.."] {
            let e = write_export(dir.path(), "正文", bad).unwrap_err();
            assert_eq!(err_code(&e), "OCR_EXPORT_001", "格式 {bad:?} 应被白名单拒");
            assert!(
                err_msg(&e).contains(&format!("{bad:?}")),
                "错误消息须点名收到的原值（引号内），实际：{}",
                err_msg(&e)
            );
            assert!(
                err_hint(&e).contains("txt | md"),
                "拒写还要列出可用集合，用户才知该改成了什么，实际：{}",
                err_hint(&e)
            );
        }
        // 正对照：白名单内两形各真的落一个文件（否则上面的"拒"可以是空洞的永远拒）
        let txt = write_export(dir.path(), "正文", "txt").unwrap();
        let md = write_export(dir.path(), "正文", "md").unwrap();
        assert!(txt.file_name().unwrap().to_string_lossy().ends_with(".txt"));
        assert!(md.file_name().unwrap().to_string_lossy().ends_with(".md"));
    }

    #[test]
    #[allow(non_snake_case)]
    fn ocrExport_writesUnderAppDataExportOnly() {
        let dir = tempfile::tempdir().unwrap();
        let export = dir.path().join("export");
        std::fs::create_dir_all(dir.path().join("other")).unwrap();

        let first = write_export(dir.path(), "第一份", "txt").unwrap();
        let second = write_export(dir.path(), "第二份", "txt").unwrap();
        let prefix = export.display().to_string() + std::path::MAIN_SEPARATOR_STR;
        assert!(
            first.display().to_string().starts_with(&prefix),
            "返回值须落在 {{app_data}}\\export\\ 下，实际：{}",
            first.display()
        );
        assert_ne!(first, second, "同毫秒两次调用必得两个文件名");
        assert!(first.exists() && second.exists());
        assert_eq!(names_in(&export).len(), 2, "export 目录恰两份，不多不少");
        assert!(
            names_in(&dir.path().join("other")).is_empty(),
            "export 之外的目录不得出现新文件（写面结构性受限）"
        );
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "第二份");
    }

    #[test]
    #[allow(non_snake_case)]
    fn ocrExport_emptyText_rejects002() {
        let dir = tempfile::tempdir().unwrap();
        for blank in ["", "   \n "] {
            let e = write_export(dir.path(), blank, "md").unwrap_err();
            assert_eq!(err_code(&e), "OCR_EXPORT_002");
        }
        assert!(
            !dir.path().join("export").exists(),
            "空文本连目录都不该建，更不产空文件"
        );
        // 正对照：同样目录同样格式，非空即成功（证明上面拒的是空而非格式）
        assert!(write_export(dir.path(), "有正文", "md").is_ok());
    }

    #[test]
    #[allow(non_snake_case)]
    fn ocrExport_atomicRename_leavesNoTmp() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_export(dir.path(), "正文", "txt").unwrap();
        let leftovers: Vec<String> = names_in(&dir.path().join("export"))
            .into_iter()
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "成功路径不得残留 .tmp，实际：{leftovers:?}"
        );
        assert!(path.exists());

        // 假失败路径：改名目标位于不存在的子目录 → rename 必败，此时 tmp 也要被清掉
        let dir2 = tempfile::tempdir().unwrap();
        let tmp = dir2.path().join("a.txt.tmp");
        std::fs::write(&tmp, b"x").unwrap();
        let e =
            write_then_rename(&tmp, &dir2.path().join("missing").join("a.txt"), b"x").unwrap_err();
        assert_eq!(err_code(&e), "OCR_EXPORT_003");
        assert!(!tmp.exists(), "失败时不留半成品 tmp");
    }

    #[test]
    #[allow(non_snake_case)]
    fn exportFileName_seqZeroIsLiteralTemplate() {
        assert_eq!(
            export_file_name(1_700_000_000_000, 0, "md"),
            "ocr-1700000000000.md"
        );
        assert_eq!(
            export_file_name(1_700_000_000_000, 2, "md"),
            "ocr-1700000000000_2.md"
        );
    }
}
