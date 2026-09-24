//! E1 文件会话模型（docs/impl/06 E1）：
//! - 编码检测：BOM → UTF-8 严格校验失败则 chardetng 猜测（GBK/Latin1 等）
//! - 保存默认保持原编码；UI 显示当前编码
//! - EOL 混合检测后**整文件统一**（CRLF/LF/LF→CRLF 由 UI 明示）
//! - 大文件阈值：>5MB 建议关语法高亮、>50MB 只读（E2 前端执行，open 返回 size）

use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

/// IPC 输入侧编码选择（T-B7-18：回写面五档）。与检测侧 `EncodingKind` 分立：
/// 检测枚举随 chardetng 事实演化，输入面只暴露承诺支持的档位。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncodingKindDto {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Gbk,
    Latin1,
}

impl EncodingKindDto {
    fn to_kind(self) -> EncodingKind {
        match self {
            Self::Utf8 => EncodingKind::Utf8,
            Self::Utf8Bom => EncodingKind::Utf8Bom,
            Self::Utf16Le => EncodingKind::Utf16Le,
            Self::Gbk => EncodingKind::Gbk,
            Self::Latin1 => EncodingKind::Latin1,
        }
    }
}

/// EOL 切换选项（Preserve=维持当前行尾不动）
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EolChoice {
    Preserve,
    Lf,
    Crlf,
}

/// 转码预览（set_encoding 返回；**转码前**算好不可映射字符数——
/// >0 时前端必须复述「将丢失 N 个字符」，静默丢字与谎称成功同罪）
#[derive(Clone, Debug, serde::Serialize)]
pub struct EncodingPreview {
    pub from: EncodingKind,
    pub to: EncodingKind,
    pub chars_before: usize,
    pub chars_after: usize,
    pub replacement_char_count: usize,
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
    /// 待生效的切换编码（T-B7-18：Some=保存时转码为该档，None=保持检测编码恒等）；
    /// 上方 encoding 字段恒为**生效视图**（preferred.unwrap_or(detected)），UI 徽标即时反映切换
    pub preferred_encoding: Option<EncodingKind>,
    /// 存在比盘上文件更新的 .nforge-autosave 草稿（open 时判定，恢复口消费）
    pub autosave_draft: bool,
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
    /// 待生效切换编码（None=保持 encoding 恒等，即旧"保持原编码"行为正对照）
    preferred_encoding: Option<EncodingKind>,
    /// open 时检测到的较新 autosave 草稿（内容已随会话载入，恢复与否由 UI 决定）
    autosave_draft: bool,
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
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// 打开文件：读原始字节 → 编码检测 → 解码 → EOL 检测（不统一，仅标记）
    /// 若存在比盘上文件更新的 autosave 草稿，置 `autosave_draft`（承重⑨"写了没人读"
    /// 死面收口：open 只探测不改内容，恢复经 `recover_draft` 显式口，UI 提示二选一）
    pub fn open(&self, path: &Path) -> Result<SessionInfo> {
        let raw = std::fs::read(path)?;
        let size = raw.len() as u64;
        let (encoding, content) = decode(&raw)?;
        let (eol, eol_mixed) = detect_eol(&content);
        let autosave_draft = draft_newer_than(path);

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
            preferred_encoding: None,
            autosave_draft,
            eol,
            eol_mixed,
            dirty: false,
            size,
            big_file: size > BIG_FILE_HIGHLIGHT,
            readonly: size > HUGE_FILE_READONLY,
        };
        self.sessions.write().insert(
            id,
            Session {
                path: path.to_path_buf(),
                encoding,
                preferred_encoding: None,
                autosave_draft,
                eol,
                eol_mixed,
                dirty: false,
                size,
                content,
            },
        );
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

    /// 保存：按**生效编码**回写（preferred 未设=保持原编码恒等）；EOL 按会话统一
    /// （混合时整文件统一——UI 已明示）。成功后清脏标记 + 删除 autosave 文件。
    pub fn save(&self, id: &str) -> Result<SessionInfo> {
        let (raw, info) = {
            let mut map = self.lock();
            let s = map
                .get_mut(id)
                .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
            // 整文件统一 EOL；转码时序=保存时算不落中间盘（T-B7-18）
            let unified = normalize_eol(&s.content, s.eol);
            let bytes = encode(&unified, s.effective_encoding());
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
            // 转码收口：盘上已是目标编码，preferred 消费落定（None 臂=旧行为零变）
            if let Some(p) = s.preferred_encoding.take() {
                s.encoding = p;
            }
            s.autosave_draft = false;
        }
        Ok(info)
    }

