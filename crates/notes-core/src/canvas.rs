//! N3 自由画布：`.nforge-canvas.json` 与 md 同目录。
//!
//! 画布属于目录（dir_rel 为空串即库根）；文件损坏时按空画布处理——
//! 以 md 为真相源，仅丢画布不丢笔记（docs/impl/06 风险标注）。

use std::path::{Path, PathBuf};

use crate::error::{NoteError, Result};
use crate::model::CanvasDoc;

/// 画布文件名（每目录一个）
pub const CANVAS_FILE: &str = ".nforge-canvas.json";

pub fn canvas_path(root: &Path, dir_rel: &str) -> PathBuf {
    let dir = if dir_rel.is_empty() { root.to_path_buf() } else { root.join(dir_rel.replace('/', "\\")) };
    dir.join(CANVAS_FILE)
}

/// 读取画布：不存在或损坏 → 空画布
pub fn load(root: &Path, dir_rel: &str) -> CanvasDoc {
    let p = canvas_path(root, dir_rel);
    let Ok(bytes) = std::fs::read(&p) else {
        return CanvasDoc::default();
    };
    serde_json::from_slice::<CanvasDoc>(&bytes).unwrap_or_default()
}

/// 保存画布（tmp + rename 原子写）
pub fn save(root: &Path, dir_rel: &str, doc: &CanvasDoc) -> Result<()> {
    let p = canvas_path(root, dir_rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(NoteError::Io)?;
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

    #[test]
    fn save_load_roundtrip_and_corrupt_fallback() {
        let root = tmpdir("rt");
        let doc = CanvasDoc {
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
            edges: vec![CanvasEdge { id: "e1".into(), from: "n1".into(), to: "n2".into(), label: None }],
        };
        save(&root, "sub", &doc).unwrap();
        assert!(root.join("sub").join(CANVAS_FILE).exists());
        let loaded = load(&root, "sub");
        assert_eq!(loaded.nodes.len(), 1);
        assert_eq!(loaded.nodes[0].text.as_deref(), Some("便签"));

        // 损坏 → 空画布
        std::fs::write(root.join("sub").join(CANVAS_FILE), b"{broken").unwrap();
        assert!(load(&root, "sub").nodes.is_empty());

        // 根目录（dir_rel 空）
        save(&root, "", &doc).unwrap();
        assert!(load(&root, "").edges.len() == 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
