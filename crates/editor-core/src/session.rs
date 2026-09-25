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
use crate::session_store::SessionMeta;

/// 自动保存文件后缀（脏后由前端防抖调用 autosave 写入；正常保存后清理）
pub const AUTOSAVE_SUFFIX: &str = ".nforge-autosave";

/// 大文件阈值（字节）
pub const BIG_FILE_HIGHLIGHT: u64 = 5 * 1024 * 1024;
pub const HUGE_FILE_READONLY: u64 = 50 * 1024 * 1024;

/// 文本编码（保存时按原编码回写）
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// 光标所在行（1 起；T-B7-20 随 autosave 更新、清单恢复后首载定位）
    pub cursor_line: u32,
    /// 打开时刻（Unix 毫秒；T-B7-20 清单排序锚，兼页签序）
    pub opened_ms: i64,
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
    /// T-B7-20 懒读：清单恢复的行=true 前不触碰文件内容（点开首拉时才读盘+检测）
    loaded: bool,
    /// 光标行（1 起；随 autosave 顺带更新）
    cursor_line: u32,
    /// 打开时刻（Unix 毫秒）；页签序锚
    opened_ms: i64,
}

/// 全部会话管理（模块级单例；RwLock 串行化——文本编辑低频重操作）
pub struct EditorSessions {
    sessions: RwLock<HashMap<String, Session>>,
    /// 清单目录（None=纯内存不落清单，既有测试/无宿主形态零扰动）
    store_dir: Option<PathBuf>,
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
            store_dir: None,
        }
    }

    /// 带清单目录的构造（EditorModule 用 `{app_data}/editor`）
    pub fn with_store(store_dir: PathBuf) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            store_dir: Some(store_dir),
        }
    }

    /// 打开文件：读原始字节 → 编码检测 → 解码 → EOL 检测（不统一，仅标记）
    /// 若存在比盘上文件更新的 autosave 草稿，置 `autosave_draft`（承重⑨"写了没人读"
    /// 死面收口：open 只探测不改内容，恢复经 `recover_draft` 显式口，UI 提示二选一）
    pub fn open(&self, path: &Path) -> Result<SessionInfo> {
        let raw = read_source(path)?;
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
            cursor_line: 1,
            opened_ms: now_ms(),
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
                loaded: true,
                cursor_line: 1,
                opened_ms: info.opened_ms,
            },
        );
        Ok(info)
    }

    /// 取内容（前端打开后拉取一次；>50MB 只读也返回——查看器分片由前端处理）。
    /// T-B7-20 懒读口：清单恢复的行在此**首拉才读盘**（此前只 stat 过），
    /// 读+检测完成后行为与 open() 会话全等。
    pub fn content(&self, id: &str) -> Result<String> {
        self.ensure_loaded(id)?;
        let map = self.lock();
        let s = map
            .get(id)
            .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
        Ok(s.content.clone())
    }

    /// 懒读收口：未载入 → 读盘+解码+EOL/草稿探测回填（清单恢复行的唯一文件读点）
    fn ensure_loaded(&self, id: &str) -> Result<()> {
        let path = {
            let map = self.lock();
            let s = map
                .get(id)
                .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
            if s.loaded {
                return Ok(());
            }
            s.path.clone()
        };
        // 读+解码在锁外（IO 不持锁，与 save 同纪律）
        let raw = read_source(&path)?;
        let size = raw.len() as u64;
        let (encoding, content) = decode(&raw)?;
        let (_, eol_mixed) = detect_eol(&content);
        let autosave_draft = draft_newer_than(&path);
        let mut map = self.lock();
        if let Some(s) = map.get_mut(id) {
            s.encoding = encoding;
            // eol 保留清单里的用户选择档（检测只补 mixed 事实）
            s.eol_mixed = eol_mixed;
            s.autosave_draft = autosave_draft;
            s.size = size;
            s.content = content;
            s.loaded = true;
        }
        Ok(())
    }

    /// 更新内容并置脏标记
    pub fn update(&self, id: &str, content: &str) -> Result<bool> {
        self.ensure_loaded(id)?;
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
        // 懒读防线：未载入的清单行直接保存=拿空缓冲覆盖盘上真文，必须先读
        self.ensure_loaded(id)?;
        // 一次取锁同时拿 path 与编码后内容（COR-04：两次取锁之间会话可能被
        // close/reload，TOCTOU 会把内容写进旧路径）
        let (path, raw, info) = {
            let mut map = self.lock();
            let s = map
                .get_mut(id)
                .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
            // 整文件统一 EOL；转码时序=保存时算不落中间盘（T-B7-18）
            let unified = normalize_eol(&s.content, s.eol);
            let bytes = encode(&unified, s.effective_encoding());
            (s.path.clone(), bytes, session_info(id, s))
        };
        // 写锁已释放再写文件（避免 IO 慢操作持锁）。
        // COR-04：原子写（同目录 tmp + fsync + rename）——fs::write 先截断再写，
        // 写中途崩溃/断电/磁盘满 = 源文件截断且草稿已删，数据不可恢复。
        write_atomic(&path, &raw)?;
        // 仅原子替换成功后才删草稿（失败路径草稿保留 = 仍有恢复依据）
        let _ = std::fs::remove_file(autosave_path(&path));
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
        self.ensure_loaded(id)?;
        let raw = {
            let map = self.lock();
            let s = map
                .get(id)
                .ok_or_else(|| EditorError::NotFound(id.to_string()))?;
            let unified = normalize_eol(&s.content, s.eol);
            encode(&unified, s.effective_encoding())
        };
        // COR-04：与 save 同一原子写语义
        write_atomic(target, &raw)?;
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
        self.ensure_loaded(id)?;
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
        // 草稿内容已进驻：清单恢复行不得再被首拉覆盖（loaded 收口）
        s.loaded = true;
        Ok(session_info(id, s))
    }

    /// 自动保存草稿（脏内容写 `<path>.nforge-autosave`；崩溃恢复入口）。
    /// T-B7-20：顺带携光标行（Some=更新，清单里重启定位的事实源；None=不动）
    pub fn autosave(&self, id: &str, content: &str, cursor_line: Option<u32>) -> Result<bool> {
        self.ensure_loaded(id)?;
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
            if let Some(line) = cursor_line {
                s.cursor_line = line.max(1);
            }
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

    /// 全部会话列表（T-B7-20：按 opened_ms 升序=页签序，HashMap 随机序不再外泄）
    pub fn list(&self) -> Vec<SessionInfo> {
        let mut out: Vec<SessionInfo> = self
            .lock()
            .iter()
            .map(|(id, s)| session_info(id, s))
            .collect();
        out.sort_by_key(|i| i.opened_ms);
        out
    }

    /// 把当前标签行写清单（T-B7-20）。store 未装配 ⇒ no-op（纯内存形态零扰动）；
    /// 写失败仅 warn——清单=可观测数据谱（T-B7-14 裁决族），编辑主流程不许被它阻塞。
    /// 由 src-tauri 命令层在列表变更后调用（open/close/save/save_as/autosave/set_encoding）。
    pub fn save_manifest(&self) {
        let Some(dir) = self.store_dir.clone() else {
            return;
        };
        let rows: Vec<SessionMeta> = {
            let map = self.lock();
            let mut rows: Vec<SessionMeta> = map
                .values()
                .map(|s| SessionMeta {
                    path: s.path.display().to_string(),
                    cursor_line: s.cursor_line,
                    eol: s.eol,
                    preferred_encoding: s.preferred_encoding,
                    opened_ms: s.opened_ms,
                })
                .collect();
            rows.sort_by_key(|r| r.opened_ms);
            rows
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(error = %e, "会话清单目录创建失败");
            return;
        }
        if let Err(e) = crate::session_store::write_manifest(&dir, &rows) {
            tracing::warn!(error = %e, "会话清单写入失败（不阻塞编辑）");
        }
    }

    /// 启动恢复标签行（EditorModule::start 调用；承"恢复的是行非内容"）。
    /// 盘上已删 ⇒ 弃行 + warn 点名（不显示死标签）；坏档 ⇒ `.corrupt` 留证 + 空继续；
    /// 已在表的同路径行跳过（幂等：二次 start 不双份）。
    pub fn load_manifest(&self) -> crate::session_store::ManifestReport {
        let Some(dir) = self.store_dir.clone() else {
            return Default::default();
        };
        let (rows, corrupt) = crate::session_store::read_manifest(&dir);
        let mut report = crate::session_store::ManifestReport {
            corrupt,
            ..Default::default()
        };
        let mut map = self.lock();
        for meta in rows {
            let path = PathBuf::from(&meta.path);
            let already = map.values().any(|s| s.path == path);
            if already {
                continue;
            }
            // stat 校验（不读内容）：文件已删的行不恢复
            let Ok(md) = std::fs::metadata(&path) else {
                tracing::warn!(path = %meta.path, "会话清单行对应文件已不在盘上，弃行");
                report.dropped.push(meta.path);
                continue;
            };
            let size = md.len();
            let id = uuid::Uuid::now_v7().to_string();
            map.insert(
                id,
                Session {
                    path,
                    // 检测档待首拉回填：未载入期 effective=preferred.unwrap_or(Utf8)
                    encoding: meta.preferred_encoding.unwrap_or(EncodingKind::Utf8),
                    preferred_encoding: meta.preferred_encoding,
                    autosave_draft: false,
                    eol: meta.eol,
                    eol_mixed: false,
                    dirty: false,
                    size,
                    content: String::new(),
                    loaded: false,
                    cursor_line: meta.cursor_line.max(1),
                    opened_ms: meta.opened_ms,
                },
            );
            report.restored += 1;
        }
        report
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
        cursor_line: s.cursor_line,
        opened_ms: s.opened_ms,
    }
}