    /// 另存为（生效编码回写；不改动原会话绑定的路径语义之外的脏状态）
    pub fn save_as(&self, id: &str, target: &Path) -> Result<SessionInfo> {
        let raw = {
            let map = self.lock();
            let s = map
                .get(id)
                .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
            let unified = normalize_eol(&s.content, s.eol);
            encode(&unified, s.effective_encoding())
        };
        std::fs::write(target, &raw)?;
        let mut map = self.lock();
        let s = map
            .get_mut(id)
            .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
        s.path = target.to_path_buf();
        s.dirty = false;
        s.eol_mixed = false;
        // size 恒为写盘字节数（与 save 同锚）：GBK 等宽字节下 String::len（UTF-8）会虚高
        s.size = raw.len() as u64;
        if let Some(p) = s.preferred_encoding.take() {
            s.encoding = p;
        }
        Ok(session_info(id, s))
    }

    /// 切换回写编码 / 统一行尾（T-B7-18）。**转码前**计算不可映射字符数并如实返回；
    /// 切换只改内存档位，落盘发生在下一次 save（不落中间盘）。
    /// 不可映射判据（探针实证后定形）：逐字符 `Encoding::encode` 的第三返回位
    /// `had_unencodable`——不猜 `?` 字节形，因为 WHATWG 单字节族对不可映射字符发
    /// **数字字符引用**（中→`&#20013;` 而非 `?`），GBK 才走 `?`，猜字节形会漏计。
    /// 预览是低频 UI 动作，逐字符探测的开销可接受。
    pub fn set_encoding(
        &self,
        id: &str,
        encoding: EncodingKindDto,
        eol: EolChoice,
    ) -> Result<EncodingPreview> {
        let to = encoding.to_kind();
        let mut map = self.lock();
        let s = map
            .get_mut(id)
            .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
        let from = s.effective_encoding();
        let enc = to.encoding();
        let introduced = s
            .content
            .chars()
            .filter(|c| {
                let mut buf = [0u8; 4];
                enc.encode(c.encode_utf8(&mut buf)).2
            })
            .count();
        let bytes = encode(&s.content, to);
        let (roundtrip, _, _) = enc.decode(&bytes);
        let preview = EncodingPreview {
            from,
            to,
            chars_before: s.content.chars().count(),
            chars_after: roundtrip.chars().count(),
            replacement_char_count: introduced,
        };
        match eol {
            EolChoice::Preserve => {}
            EolChoice::Lf => s.eol = Eol::Lf,
            EolChoice::Crlf => s.eol = Eol::Crlf,
        }
        // 切回检测编码=撤销档位（effective 视图不变，preferred 归 None 恒等）
        s.preferred_encoding = if to == s.encoding { None } else { Some(to) };
        Ok(preview)
    }

    /// 恢复 autosave 草稿（T-B7-18 回读口）：草稿比盘上文件新才恢复——
    /// 拒陈旧草稿是"无事实源就无文案"的写侧形态；恢复后置脏（未保存事实），
    /// 草稿文件本身留到下一次 save/close 统一清理（恢复未保存前崩溃仍可再恢复）。
    pub fn recover_draft(&self, id: &str) -> Result<SessionInfo> {
        let path = {
            let map = self.lock();
            map.get(id)
                .ok_or_else(|| EditorError::NotFound(id.to_string()))?
                .path
                .clone()
        };
        if !draft_newer_than(&path) {
            return Err(EditorError::BadParam(
                "没有比盘上文件更新的自动保存草稿".into(),
            ));
        }
        let raw = std::fs::read(autosave_path(&path))?;
        // 草稿恒 UTF-8（autosave 唯一写口）；坏草稿报编码错，不 lossy 吞
        let text = String::from_utf8(raw)
            .map_err(|_| EditorError::Encoding("自动保存草稿不是有效 UTF-8，无法恢复".into()))?;
        let mut map = self.lock();
        let s = map
            .get_mut(id)
            .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
        if s.readonly() {
            return Err(EditorError::BadParam(
                "文件超过 50MB 只读，不恢复草稿".into(),
            ));
        }
        s.content = text;
        s.dirty = true;
        s.autosave_draft = false;
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
        if let Some(s) = self.lock().get_mut(id) {
            s.autosave_draft = true;
        }
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

    fn lock(&self) -> parking_lot::RwLockWriteGuard<'_, HashMap<String, Session>> {
        self.sessions.write()
    }
}

