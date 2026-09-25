//! 文本编码检测与回写共享工具（COR-09 上收至宿主级，editor-core/notes-core 共用）。
//!
//! 检测链（与 editor-core::session::decode 同策略）：BOM → UTF-8 严格校验 →
//! chardetng 猜测 → WINDOWS_1252 兜底。[`DecodedText::lossy`] 标记"解码发生了
//! 不可逆替换"——任何"读→改→写回同一文件"的链路在 `lossy == true` 时必须
//! 跳过回写，否则原始字节将被永久破坏（from_utf8_lossy 禁止用于回写路径）。

use encoding_rs::Encoding;

/// 解码结果：文本 + 原编码 + 回写所需元数据
pub struct DecodedText {
    pub text: String,
    pub encoding: &'static Encoding,
    /// 原文件带 BOM（回写时补回）
    pub had_bom: bool,
    /// UTF-16LE（encoding_rs 按 WHATWG 不提供该编码的 encode，需自研回写）
    pub utf16le: bool,
    /// 解码发生不可逆替换（禁止以该结果回写原文件）
    pub lossy: bool,
}

/// BOM → UTF-8 严格校验 → chardetng 猜测 → WINDOWS_1252 兜底
pub fn detect_and_decode(bytes: &[u8]) -> DecodedText {
    // ① BOM
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let (text, _, had_errors) = encoding_rs::UTF_8.decode(&bytes[3..]);
        if !had_errors {
            return DecodedText {
                text: text.into_owned(),
                encoding: encoding_rs::UTF_8,
                had_bom: true,
                utf16le: false,
                lossy: false,
            };
        }
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (text, _, had_errors) = encoding_rs::UTF_16LE.decode(bytes);
        if !had_errors {
            return DecodedText {
                text: text.into_owned(),
                encoding: encoding_rs::UTF_16LE,
                had_bom: true,
                utf16le: true,
                lossy: false,
            };
        }
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let (text, _, had_errors) = encoding_rs::UTF_16BE.decode(bytes);
        if !had_errors {
            // BE 罕见且回写需自研：标记 lossy 禁止回写（宁可不改写也不毁文件）
            return DecodedText {
                text: text.into_owned(),
                encoding: encoding_rs::UTF_16BE,
                had_bom: true,
                utf16le: false,
                lossy: true,
            };
        }
    }

    // ② UTF-8 严格校验（无替换）
    if let Ok(text) = std::str::from_utf8(bytes) {
        return DecodedText {
            text: text.to_string(),
            encoding: encoding_rs::UTF_8,
            had_bom: false,
            utf16le: false,
            lossy: false,
        };
    }

    // ③ chardetng 猜测（GBK 为主）
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(bytes, true);
    let guessed = detector.guess(None, true);
    let (text, _, had_errors) = guessed.decode(bytes);
    if !had_errors {
        return DecodedText {
            text: text.into_owned(),
            encoding: guessed,
            had_bom: false,
            utf16le: false,
            lossy: false,
        };
    }

    // ④ WINDOWS_1252 兜底（恒成功，但相对真实编码必有损 → 禁止回写）
    let (text, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
    DecodedText {
        text: text.into_owned(),
        encoding: encoding_rs::WINDOWS_1252,
        had_bom: false,
        utf16le: false,
        lossy: true,
    }
}

/// 按检测到的原编码回写字节（含 BOM / UTF-16LE 还原）
pub fn encode_with(d: &DecodedText, text: &str) -> Vec<u8> {
    if d.utf16le {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        return bytes;
    }
    let mut bytes = d.encoding.encode(text).0.into_owned();
    if d.had_bom {
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(&bytes);
        bytes = with_bom;
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_no_bom_roundtrip() {
        let d = detect_and_decode("中文内容".as_bytes());
        assert!(!d.lossy);
        assert!(!d.had_bom);
        assert_eq!(d.encoding, encoding_rs::UTF_8);
        assert_eq!(encode_with(&d, "中文改"), "中文改".as_bytes());
    }

    #[test]
    fn utf8_bom_roundtrip() {
        let raw: &[u8] = &[0xEF, 0xBB, 0xBF, b'a', 0xE4, 0xB8, 0xAD];
        let d = detect_and_decode(raw);
        assert!(!d.lossy);
        assert!(d.had_bom);
        assert_eq!(encode_with(&d, "a中"), raw);
    }

    #[test]
    fn gbk_detected_and_preserved() {
        // "中文笔记" 的 GBK 字节
        let gbk_bytes: Vec<u8> = encoding_rs::GBK.encode("中文笔记 [[旧名]]").0.into_owned();
        let d = detect_and_decode(&gbk_bytes);
        assert!(!d.lossy, "GBK 必须被 chardetng 无损解码");
        assert_eq!(d.encoding, encoding_rs::GBK);
        assert_eq!(d.text, "中文笔记 [[旧名]]");
        // 按原编码回写 → 字节不变
        assert_eq!(encode_with(&d, &d.text), gbk_bytes);
    }

    #[test]
    fn undecodable_bytes_never_claim_lossless_without_roundtrip() {
        // 混合非法字节（截断 UTF-16 + 孤立代理项）：UTF-8 不合法、UTF-16 解码有错。
        // 无论最终落到哪个编码分支，都只允许两种结果之一：
        // ① 标 lossy（禁止回写）；② 判为某编码"无损"但回写必须逐字节还原。
        let garbage: [u8; 6] = [0xFF, 0xFE, 0x00, 0xD8, 0x00, 0x00];
        let d = detect_and_decode(&garbage);
        if d.lossy {
            // lossy 标记 → 调用方将跳过回写（无需再断言字节还原）
        } else {
            assert_eq!(
                encode_with(&d, &d.text),
                garbage,
                "无损失配必须可逐字节还原"
            );
        }
    }

    #[test]
    fn utf16le_detected_and_roundtrip() {
        let mut raw: Vec<u8> = vec![0xFF, 0xFE];
        raw.extend("中文".encode_utf16().flat_map(u16::to_le_bytes));
        let d = detect_and_decode(&raw);
        assert!(!d.lossy);
        assert!(d.utf16le);
        assert_eq!(d.text, "中文");
        assert_eq!(encode_with(&d, "中文"), raw);
    }
}
