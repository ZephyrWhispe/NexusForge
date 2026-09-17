//! E1 文件会话模型（docs/impl/06 E1）：
//! - 编码检测：BOM → UTF-8 严格校验失败则 chardetng 猜测（GBK/Latin1 等）
//! - 保存默认保持原编码；UI 显示当前编码
//! - EOL 混合检测后**整文件统一**（CRLF/LF/LF→CRLF 由 UI 明示）
//! - 大文件阈值：>5MB 建议关语法高亮、>50MB 只读（E2 前端执行，open 返回 size）

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use encoding_rs::{Encoding, UTF_8};

use crate::error::{EditorError, Result};

/// 自动保存文件后缀（脏后由前端防抖调用 autosave 写入；正常保存后清理）
pub const AUTOSAVE_SUFFIX: &str = ".nforge-autosave";

/// 大文件阈值（字节）
pub const BIG_FILE_HIGHLIGHT: u64 = 5 * 1024 * 1024;
pub const HUGE_FILE_READONLY: u64 = 50 * 1024 * 1024;

/// 文本编码（保存时按原编码回写）
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EncodingKind {
    Utf8,
    /// UTF-8 带 BOM
    Utf8Bom,
    Utf16Le,
    /// GBK（chardetng 猜测）
    Gbk,
    /// Latin-1 兜底（不会失败）
    Latin1,
}

impl EncodingKind {
    fn label(&self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf8Bom => "UTF-8 BOM",
            Self::Utf16Le => "UTF-16 LE",
            Self::Gbk => "GBK",
            Self::Latin1 => "Latin-1",
        }
    }

    fn encoding(&self) -> &'static Encoding {
        match self {
            Self::Utf8 | Self::Utf8Bom => UTF_8,
            Self::Utf16Le => encoding_rs::UTF_16LE,
            Self::Gbk => encoding_rs::GBK,
            Self::Latin1 => encoding_rs::WINDOWS_1252,
        }
    }
}

/// 行尾（检测：出现 \r\n → Crlf；否则 Lf）
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Eol {
    Crlf,
    Lf,
}

impl Eol {
    fn sep(&self) -> &'static str {
        match self {
            Self::Crlf => "\r\n",
            Self::Lf => "\n",
        }
    }
}

/// 会话元信息（IPC 返回；content 不回传——前端按需拉取）
#[derive(Clone, Debug, serde::Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub path: String,
    pub name: String,
    pub encoding: EncodingKind,
    /// 编码显示标签（"UTF-8"/"GBK"…）
    pub encoding_label: &'static str,
    pub eol: Eol,
    /// 检测到的 EOL 混合（打开时存在混合行尾，保存将整文件统一——UI 需明示）
    pub eol_mixed: bool,
    pub dirty: bool,
    pub size: u64,
    /// E2 大文件降级标志：>5MB 建议关语法高亮
    pub big_file: bool,
    /// >50MB 只读（前端禁止编辑提交）
    pub readonly: bool,
}

struct Session {
    path: PathBuf,
    encoding: EncodingKind,
    eol: Eol,
    eol_mixed: bool,
    dirty: bool,
    size: u64,
    content: String,
}

/// 全部会话管理（模块级单例；RwLock 串行化——文本编辑低频重操作）
pub struct EditorSessions {
    sessions: RwLock<HashMap<String, Session>>,
}

impl Default for EditorSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorSessions {
    pub fn new() -> Self {
        Self { sessions: RwLock::new(HashMap::new()) }
    }

    /// 打开文件：读原始字节 → 编码检测 → 解码 → EOL 检测（不统一，仅标记）
    pub fn open(&self, path: &Path) -> Result<SessionInfo> {
        let raw = std::fs::read(path)?;
        let size = raw.len() as u64;
        let (encoding, content) = decode(&raw)?;
        let (eol, eol_mixed) = detect_eol(&content);

        let id = uuid::Uuid::now_v7().to_string();
        let info = SessionInfo {
            id: id.clone(),
            path: path.display().to_string(),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            encoding,
            encoding_label: encoding.label(),
            eol,
            eol_mixed,
            dirty: false,
            size,
            big_file: size > BIG_FILE_HIGHLIGHT,
            readonly: size > HUGE_FILE_READONLY,
        };
        self.sessions
            .write()
            .map_err(|_| EditorError::Encoding("会话锁污染".into()))?
            .insert(id, Session { path: path.to_path_buf(), encoding, eol, eol_mixed, dirty: false, size, content });
        Ok(info)
    }

