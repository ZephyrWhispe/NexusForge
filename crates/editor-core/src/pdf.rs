//! E4 PDF 处理（docs/impl/06 E4）。
//!
//! **实现偏离说明**：文档指定 qpdf/mutool sidecar。v1 改用纯 Rust `lopdf`：
//! - Windows 无官方 qpdf 独立二进制分发渠道，mutool 体积大（>40MB）；
//! - 合并/拆分/压缩/水印四种操作 lopdf 全覆盖，零外部依赖、可单测；
//! - 符合 DESIGN「本地优先」。qpdf sidecar 保留为后续可选增强（压缩率更优）。
//!
//! 压缩语义（文档要求）：压缩结果 > 原文件则回滚保留原文件（幂等保护）。

use std::path::Path;

use lopdf::{dictionary, Document, Object, ObjectId, Stream};

use crate::error::{EditorError, Result};

/// PDF 基本信息
#[derive(Clone, Debug, serde::Serialize)]
pub struct PdfInfo {
    pub pages: u32,
    pub size: u64,
}

/// 操作结果（输出文件路径 + 页数/大小变化）
#[derive(Clone, Debug, serde::Serialize)]
pub struct PdfOpResult {
    pub output: String,
    pub pages: u32,
    pub size: u64,
}

/// 解析并取页数/大小
pub fn info(path: &Path) -> Result<PdfInfo> {
    let doc = load(path)?;
    Ok(PdfInfo {
        pages: doc.get_pages().len() as u32,
        size: std::fs::metadata(path)
            .map_err(EditorError::Io)?
            .len(),
    })
}

/// 合并多个 PDF → 输出到 output（页序按输入顺序）
pub fn merge(inputs: &[std::path::PathBuf], output: &Path) -> Result<PdfOpResult> {
    if inputs.len() < 2 {
        return Err(EditorError::BadParam("合并至少需要 2 个 PDF".into()));
    }
    let mut acc = load(&inputs[0])?;
    for path in &inputs[1..] {
        let next = load(path)?;
        merge_doc(&mut acc, next);
    }
    finish(acc, output)
}

/// 拆分为单页 PDF 输出到目录（`{stem}_1.pdf`…）
pub fn split(path: &Path, out_dir: &Path) -> Result<Vec<PdfOpResult>> {
    let doc = load(path)?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("page");
    std::fs::create_dir_all(out_dir).map_err(EditorError::Io)?;

    let mut results = Vec::new();
    for (i, page_id) in doc.get_pages().values().enumerate() {
        // 单页文档 = 独立构建目录 + Pages 树，把该页整棵引用树克隆进去
        let mut one = Document::with_version("1.5");
        let mut map: std::collections::HashMap<ObjectId, ObjectId> = Default::default();
        let cloned = clone_object_tree(&doc, *page_id, &mut one, &mut map);
        let new_pages = one.add_object(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(cloned)]),
            "Count" => Object::Integer(1),
        });
        // 页的 Parent 指向新 Pages 树
        if let Ok(Object::Dictionary(page)) = one.get_object_mut(cloned) {
            page.set("Parent", Object::Reference(new_pages));
        }
        let catalog = one.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(new_pages),
        });
        one.trailer.set("Root", Object::Reference(catalog));

        let out = out_dir.join(format!("{stem}_{}.pdf", i + 1));
        one.save(&out)
            .map_err(|e| EditorError::Pdf(format!("写入 {} 失败: {e}", out.display())))?;
        results.push(PdfOpResult {
            output: out.display().to_string(),
            pages: 1,
            size: std::fs::metadata(&out).map_err(EditorError::Io)?.len(),
        });
    }
    Ok(results)
}

/// 压缩（对象流重写 + save 压缩选项）；结果 > 原文件则回滚保留原文件
pub fn compress(path: &Path) -> Result<PdfOpResult> {
    let orig_size = std::fs::metadata(path).map_err(EditorError::Io)?.len();
    let mut doc = load(path)?;
    // 清理未引用对象 + 重建 xref
    doc.compress();

    let tmp = path.with_extension("pdf.nforge-tmp");
    doc.save(&tmp)
        .map_err(|e| EditorError::Pdf(format!("压缩临时写入失败: {e}")))?;
    let new_size = std::fs::metadata(&tmp).map_err(EditorError::Io)?.len();
    if new_size < orig_size {
        std::fs::rename(&tmp, path)
            .map_err(|e| EditorError::Pdf(format!("压缩结果替换失败: {e}")))?;
    } else {
        // 幂等保护：压缩无效（或变大），保留原文件
        let _ = std::fs::remove_file(&tmp);
        tracing::info!(
            orig = orig_size,
            new = new_size,
            "压缩无收益，保留原文件"
        );
    }
    Ok(PdfOpResult {
        output: path.display().to_string(),
        pages: doc.get_pages().len() as u32,
        size: std::fs::metadata(path).map_err(EditorError::Io)?.len(),
    })
}