/// 打开时刻（Unix 毫秒）。系统时钟早于 epoch 的理论臂归 0——排序锚退化不影响正确性。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

// 文件内容**唯一读点**计数（任务书行"FakeFS 计"的收口形态：不引入文件抽象层，
// 懒读判据「首开前零内容读」以漏斗计数在 cfg(test) 下机检——偏已在提交信息登记）。
// thread_local：cargo 测试并行，进程级计数会被其他用例如 open() 污染。
// （普通注释而非 ///：doc 注释挂 macro 调用项上会被 rustc 判 unused doc comment）
#[cfg(test)]
thread_local! {
    static TEST_SOURCE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn test_source_reads() -> usize {
    TEST_SOURCE_READS.with(|c| c.get())
}

#[cfg(test)]
pub(crate) fn reset_test_source_reads() {
    TEST_SOURCE_READS.with(|c| c.set(0));
}

/// 读原始字节（一切文件内容读取都过此口；stat/mtime 探测不算内容读）
fn read_source(path: &Path) -> Result<Vec<u8>> {
    #[cfg(test)]
    TEST_SOURCE_READS.with(|c| c.set(c.get() + 1));
    Ok(std::fs::read(path)?)
}

fn autosave_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}{AUTOSAVE_SUFFIX}", path.display()))
}