    /// 取内容（前端打开后拉取一次；>50MB 只读也返回——查看器分片由前端处理）
    pub fn content(&self, id: &str) -> Result<String> {
        let map = self.lock();
        let s = map
            .get(id)
            .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
        Ok(s.content.clone())
    }

    /// 更新内容并置脏标记
    pub fn update(&self, id: &str, content: &str) -> Result<bool> {
        let mut s = self.get_mut(id)?;
        if s.size > HUGE_FILE_READONLY {
            return Err(EditorError::BadParam(
                "文件超过 50MB，编辑器只读；请使用分片查看器".into(),
            ));
        }
        s.dirty = true;
        s.content = content.to_string();
        Ok(true)
    }

    /// 保存：保持原编码；EOL 按会话统一（混合时整文件统一——UI 已明示）
    /// 成功后清脏标记 + 删除 autosave 文件。
    pub fn save(&self, id: &str) -> Result<SessionInfo> {
        let (raw, info) = {
            let mut map = self.lock();
            let s = map
                .get_mut(id)
                .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
            // 整文件统一 EOL
            let unified = normalize_eol(&s.content, s.eol);
            let bytes = encode(&unified, s.encoding);
            (bytes, session_info(id, s))
        };
        // 写锁已释放再写文件（避免 IO 慢操作持锁）
        if let Some(s) = self.lock().get(id) {
            std::fs::write(&s.path, &raw)?;
            // 删除自动保存草稿
            let _ = std::fs::remove_file(autosave_path(&s.path));
        }
        let mut map = self.lock();
        if let Some(s) = map.get_mut(id) {
            s.dirty = false;
            s.eol_mixed = false;
            s.size = raw.len() as u64;
        }
        Ok(info)
    }

    /// 另存为（编码保持；不改动原会话绑定的路径语义之外的脏状态）
    pub fn save_as(&self, id: &str, target: &Path) -> Result<SessionInfo> {
        let (raw, unified, eol) = {
            let map = self.lock();
            let s = map
                .get(id)
                .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
            let unified = normalize_eol(&s.content, s.eol);
            (encode(&unified, s.encoding), unified, s.eol)
        };
        std::fs::write(target, &raw)?;
        let mut map = self.lock();
        let s = map
            .get_mut(id)
            .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
        s.path = target.to_path_buf();
        s.dirty = false;
        s.eol_mixed = false;
        s.size = unified.len() as u64;
        let _ = eol;
        Ok(session_info(id, s))
    }

    /// 自动保存草稿（脏内容写 `<path>.nforge-autosave`；崩溃恢复入口）
    pub fn autosave(&self, id: &str, content: &str) -> Result<bool> {
        let p = {
            let mut map = self.lock();
            let s = map
                .get_mut(id)
                .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
            if s.readonly() {
                return Ok(false);
            }
            s.dirty = true;
            s.content = content.to_string();
            autosave_path(&s.path)
        };
        // 草稿恒 UTF-8（恢复时重新检测，无需保持原编码）
        std::fs::write(&p, content.as_bytes())?;
        Ok(true)
    }

    /// 关闭会话：清理 autosave，返回是否已保存过（提示用：脏关闭由 UI 确认）
    pub fn close(&self, id: &str) -> Result<bool> {
        let was_dirty = {
            let mut map = self.lock();
            if let Some(s) = map.remove(id) {
                let _ = std::fs::remove_file(autosave_path(&s.path));
                s.dirty
            } else {
                return Err(EditorError::NotFound(id.to_string()));
            }
        };
        Ok(was_dirty)
    }

    /// 全部会话列表
    pub fn list(&self) -> Vec<SessionInfo> {
        self.lock()
            .iter()
            .map(|(id, s)| session_info(id, s))
            .collect()
    }

    fn get_mut(&self, id: &str) -> Result<SessionMutGuard<'_>> {
        Ok(SessionMutGuard {
            inner: self.lock(),
            id: id.to_string(),
        })
    }

    fn lock(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Session>> {
        self.sessions
            .write()
            .expect("会话锁污染")
    }
}

// ---- 内部辅助 ----

struct SessionMutGuard<'a> {
    inner: std::sync::RwLockWriteGuard<'a, HashMap<String, Session>>,
    id: String,
}

impl std::ops::Deref for SessionMutGuard<'_> {
    type Target = Session;
    fn deref(&self) -> &Self::Target {
        self.inner.get(&self.id).expect("guard 持有期间被移除")
    }
}

impl Session {
    fn readonly(&self) -> bool {
        self.size > HUGE_FILE_READONLY
    }
}

