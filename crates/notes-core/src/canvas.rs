//! N3 自由画布：`.nforge-canvas.json` 与 md 同目录。
//!
//! 画布属于目录（dir_rel 为空串即库根）；dir_rel 一律经 [`NoteLibrary::norm_rel`]
//! 校验（SEC-03：拒 `..`/绝对路径/盘符前缀，防越出库根读写）。
//! 画布文件损坏时 fail-closed（COR-23：报错保护，禁止以空画布覆盖保存）——
//! 以 md 为真相源，仅丢画布不丢笔记，但损坏必须可见而非静默清空。

use std::path::{Path, PathBuf};

use crate::error::{NoteError, Result};
use crate::library::NoteLibrary;
use crate::model::CanvasDoc;

/// 画布文件名（每目录一个）
pub const CANVAS_FILE: &str = ".nforge-canvas.json";

/// 画布文件路径：dir_rel 空串 = 库根；其余必须通过相对路径校验（SEC-03）
pub fn canvas_path(root: &Path, dir_rel: &str) -> Result<PathBuf> {
    let dir = if dir_rel.is_empty() {
        root.to_path_buf()
    } else {
        let norm = NoteLibrary::norm_rel(dir_rel)?;
        root.join(norm.replace('/', std::path::MAIN_SEPARATOR_STR))
    };
    Ok(dir.join(CANVAS_FILE))
}

/// 读取画布：不存在 → 空画布；损坏 → Err（fail-closed，防"读到坏文件→空画布→
/// 覆盖保存清空真数据"）
pub fn load(root: &Path, dir_rel: &str) -> Result<CanvasDoc> {
    let p = canvas_path(root, dir_rel)?;
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(CanvasDoc::default()),
        Err(e) => return Err(NoteError::Io(e)),
    };
    serde_json::from_slice::<CanvasDoc>(&bytes).map_err(|e| {
        tracing::warn!(path = %p.display(), error = %e, "画布损坏：拒绝以空画布替代");
        NoteError::Canvas(format!("画布文件损坏（已保护，禁止覆盖保存）：{e}"))
    })
}

/// 保存画布（tmp + rename 原子写；落盘前复核最终路径仍在库根内）
pub fn save(root: &Path, dir_rel: &str, doc: &CanvasDoc) -> Result<()> {
    let p = canvas_path(root, dir_rel)?;
    // 纵深防御：建目录后 canonicalize 复核父目录未越出库根
    let parent = p
        .parent()
        .ok_or_else(|| NoteError::BadPath("画布路径无父目录".into()))?;
    std::fs::create_dir_all(parent).map_err(NoteError::Io)?;
    let real_root = std::fs::canonicalize(root).map_err(NoteError::Io)?;
    let real_parent = std::fs::canonicalize(parent).map_err(NoteError::Io)?;
    if !real_parent.starts_with(&real_root) {
        return Err(NoteError::BadPath(format!("画布目录越出笔记库: {dir_rel}")));
    }
    let data = serde_json::to_vec_pretty(doc).map_err(|e| NoteError::Canvas(e.to_string()))?;
    let tmp = p.with_extension("nf-tmp");
    std::fs::write(&tmp, data).map_err(NoteError::Io)?;
    std::fs::rename(&tmp, &p).map_err(NoteError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CanvasEdge, CanvasNode};

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_notes_canvas_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample() -> CanvasDoc {
        CanvasDoc {
            version: 1,
            nodes: vec![CanvasNode {
                id: "n1".into(),
                kind: "sticky".into(),
                x: 10.0,
                y: 20.0,
                w: 180.0,
                h: 80.0,
                r#ref: None,
                text: Some("便签".into()),
                src: None,
                label: None,
            }],
            edges: vec![CanvasEdge {
                id: "e1".into(),
                from: "n1".into(),
                to: "n2".into(),
                label: None,
            }],
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let root = tmpdir("rt");
        let doc = sample();
        save(&root, "sub", &doc).unwrap();
        assert!(root.join("sub").join(CANVAS_FILE).exists());
        let loaded = load(&root, "sub").unwrap();
        assert_eq!(loaded.nodes.len(), 1);
        assert_eq!(loaded.nodes[0].text.as_deref(), Some("便签"));

        // 根目录（dir_rel 空）
        save(&root, "", &doc).unwrap();
        assert!(load(&root, "").unwrap().edges.len() == 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- SEC-03：路径穿越负例 ----

    #[test]
    fn canvas_path_rejects_traversal_and_absolute() {
        let root = tmpdir("guard");
        for bad in [
            "..",
            "..\\..\\Windows",
            "C:\\Windows",
            "C:/Windows",
            "\\\\server\\share",
            "a\\..\\..\\b",
            "/root-escape",
        ] {
            assert!(canvas_path(&root, bad).is_err(), "{bad} 必须被拒");
        }
        assert!(canvas_path(&root, "").is_ok(), "库根必须允许");
        assert!(canvas_path(&root, "sub/dir").is_ok(), "正常子目录必须允许");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn canvas_save_never_writes_outside_root() {
        let root = tmpdir("write_guard");
        let outside = root.parent().unwrap().join("nf_should_not_exist.json");
        let _ = std::fs::remove_file(&outside);
        assert!(save(&root, r"..\nf_should_not_exist", &sample()).is_err());
        assert!(!outside.exists(), "越界文件不得落盘");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&outside);
    }

    // ---- COR-23：损坏 fail-closed ----

    #[test]
    fn corrupt_canvas_is_error_not_empty() {
        let root = tmpdir("corrupt");
        save(&root, "sub", &sample()).unwrap();
        std::fs::write(root.join("sub").join(CANVAS_FILE), b"{broken").unwrap();
        assert!(load(&root, "sub").is_err(), "损坏画布必须报错而非静默空");
        // 不存在 → 空画布（正常路径不变）
        assert!(load(&root, "no-such-dir").unwrap().nodes.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