/// 原子写（COR-04）：同目录临时文件 → fsync → rename。复用 host-core 收敛的
/// 全仓唯一落盘入口（失败清理 tmp，原文件保持完整；Windows rename 覆盖目标）。
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    host_core::util::write_atomic(path, bytes).map_err(EditorError::Io)
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
        sessions
            .autosave(&info.id, "# title\nchanged", None)
            .unwrap();
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
    fn save_failure_keeps_source_and_draft_intact() {
        // COR-04：保存失败（写目标被只读占位）→ 原文件不被截断、草稿保留。
        // 原子写先写 tmp 再 rename；目标只读时 rename 失败 → 源文件完整无损。
        let dir = tmpdir("savefail");
        let path = dir.join("doc.txt");
        std::fs::write(&path, b"original").unwrap();

        let sessions = EditorSessions::new();
        let info = sessions.open(&path).unwrap();
        sessions.update(&info.id, "changed content").unwrap();
        sessions
            .autosave(&info.id, "changed content", None)
            .unwrap();
        let draft = path.with_file_name("doc.txt.nforge-autosave");
        assert!(draft.is_file());

        // 目标文件置只读 → 原子替换的 rename 失败
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();

        let r = sessions.save(&info.id);
        // Windows 下只读目标的 rename 覆盖被拒；若平台放行则保存成功也成立
        if r.is_err() {
            assert_eq!(
                std::fs::read(&path).unwrap(),
                b"original",
                "失败路径源文件必须保持完整"
            );
            assert!(draft.is_file(), "失败路径草稿必须保留（唯一恢复依据）");
            assert!(sessions.list()[0].dirty, "失败不得清脏标记");
        }
        // 还原只读位清理
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    #[test]
    fn save_writes_atomically_no_truncate_window() {
        // COR-04 正向对照：保存成功后文件内容正确、无残留 tmp 文件
        let dir = tmpdir("atomic-ok");
        let path = dir.join("ok.txt");
        std::fs::write(&path, b"v1").unwrap();
        let sessions = EditorSessions::new();
        let info = sessions.open(&path).unwrap();
        sessions.update(&info.id, "v2").unwrap();
        sessions.save(&info.id).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"v2");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("nf-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "不得残留临时文件: {leftovers:?}");
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

    // ======================== T-B7-20 会话持久化（标签行清单） ========================

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-20）字面测试名优先于 rustc 命名惯例
    fn sessionManifest_roundTripsReopen() {
        let dir = tmpdir("manifest");
        let store = dir.join("store");
        let path = dir.join("note.txt");
        let path2 = dir.join("second.md");
        std::fs::write(&path, "第一行\n第二行\n第三行\n".as_bytes()).unwrap();
        std::fs::write(&path2, b"# doc").unwrap();

        let s1 = EditorSessions::with_store(store.clone());
        let info = s1.open(&path).unwrap();
        // 毫秒级排序锚：两连开可能同 ms（HashMap 序退化非确定），睡一拍钉死页签序
        std::thread::sleep(std::time::Duration::from_millis(3));
        s1.open(&path2).unwrap();
        // 光标行随 autosave 顺带更新（零新命令承载）；档位切换入清单
        s1.autosave(&info.id, "改后内容", Some(7)).unwrap();
        s1.set_encoding(&info.id, EncodingKindDto::Gbk, EolChoice::Crlf)
            .unwrap();
        s1.save_manifest();
        assert!(crate::session_store::manifest_path(&store).is_file());

        // 新进程形态：全新实例只读清单——两标签关重开列表在场
        let s2 = EditorSessions::with_store(store);
        let report = s2.load_manifest();
        assert_eq!(report.restored, 2);
        assert!(report.dropped.is_empty());
        assert!(!report.corrupt);
        let rows = s2.list();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "note.txt");
        assert_eq!(rows[1].name, "second.md", "opened_ms 升序=页签序");
        assert_eq!(rows[0].path, path.display().to_string());
        assert_eq!(rows[0].cursor_line, 7, "重启定位的事实源在清单里");
        assert_eq!(rows[0].preferred_encoding, Some(EncodingKind::Gbk));
        assert_eq!(rows[0].eol, Eol::Crlf, "用户行尾选择随行回归");
        assert_eq!(rows[1].cursor_line, 1, "未动过的行回到默认位");
        // 点开首拉：恢复的是行非内容——内容=盘上真相（未保存的编辑器缓冲不在
        // 清单职责内，较新草稿经 autosave_draft 提示通道走 recover_draft）
        assert_eq!(s2.content(&rows[0].id).unwrap(), "第一行\n第二行\n第三行\n");
        assert_eq!(s2.list()[0].size, "第一行\n第二行\n第三行\n".len() as u64);
        // 幂等：二次 load 不双份
        assert_eq!(s2.load_manifest().restored, 0);
        assert_eq!(s2.list().len(), 2);
    }

    #[test]
    #[allow(non_snake_case)]
    fn sessionManifest_deletedOnDisk_rowDroppedWithWarn() {
        let dir = tmpdir("manifest-dead");
        let store = dir.join("store");
        let path = dir.join("doomed.txt");
        std::fs::write(&path, b"x").unwrap();

        let s1 = EditorSessions::with_store(store.clone());
        s1.open(&path).unwrap();
        s1.save_manifest();
        std::fs::remove_file(&path).unwrap();

        let s2 = EditorSessions::with_store(store);
        let report = s2.load_manifest();
        assert_eq!(report.restored, 0);
        assert_eq!(
            report.dropped,
            vec![path.display().to_string()],
            "弃行必须点名"
        );
        assert!(s2.list().is_empty(), "死标签不上屏");
    }

    #[test]
    #[allow(non_snake_case)]
    fn sessionRestore_lazyRead_noFileAccessUntilOpen() {
        let dir = tmpdir("manifest-lazy");
        let store = dir.join("store");
        let path = dir.join("big.txt");
        std::fs::write(&path, "内容字节").unwrap();

        let s1 = EditorSessions::with_store(store.clone());
        s1.open(&path).unwrap();
        s1.save_manifest();

        reset_test_source_reads();
        let s2 = EditorSessions::with_store(store);
        assert_eq!(s2.load_manifest().restored, 1);
        assert_eq!(s2.list().len(), 1);
        assert_eq!(
            test_source_reads(),
            0,
            "恢复只 stat 不读内容——启动扫大盘是任务书行点名的负形"
        );
        // 点开才读：首拉恰一次内容读
        let id = s2.list()[0].id.clone();
        s2.content(&id).unwrap();
        assert_eq!(test_source_reads(), 1);
        // 已载入后再次取内容不再触盘
        s2.content(&id).unwrap();
        assert_eq!(test_source_reads(), 1);
    }

    #[test]
    #[allow(non_snake_case)]
    fn sessionManifest_corruptFile_emptyWithWarn() {
        let dir = tmpdir("manifest-corrupt");
        let store = dir.join("store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(crate::session_store::manifest_path(&store), b"{{{ broken").unwrap();

        let s = EditorSessions::with_store(store.clone());
        let report = s.load_manifest();
        assert!(report.corrupt, "损坏必须上报（静默=谎报）");
        assert_eq!(report.restored, 0);
        assert!(s.list().is_empty(), "坏档按空清单继续，不锁死启动");
        assert!(
            crate::session_store::manifest_path(&store)
                .with_extension("json.corrupt")
                .exists(),
            "损坏档改名留证"
        );
    }
}