// ---- 内部辅助 ----

struct SessionMutGuard<'a> {
    inner: parking_lot::RwLockWriteGuard<'a, HashMap<String, Session>>,
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

    /// 生效编码：待切换档优先，None=保持检测编码（旧行为恒等正对照）
    fn effective_encoding(&self) -> EncodingKind {
        self.preferred_encoding.unwrap_or(self.encoding)
    }
}

impl std::ops::DerefMut for SessionMutGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.get_mut(&self.id).expect("guard 持有期间被移除")
    }
}

fn session_info(id: &str, s: &Session) -> SessionInfo {
    let effective = s.effective_encoding();
    SessionInfo {
        id: id.to_string(),
        path: s.path.display().to_string(),
        name: s
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.path.display().to_string()),
        encoding: effective,
        encoding_label: effective.label(),
        preferred_encoding: s.preferred_encoding,
        autosave_draft: s.autosave_draft,
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

/// 草稿是否比盘上文件更新（mtime 比较；任一侧取不到 mtime 按 false——
/// 无事实源就不提示，宁可不恢复也不拿陈旧草稿覆盖编辑缓冲）
fn draft_newer_than(path: &Path) -> bool {
    let draft = autosave_path(path);
    match (
        std::fs::metadata(&draft).and_then(|m| m.modified()),
        std::fs::metadata(path).and_then(|m| m.modified()),
    ) {
        (Ok(d), Ok(f)) => d > f,
        _ => false,
    }
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
    if let Ok(text) = std::str::from_utf8(raw) {
        return Ok((EncodingKind::Utf8, text.to_string()));
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
///
/// 探针实证（T-B7-18）：encoding_rs 按 WHATWG 规格把 UTF-16LE 定为**只解码**编码
/// （`encode` 直出 UTF-8 字节，注释原文 "The output encoding of this encoding is
/// UTF-8"），所以 Utf16Le 的编码必须自研：FF FE BOM + `encode_utf16` 小端码元。
/// Utf8Bom 也需手补（WHATWG 的 UTF-8 编码器不发 BOM）。
pub fn encode(text: &str, kind: EncodingKind) -> Vec<u8> {
    if kind == EncodingKind::Utf16Le {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        return bytes;
    }
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

        let (k, t) = decode(
            [0xEF, 0xBB, 0xBF]
                .iter()
                .chain("BOM文本".as_bytes())
                .copied()
                .collect::<Vec<u8>>()
                .as_slice(),
        )
        .unwrap();
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
        std::fs::write(&path, &encoding_rs::GBK.encode("你好\r\n世界").0).unwrap();

        let sessions = EditorSessions::new();
        let info = sessions.open(&path).unwrap();
        assert_eq!(info.encoding, EncodingKind::Gbk);
        assert_eq!(info.encoding_label, "GBK");
        assert_eq!(info.eol, Eol::Crlf);
        assert!(!info.dirty);

        // 更新置脏
        let content = sessions.content(&info.id).unwrap();
        assert_eq!(content, "你好\r\n世界");
        sessions
            .update(&info.id, "你好\r\n世界\r\n新增一行")
            .unwrap();
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
        assert!(
            !sessions.close(&info.id).unwrap(),
            "已保存的会话关闭不提示脏"
        );
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
    fn save_as_reports_disk_byte_size_not_char_len() {
        let dir = tmpdir("sizebytes");
        let a = dir.join("gbk_src.txt");
        let b = dir.join("gbk_copy.txt");
        // "你好世界" GBK 写盘 8 字节，而 String::len（UTF-8 视角）是 12——缺陷式（unified.len）必现 12
        std::fs::write(&a, &encoding_rs::GBK.encode("你好世界").0).unwrap();

        let sessions = EditorSessions::new();
        let info = sessions.open(&a).unwrap();
        assert_eq!(info.size as usize, 8);

        let saved = sessions.save_as(&info.id, &b).unwrap();
        let on_disk = std::fs::read(&b).unwrap();
        assert_eq!(on_disk.len(), 8);
        assert_eq!(
            saved.size as usize,
            on_disk.len(),
            "save_as 的 size 必须是写盘字节数（与 save 同锚），不是 UTF-8 长度"
        );
        assert_eq!(sessions.list()[0].size, saved.size);

        // save 同锚回归位：save_as 已把会话换绑到 b，后续 save 写 b——变更后 size 同样等于磁盘字节数
        // （GBK 6 字 = 12 字节；a 保持打开时的 8 字节不动）
        sessions.update(&info.id, "你好世界你好").unwrap();
        sessions.save(&info.id).unwrap();
        let on_disk = std::fs::read(&b).unwrap();
        assert_eq!(on_disk.len(), 12);
        assert_eq!(sessions.list()[0].size as usize, on_disk.len());
        assert_eq!(std::fs::read(&a).unwrap().len(), 8);
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
        // T-B7-18：UTF-16LE 落盘带 BOM（探针实证=encoding_rs 按 WHATWG 把 UTF-16LE
        // 定为只解码编码，encode 直出 UTF-8 字节，故编码在 encode() 内自研补 FF FE），
        // 判据在**检测口 decode()** 认档而非编码库自洽——否则重开走 chardetng 误判
        let bytes = encode("中文 Text", EncodingKind::Utf16Le);
        assert!(bytes.starts_with(&[0xFF, 0xFE]));
        let (k, t) = decode(&bytes).unwrap();
        assert_eq!(k, EncodingKind::Utf16Le);
        assert_eq!(t, "中文 Text");
    }

    // ======================== T-B7-18 编码/EOL 真实切换 ========================

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-18）字面测试名优先于 rustc 命名惯例
    fn setEncoding_utf8ToGbk_roundTripsBytes() {
        let dir = tmpdir("enc-gbk");
        let path = dir.join("out.txt");
        std::fs::write(&path, "你好\n世界".as_bytes()).unwrap(); // UTF-8 源文

        let sessions = EditorSessions::new();
        let info = sessions.open(&path).unwrap();
        assert_eq!(info.encoding, EncodingKind::Utf8);

        let p = sessions
            .set_encoding(&info.id, EncodingKindDto::Gbk, EolChoice::Preserve)
            .unwrap();
        assert_eq!(p.from, EncodingKind::Utf8);
        assert_eq!(p.to, EncodingKind::Gbk);
        assert_eq!(p.replacement_char_count, 0, "GBK 全覆盖中文，应零丢失");
        // 生效视图即刻反映切换（徽标即时性），但档位未落盘——preferred 待 save 消费
        assert_eq!(sessions.list()[0].encoding, EncodingKind::Gbk);
        assert_eq!(
            sessions.list()[0].preferred_encoding,
            Some(EncodingKind::Gbk)
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            "你好\n世界".as_bytes(),
            "切换不落中间盘"
        );

        sessions.save(&info.id).unwrap();
        let gbk_fixture = encoding_rs::GBK.encode("你好\n世界").0.into_owned();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            gbk_fixture,
            "真盘字节=GBK 夹具"
        );

        // 盘上真实性：重开检测回 GBK，且收口后的 encoding 字段与盘一致
        sessions.close(&info.id).unwrap();
        let info2 = sessions.open(&path).unwrap();
        assert_eq!(info2.encoding, EncodingKind::Gbk);
        assert_eq!(info2.preferred_encoding, None, "save 已消费档位");
    }

    #[test]
    #[allow(non_snake_case)]
    fn setEncoding_lossyChars_reportedNotSilent() {
        let dir = tmpdir("enc-lossy");
        let path = dir.join("mix.txt");
        // 夹具含真 '?'（可映射混计臂）与 é（cp1252 可映射臂）——都不许进丢失数
        std::fs::write(&path, "中文? café").unwrap();

        let sessions = EditorSessions::new();
        let info = sessions.open(&path).unwrap();
        let p = sessions
            .set_encoding(&info.id, EncodingKindDto::Latin1, EolChoice::Preserve)
            .unwrap();
        assert_eq!(
            p.replacement_char_count, 2,
            "两枚中文在 WINDOWS_1252 不可映射；真 '?' 与 é 不得混计数"
        );
        assert!(
            p.chars_after > p.chars_before,
            "WHATWG 单字节族不可映射=数字字符引用展开（中→&#20013;），预览如实报膨胀"
        );
        // 预览如实报 >0 即为达标——静默是罪
    }

    #[test]
    #[allow(non_snake_case)]
    fn preferredNone_keepsLegacySaveVerbatim() {
        let dir = tmpdir("enc-legacy");
        let path = dir.join("legacy_gbk.txt");
        let fixture = encoding_rs::GBK.encode("你好\r\n世界").0.into_owned();
        std::fs::write(&path, &fixture).unwrap();

        let sessions = EditorSessions::new();
        let info = sessions.open(&path).unwrap();
        assert_eq!(info.encoding, EncodingKind::Gbk);
        assert_eq!(info.preferred_encoding, None);
        // 不设档直接保存=旧"保持原编码"恒等（正对照，字节零变）
        sessions.save(&info.id).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), fixture);
    }

    #[test]
    #[allow(non_snake_case)]
    fn eolNormalize_onlyLineEnders_touched() {
        let dir = tmpdir("eol-only");
        let path = dir.join("trail.txt");
        // LF 主导 + 空行 + 尾换行三特征同夹具：任何"补行/吞行/双 \r"都会现形
        std::fs::write(&path, b"a\n\nb\n").unwrap();

        let sessions = EditorSessions::new();
        let info = sessions.open(&path).unwrap();
        assert_eq!(info.eol, Eol::Lf);
        // EOL setter 与编码同命令：encoding 传当前档（utf8）恒等，只改行尾
        sessions
            .set_encoding(&info.id, EncodingKindDto::Utf8, EolChoice::Crlf)
            .unwrap();
        assert_eq!(sessions.list()[0].eol, Eol::Crlf);
        sessions.save(&info.id).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"a\r\n\r\nb\r\n",
            "仅行尾展开：空行/尾换行原样保留，不新增也不丢失行结束符"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn autosaveDraft_resumesOnOpen() {
        use std::time::{Duration, SystemTime};
        let dir = tmpdir("draft");
        let path = dir.join("doc.txt");
        std::fs::write(&path, "磁盘版本".as_bytes()).unwrap();
        let draft = autosave_path(&path);
        let file_m = std::fs::metadata(&path).unwrap().modified().unwrap();
        let set_mtime = |p: &Path, t: SystemTime| {
            let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
            f.set_modified(t).unwrap();
        };

        // 无草稿：open 不提示（mtime 事实源缺失臂）
        let s0 = EditorSessions::new();
        assert!(!s0.open(&path).unwrap().autosave_draft);

        // 旧草稿（比盘上文件旧）：不提示且恢复拒——陈旧草稿覆盖编辑缓冲是撒谎
        std::fs::write(&draft, "陈旧草稿".as_bytes()).unwrap();
        set_mtime(&draft, file_m - Duration::from_secs(3600));
        let s1 = EditorSessions::new();
        let info1 = s1.open(&path).unwrap();
        assert!(!info1.autosave_draft);
        assert!(s1.recover_draft(&info1.id).is_err());

        // 新草稿：open 提示 → 恢复回读内容并置脏 → save 后草稿清理
        std::fs::write(&draft, "草稿内容".as_bytes()).unwrap();
        set_mtime(&draft, file_m + Duration::from_secs(60));
        let sessions = EditorSessions::new();
        let info = sessions.open(&path).unwrap();
        assert!(info.autosave_draft, "草稿较新必须提示恢复");
        let after = sessions.recover_draft(&info.id).unwrap();
        assert!(after.dirty, "恢复=未保存事实");
        assert!(!after.autosave_draft, "已消费不再重复提示");
        assert_eq!(sessions.content(&info.id).unwrap(), "草稿内容");
        sessions.save(&info.id).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), "草稿内容".as_bytes());
        assert!(!draft.exists(), "正常保存清草稿（既有纪律同源）");
    }
}