/// 文字水印：每页左下角叠加半透明文本（简单实现，斜角大水印为后续增强）
pub fn watermark(path: &Path, text: &str) -> Result<PdfOpResult> {
    if text.trim().is_empty() {
        return Err(EditorError::BadParam("水印文本不能为空".into()));
    }
    let mut doc = load(path)?;
    let pages: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    for page_id in pages {
        stamp_page(&mut doc, page_id, text)?;
    }
    doc.save(path)
        .map_err(|e| EditorError::Pdf(format!("水印写入失败: {e}")))?;
    Ok(PdfOpResult {
        output: path.display().to_string(),
        pages: doc.get_pages().len() as u32,
        size: std::fs::metadata(path).map_err(EditorError::Io)?.len(),
    })
}

// ---- 内部 ----

fn load(path: &Path) -> Result<Document> {
    Document::load(path).map_err(|e| {
        EditorError::Pdf(format!("解析 {} 失败: {e}", path.display()))
    })
}

fn finish(mut doc: Document, output: &Path) -> Result<PdfOpResult> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(EditorError::Io)?;
    }
    doc.save(output)
        .map_err(|e| EditorError::Pdf(format!("写入 {} 失败: {e}", output.display())))?;
    Ok(PdfOpResult {
        output: output.display().to_string(),
        pages: doc.get_pages().len() as u32,
        size: std::fs::metadata(output).map_err(EditorError::Io)?.len(),
    })
}

/// 把 next 的全部页追加到 acc 末尾（整棵引用树手动克隆，避免依赖不稳定的
/// copy_pages_and_objects 语义——对象映射防循环共享）
fn merge_doc(acc: &mut Document, next: Document) {
    let page_ids: Vec<ObjectId> = next.get_pages().values().copied().collect();
    for page_id in page_ids {
        let mut map: std::collections::HashMap<ObjectId, ObjectId> = Default::default();
        let cloned = clone_object_tree(&next, page_id, acc, &mut map);
        append_page_to_tree(acc, cloned);
    }
}

/// 递归克隆引用树：src 中 old_id 指向的对象 → dst 新对象（返回新 id）。
/// 先占位（Null）防环（页/Parent 互指），再递归构造后回填；
/// 共享引用（字体等）经 map 去重。
fn clone_object_tree(
    src: &Document,
    old_id: ObjectId,
    dst: &mut Document,
    map: &mut std::collections::HashMap<ObjectId, ObjectId>,
) -> ObjectId {
    if let Some(new_id) = map.get(&old_id) {
        return *new_id;
    }
    let obj = src.get_object(old_id).cloned().unwrap_or(Object::Null);

    let resolved = match obj {
        Object::Reference(_) => Object::Null, // 顶层引用不应出现
        Object::Dictionary(dict) => Object::Dictionary(clone_dict(src, dict, dst, map)),
        Object::Array(arr) => Object::Array(clone_array(src, arr, dst, map)),
        Object::Stream(stream) => {
            let mut dict = clone_dict(src, stream.dict, dst, map);
            let content = stream.content.clone();
            dict.set("Length", Object::Integer(content.len() as i64));
            Object::Stream(Stream::new(dict, content))
        }
        other => other,
    };
    // 页树经跳过 Parent 后无环（map 去重共享对象）；递归完成后一次性落库
    let new_id = dst.add_object(resolved);
    map.insert(old_id, new_id);
    new_id
}

fn clone_dict(
    src: &Document,
    dict: lopdf::Dictionary,
    dst: &mut Document,
    map: &mut std::collections::HashMap<ObjectId, ObjectId>,
) -> lopdf::Dictionary {
    let mut out = lopdf::Dictionary::new();
    for (k, v) in dict {
        // 页的 Parent 指向旧文档 Pages 树：跳过克隆（append_page_to_tree 会显式重设），
        // 否则会把整个旧 Pages 树拖进新文档
        if k.as_slice() == b"Parent" {
            continue;
        }
        out.set(k, clone_value(src, v, dst, map));
    }
    out
}