impl std::ops::DerefMut for SessionMutGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.get_mut(&self.id).expect("guard 持有期间被移除")
    }
}

fn session_info(id: &str, s: &Session) -> SessionInfo {
    SessionInfo {
        id: id.to_string(),
        path: s.path.display().to_string(),
        name: s
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.path.display().to_string()),
        encoding: s.encoding,
        encoding_label: s.encoding.label(),
        eol: s.eol,
        eol_mixed: s.eol_mixed,
        dirty: s.dirty,
        size: s.size,
        big_file: s.size > BIG_FILE_HIGHLIGHT,
        readonly: s.size > HUGE_FILE_READONLY,
    }
}

fn autosave_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}{AUTOSAVE_SUFFIX}", path.display()))
}

/// 编码检测（docs/impl/06 E1）：BOM → UTF-8 严格校验 → chardetng 猜测 → Latin-1 兜底
pub fn decode(raw: &[u8]) -> Result<(EncodingKind, String)> {
    // ① BOM
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let (text, _, had_errors) = UTF_8.decode(&raw[3..]);
        if !had_errors {
            return Ok((EncodingKind::Utf8Bom, text.into_owned()));
        }
    }
    if raw.starts_with(&[0xFF, 0xFE]) {
        let (text, _, had_errors) = encoding_rs::UTF_16LE.decode(raw);
        if !had_errors {
            return Ok((EncodingKind::Utf16Le, text.into_owned()));
        }
    }
    if raw.starts_with(&[0xFE, 0xFF]) {
        let (text, _, had_errors) = encoding_rs::UTF_16BE.decode(raw);
        if !had_errors {
            // BE 罕见：按 LE 语义保存会破坏——映射到 Utf16Le 标签但不回写 BOM 差异可接受
            return Ok((EncodingKind::Utf16Le, text.into_owned()));
        }
    }

    // ② UTF-8 严格校验
    match std::str::from_utf8(raw) {
        Ok(text) => return Ok((EncodingKind::Utf8, text.to_string())),
        Err(_) => {}
    }

    // ③ chardetng 猜测（GBK 为主）
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(raw, true);
    let guessed = detector.guess(None, true);
    let (text, _, had_errors) = guessed.decode(raw);
    if !had_errors {
        if guessed == encoding_rs::GBK {
            return Ok((EncodingKind::Gbk, text.into_owned()));
        }
        // 其他非 UTF-8 单字节编码统一按 Latin-1 语义（WINDOWS_1252 保存）
        return Ok((EncodingKind::Latin1, text.into_owned()));
    }

    // ④ Latin-1 兜底（恒成功）
    let (text, _, _) = encoding_rs::WINDOWS_1252.decode(raw);
    Ok((EncodingKind::Latin1, text.into_owned()))
}

/// 编码回写（BOM 编码补前缀）
pub fn encode(text: &str, kind: EncodingKind) -> Vec<u8> {
    let mut bytes = kind.encoding().encode(text).0.into_owned();
    if kind == EncodingKind::Utf8Bom {
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(&bytes);
        bytes = with_bom;
    }
    bytes
}

/// EOL 检测：返回（主导行尾, 是否混合）
fn detect_eol(content: &str) -> (Eol, bool) {
    let has_crlf = content.contains("\r\n");
    // 孤立 \n（非 \r\n 的一部分）：去掉所有 \r\n 后仍含 \n 即混合
    let lone_lf = content.replace("\r\n", "").contains('\n');
    let lone_cr = content.replace("\r\n", "").contains('\r');
    match (has_crlf, lone_lf || lone_cr) {
        (true, true) => (Eol::Crlf, true),
        (true, false) => (Eol::Crlf, false),
        _ => (Eol::Lf, false),
    }
}