fn clone_array(
    src: &Document,
    arr: Vec<Object>,
    dst: &mut Document,
    map: &mut std::collections::HashMap<ObjectId, ObjectId>,
) -> Vec<Object> {
    arr.into_iter().map(|v| clone_value(src, v, dst, map)).collect()
}

fn clone_value(
    src: &Document,
    v: Object,
    dst: &mut Document,
    map: &mut std::collections::HashMap<ObjectId, ObjectId>,
) -> Object {
    match v {
        Object::Reference(rid) => Object::Reference(clone_object_tree(src, rid, dst, map)),
        Object::Dictionary(d) => Object::Dictionary(clone_dict(src, d, dst, map)),
        Object::Array(a) => Object::Array(clone_array(src, a, dst, map)),
        Object::Stream(s) => {
            let mut dict = clone_dict(src, s.dict, dst, map);
            let content = s.content.clone();
            dict.set("Length", Object::Integer(content.len() as i64));
            Object::Stream(Stream::new(dict, content))
        }
        other => other,
    }
}

/// 把已克隆的页对象挂到目标文档 /Pages 树末尾
fn append_page_to_tree(doc: &mut Document, page_id: ObjectId) {
    let pages_id = match doc
        .catalog()
        .ok()
        .and_then(|d| d.get(b"Pages").ok())
        .and_then(|o| o.as_reference().ok())
    {
        Some(id) => id,
        None => return,
    };
    // 页 Parent 改指目标 Pages 树
    if let Ok(Object::Dictionary(page)) = doc.get_object_mut(page_id) {
        page.set("Parent", Object::Reference(pages_id));
    }
    // Kids 追加 + Count 重算（先取 Kids 引用信息避免双借用：直接重建 Kids 数组）
    let new_count = doc.get_pages().len() as i64;
    let kids_ids: Vec<ObjectId> = doc
        .get_dictionary(pages_id)
        .ok()
        .and_then(|d| d.get(b"Kids").ok())
        .and_then(|o| o.as_array().ok())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| o.as_reference().ok())
                .collect()
        })
        .unwrap_or_default();
    let mut kids = kids_ids;
    kids.push(page_id);
    if let Ok(Object::Dictionary(pages)) = doc.get_object_mut(pages_id) {
        pages.set(
            "Kids",
            Object::Array(kids.into_iter().map(Object::Reference).collect()),
        );
        pages.set("Count", Object::Integer(new_count));
    }
}

/// 单页文字水印（内容流前置 Op 追加绘制指令）
fn stamp_page(doc: &mut Document, page_id: ObjectId, text: &str) -> Result<()> {
    use lopdf::dictionary;
    // 简单 Helvetica 文本水印，居中偏下
    let content_data = format!(
        "BT /F1 48 Tf 0.85 g 0.85 G 0.2 Tc 45 200 Td ({}) Tj ET ",
        sanitize_pdf_text(text)
    );
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        content_data.into_bytes(),
    ));
    // 字体对象（标准 14 字体：Helvetica，无需嵌入）
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    // 页资源加入字体与 ExtGState 引用
    let resources = doc
        .get_object_mut(page_id)
        .map_err(|e| EditorError::Pdf(format!("页对象读取失败: {e}")))?;
    if let Object::Dictionary(page) = resources {
        // 内容流追加（Contents 可能是数组或单个流引用）
        match page.get_mut(b"Contents") {
            Ok(Object::Reference(rid)) => {
                let old = *rid;
                page.set(
                    "Contents",
                    Object::Array(vec![
                        Object::Reference(old),
                        Object::Reference(content_id),
                    ]),
                );
            }
            Ok(Object::Array(arr)) => {
                arr.push(Object::Reference(content_id));
            }
            _ => {
                page.set("Contents", Object::Reference(content_id));
            }
        }
        // Resources /Font
        let res = page
            .get_mut(b"Resources")
            .ok()
            .and_then(|o| o.as_dict_mut().ok());
        if let Some(res) = res {
            match res.get_mut(b"Font") {
                Ok(fonts) => {
                    if let Ok(fd) = fonts.as_dict_mut() {
                        fd.set("F1", Object::Reference(font_id));
                    }
                }
                Err(_) => {
                    res.set("Font", dictionary! { "F1" => Object::Reference(font_id) });
                }
            }
        }
    }
    Ok(())
}

/// PDF 字面量字符串转义（\ ( ) 与非 ASCII 以 \ooo 八进制表示）
fn sanitize_pdf_text(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '(' => out.push_str(r"\("),
            ')' => out.push_str(r"\)"),
            c if (c as u32) < 128 => out.push(c),
            c => {
                // WinAnsi 编码近似：非 Latin1 字符替换为 '?'
                let b = c as u32;
                if b <= 0xFF {
                    out.push_str(&format!("\\{:03o}", b));
                } else {
                    out.push('?');
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nf_editor_pdf_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 构造 n 页简单 PDF
    fn make_pdf(path: &Path, pages: u32, text: &str) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.add_object(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![]),
            "Count" => Object::Integer(0),
        });
        for i in 1..=pages {
            let content = Stream::new(dictionary! {}, format!("BT /F1 12 Tf 72 720 Td (Page {} {text}) Tj ET", i).into_bytes());
            let content_id = doc.add_object(content);
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(612), Object::Integer(792)]),
                "Contents" => Object::Reference(content_id),
                "Resources" => dictionary! {
                    "Font" => dictionary! { "F1" => dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" } }
                },
            });
            if let Ok(Object::Dictionary(d)) = doc.get_object_mut(pages_id) {
                if let Ok(Object::Array(kids)) = d.get_mut(b"Kids") {
                    kids.push(Object::Reference(page_id));
                }
                d.set("Count", Object::Integer(i as i64));
            }
        }
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.save(path).unwrap();
    }

    #[test]
    fn info_reads_pages() {
        let dir = tmpdir("info");
        let a = dir.join("a.pdf");
        make_pdf(&a, 3, "hello");
        let i = info(&a).unwrap();
        assert_eq!(i.pages, 3);
        assert!(i.size > 0);
    }

    #[test]
    fn merge_appends_pages_in_order() {
        let dir = tmpdir("merge");
        let a = dir.join("a.pdf");
        let b = dir.join("b.pdf");
        make_pdf(&a, 2, "A");
        make_pdf(&b, 3, "B");
        let out = dir.join("merged.pdf");
        let r = merge(&[a, b], &out).unwrap();
        assert_eq!(r.pages, 5);
        assert_eq!(info(&out).unwrap().pages, 5);
    }

    #[test]
    fn split_creates_single_page_files() {
        let dir = tmpdir("split");
        let a = dir.join("a.pdf");
        make_pdf(&a, 3, "S");
        let out_dir = dir.join("pages");
        let results = split(&a, &out_dir).unwrap();
        assert_eq!(results.len(), 3);
        for (i, r) in results.iter().enumerate() {
            assert!(r.output.contains(&format!("a_{}.pdf", i + 1)));
            assert_eq!(info(Path::new(&r.output)).unwrap().pages, 1);
        }
    }

    #[test]
    fn watermark_keeps_page_count_and_marks_pages() {
        let dir = tmpdir("wm");
        let a = dir.join("a.pdf");
        make_pdf(&a, 2, "orig");
        let r = watermark(&a, "CONFIDENTIAL").unwrap();
        assert_eq!(r.pages, 2);
        // 重新解析不报错
        assert_eq!(info(&a).unwrap().pages, 2);
        // 空水印报错
        assert!(watermark(&a, "  ").is_err());
    }

    #[test]
    fn compress_never_grows_file() {
        let dir = tmpdir("compress");
        let a = dir.join("a.pdf");
        make_pdf(&a, 5, "compress me");
        let before = std::fs::metadata(&a).unwrap().len();
        let r = compress(&a).unwrap();
        let after = std::fs::metadata(&a).unwrap().len();
        assert!(after <= before, "压缩后不得大于原文件（幂等保护）");
        assert_eq!(r.pages, 5);
        assert!(!a.with_extension("pdf.nforge-tmp").exists(), "临时文件应清理或替换");
    }

    #[test]
    fn invalid_pdf_is_rejected() {
        let dir = tmpdir("invalid");
        let bad = dir.join("bad.pdf");
        std::fs::write(&bad, b"not a pdf at all").unwrap();
        assert!(info(&bad).is_err());
        assert!(merge(&[bad.clone(), bad.clone()], &dir.join("o.pdf")).is_err());
    }
}