/// 整文件统一 EOL（保存前调用；docs/impl/06 风险标注：UI 明示后执行）
fn normalize_eol(content: &str, eol: Eol) -> String {
    // 先全部归一为 \n，再按目标展开
    let lf = content.replace("\r\n", "\n").replace('\r', "\n");
    if eol == Eol::Crlf {
        lf.replace('\n', "\r\n")
    } else {
        lf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_editor_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn decode_utf8_and_bom() {
        let (k, t) = decode("hello 世界".as_bytes()).unwrap();
        assert_eq!(k, EncodingKind::Utf8);
        assert_eq!(t, "hello 世界");

        let (k, t) = decode([0xEF, 0xBB, 0xBF].iter().chain("BOM文本".as_bytes()).copied().collect::<Vec<u8>>().as_slice()).unwrap();
        assert_eq!(k, EncodingKind::Utf8Bom);
        assert_eq!(t, "BOM文本");
    }

    #[test]
    fn decode_gbk_fallback() {
        // "中文" GBK 编码字节（非合法 UTF-8）
        let gbk_bytes = encoding_rs::GBK.encode("中文测试").0.into_owned();
        let (k, t) = decode(&gbk_bytes).unwrap();
        assert_eq!(k, EncodingKind::Gbk);
        assert_eq!(t, "中文测试");
    }

    #[test]
    fn open_update_save_roundtrip_keeps_encoding() {
        let dir = tmpdir("roundtrip");
        let path = dir.join("gbk.txt");
        std::fs::write(&path, encoding_rs::GBK.encode("你好\r\n世界").0.into_owned()).unwrap();

        let sessions = EditorSessions::new();
        let info = sessions.open(&path).unwrap();
        assert_eq!(info.encoding, EncodingKind::Gbk);
        assert_eq!(info.encoding_label, "GBK");
        assert_eq!(info.eol, Eol::Crlf);
        assert!(!info.dirty);

        // 更新置脏
        let content = sessions.content(&info.id).unwrap();
        assert_eq!(content, "你好\r\n世界");
        sessions.update(&info.id, "你好\r\n世界\r\n新增一行").unwrap();
        assert!(sessions.list()[0].dirty);

        // 保存保持 GBK 编码
        sessions.save(&info.id).unwrap();
        assert!(!sessions.list()[0].dirty);
        let raw = std::fs::read(&path).unwrap();
        let (decoded, _, had_errors) = encoding_rs::GBK.decode(&raw);
        assert!(!had_errors);
        assert_eq!(decoded, "你好\r\n世界\r\n新增一行");
    }

    #[test]
    fn eol_mixed_normalized_on_save() {
        let dir = tmpdir("eol");
        let path = dir.join("mixed.txt");
        std::fs::write(&path, b"a\r\nb\nc").unwrap();

        let sessions = EditorSessions::new();
        let info = sessions.open(&path).unwrap();
        assert!(info.eol_mixed, "应检测到混合行尾");
        assert_eq!(info.eol, Eol::Crlf, "存在 CRLF 时主导 CRLF");

        sessions.save(&info.id).unwrap();
        let raw = std::fs::read(&path).unwrap();
        // 整文件统一为 CRLF；末尾无行尾的 c 不补换行
        assert_eq!(raw, b"a\r\nb\r\nc");
        assert!(!sessions.list()[0].eol_mixed);
    }

    #[test]
    fn autosave_and_close_cleanup() {
        let dir = tmpdir("autosave");
        let path = dir.join("note.md");
        std::fs::write(&path, b"# title").unwrap();

        let sessions = EditorSessions::new();
        let info = sessions.open(&path).unwrap();
        sessions.update(&info.id, "# title\nchanged").unwrap();
        sessions.autosave(&info.id, "# title\nchanged").unwrap();
        assert!(path.with_file_name("note.md.nforge-autosave").is_file());

        // 正常保存清理草稿
        sessions.save(&info.id).unwrap();
        assert!(!path.with_file_name("note.md.nforge-autosave").exists());

        // 关闭
        assert!(!sessions.close(&info.id).unwrap(), "已保存的会话关闭不提示脏");
        assert!(sessions.close(&info.id).is_err(), "重复关闭报 NotFound");
    }

    #[test]
    fn save_as_switches_path() {
        let dir = tmpdir("saveas");
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, b"content").unwrap();

        let sessions = EditorSessions::new();
        let info = sessions.open(&a).unwrap();
        sessions.update(&info.id, "content v2").unwrap();
        let new_info = sessions.save_as(&info.id, &b).unwrap();
        assert_eq!(new_info.path, b.display().to_string());
        assert_eq!(std::fs::read(&b).unwrap(), b"content v2");
        assert!(!new_info.dirty);
    }

    #[test]
    fn encode_roundtrip_all_kinds() {
        for (kind, text) in [
            (EncodingKind::Utf8, "文本 Text"),
            (EncodingKind::Utf8Bom, "文本 Text"),
            (EncodingKind::Gbk, "中文 Text"),
            // Latin-1（WINDOWS_1252）存不了 CJK，仅 Latin 字符
            (EncodingKind::Latin1, "Café Text"),
        ] {
            let bytes = encode(text, kind);
            let (decoded, _, had_errors) = kind.encoding().decode(&bytes);
            assert!(!had_errors);
            assert_eq!(decoded, text);
        }
    }
}
