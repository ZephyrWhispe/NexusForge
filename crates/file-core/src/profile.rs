//! T-B6-1（09 §6.2）：远程连接档案数据面。
//!
//! 承重⑪：本文件是**档案**（站点清单）面，结构上不含任何口令字段——
//! `AuthKind` 只携带 `key_path`/`entry_id` 两枚指针。口令的逐次入参面
//! （`AuthSecret`）归 T-B6-3 起，且只进不出。
//! 承重①：档案 id 恒带 `remote:` 前缀，`"local"` 为 Reserved——
//! `DriverRegistry::register` 同 id 顶替 + notes-core 消费 `driver("local")`，
//! 前缀使顶替在字符串层面结构性不可达。

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::FileError;

/// 本批协议全集（§6.3：无 Netdisk/Rclone 档）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteProtocol {
    WebDav,
    Https,
    Sftp,
    Ftp,
}

impl RemoteProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            RemoteProtocol::WebDav => "webdav",
            RemoteProtocol::Https => "https",
            RemoteProtocol::Sftp => "sftp",
            RemoteProtocol::Ftp => "ftp",
        }
    }
    /// 该协议是否以明文过网（insecure_plaintext 总闸的对象，T-B6-6）
    pub fn is_plaintext(self) -> bool {
        self == RemoteProtocol::Ftp
    }
}

/// 认证方式——**只有指针与标记，没有口令值**（承重⑪）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AuthKind {
    Anonymous,
    SshKey { key_path: String },
    VaultEntry { entry_id: String },
    PromptEachTime,
    SessionPassword,
}

/// 凭据**来源**（T-B6-8）：`file_remote_drivers` 返回体的一列——只说来源不说值
/// （对齐 B5 `addr_source` 单源纪律；值一列都不出连接面）。`Typed` = 每次连接
/// 现打的凭据（PromptEachTime），`Session` = 本会话口令（SessionPassword）——
/// 两档的值都只在进程内逐次过手，永不落盘。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthSource {
    Anonymous,
    KeyFile,
    VaultEntry,
    Session,
    Typed,
}

/// 档案 → 来源（纯函数唯一投影口：`RemoteDriverInfo::auth_source` 列的算式）
pub fn auth_source_of(p: &RemoteProfile) -> AuthSource {
    match &p.auth {
        AuthKind::Anonymous => AuthSource::Anonymous,
        AuthKind::SshKey { .. } => AuthSource::KeyFile,
        AuthKind::VaultEntry { .. } => AuthSource::VaultEntry,
        AuthKind::SessionPassword => AuthSource::Session,
        AuthKind::PromptEachTime => AuthSource::Typed,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RemoteProfile {
    pub id: String,
    pub label: String,
    pub protocol: RemoteProtocol,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub base_path: String,
    pub auth: AuthKind,
    #[serde(default)]
    pub preset_id: Option<String>,
    #[serde(default)]
    pub last_used_ms: i64,
}

/// `remote:` 前缀派生（唯一造 id 的口，见 §6.1 ①）
pub fn profile_id_of(name: &str) -> String {
    format!("remote:{name}")
}

pub fn looks_like_loopback(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host.starts_with("[::1]")
}

fn has_scheme_prefix(host: &str) -> bool {
    [
        "http://",
        "https://",
        "ftp://",
        "dav://",
        "sftp://",
        "webdav://",
    ]
    .iter()
    .any(|p| host.starts_with(p))
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

fn is_drive_prefixed(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

/// id 主体（去 `remote:` 前缀后）的文件名安全形状
fn id_slug_ok(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// 写侧纯校验（唯一裁决口）。id 类问题 → FILE_REMOTE_002，其余字段 →
/// FILE_REMOTE_005，错误逐字段点名。明文 FTP + 非回环 host **此处不拒**——
/// 运行期由 `insecure_plaintext` 总闸（T-B6-6，FILE_REMOTE_006/007）拦截；
/// 两闸分工在报错消息里互相点名。
pub fn validate_profile(p: &RemoteProfile) -> Result<(), FileError> {
    let mut id_errs: Vec<String> = Vec::new();
    let mut field_errs: Vec<String> = Vec::new();

    if p.id == "local" {
        id_errs.push(format!(
            "id {:?} 占用保留 id：\"local\" 归 LocalDriver，远端档案永不得顶替（notes-core 经 driver(\"local\") 取存储）",
            p.id
        ));
    } else if p.id.is_empty() {
        id_errs.push(
            "id 不得为空（save 缺 id 时由 store 派生，直调本函数须给带 remote: 前缀的 id）".into(),
        );
    } else if let Some(slug) = p.id.strip_prefix("remote:") {
        if !id_slug_ok(slug) {
            id_errs.push(format!(
                "id {:?} 主体非法：`remote:` 之后只许小写字母/数字/-/_，长度 1..=64",
                p.id
            ));
        }
    } else {
        id_errs.push(format!(
            "id {:?} 非法：必须以 `remote:` 开头（防顶替 DriverRegistry 的 \"local\"）",
            p.id
        ));
    }

    if p.label.trim().is_empty() {
        field_errs.push("label 不得为空".into());
    } else if has_control(&p.label) {
        field_errs.push("label 不得含控制字符".into());
    }

    if p.host.is_empty() {
        field_errs.push("host 不得为空".into());
    } else {
        if has_scheme_prefix(&p.host) {
            field_errs.push(format!(
                "host {:?} 含 scheme 前缀：协议由 protocol 字段表达，host 只放主机名",
                p.host
            ));
        }
        if p.host.chars().any(|c| c.is_whitespace() || c.is_control()) {
            field_errs.push("host 不得含空白或控制字符（CRLF 注入面）".into());
        }
    }

    if p.port == 0 {
        field_errs.push("port 不得为 0".into());
    }

    if !p.base_path.starts_with('/') && !is_drive_prefixed(&p.base_path) {
        field_errs.push(format!(
            "base_path {:?} 非法：须以 `/` 或盘符（如 `D:/`）开头",
            p.base_path
        ));
    }

    match &p.auth {
        AuthKind::SshKey { key_path } if key_path.trim().is_empty() => {
            field_errs.push("auth.key_path 不得为空".into());
        }
        AuthKind::VaultEntry { entry_id } if entry_id.trim().is_empty() => {
            field_errs.push("auth.entry_id 不得为空".into());
        }
        _ => {}
    }

    if !id_errs.is_empty() {
        return Err(FileError::Remote {
            code: crate::error::FILE_REMOTE_ID,
            msg: format!("档案校验失败：{}", id_errs.join("；")),
        });
    }
    if !field_errs.is_empty() {
        let mut msg = format!("档案校验失败：{}", field_errs.join("；"));
        if p.protocol.is_plaintext() && !looks_like_loopback(&p.host) {
            msg.push_str(
                "（注：明文 FTP + 非回环 host 不在档案闸拒绝，由运行期 insecure_plaintext 总闸拦截，码 FILE_REMOTE_006/007，见 09 §6.2 T-B6-6）",
            );
        }
        return Err(FileError::Remote {
            code: crate::error::FILE_REMOTE_FIELD,
            msg,
        });
    }
    Ok(())
}

/// 档案存储：`{app_data}/file/profiles/`，每档一份 `remote--{slug}.json`，
/// tmp+sync+rename 原子写（沿用 ops.rs persist_pending 形状）。
pub struct ProfileStore {
    dir: PathBuf,
    profiles: RwLock<Vec<RemoteProfile>>,
}

impl ProfileStore {
    /// 打开目录并全量读入。目录不存在 = 首用，建空目录非错。
    /// 盘上档案解析失败即 `Err`（fail-closed：静默跳过等于丢用户的站点）。
    pub fn open(dir: &Path) -> Result<Self, FileError> {
        std::fs::create_dir_all(dir)?;
        let mut profiles = Vec::new();
        for item in std::fs::read_dir(dir)? {
            let path = item?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let raw = std::fs::read(&path)?;
            let p: RemoteProfile = serde_json::from_slice(&raw).map_err(|e| FileError::Remote {
                code: crate::error::FILE_REMOTE_MISSING,
                msg: format!("档案文件 {} 解析失败: {e}", path.display()),
            })?;
            profiles.push(p);
        }
        profiles.sort_by(|a, b| {
            b.last_used_ms
                .cmp(&a.last_used_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(Self {
            dir: dir.to_path_buf(),
            profiles: RwLock::new(profiles),
        })
    }

    pub fn list(&self) -> Vec<RemoteProfile> {
        self.profiles.read().clone()
    }

    pub fn get(&self, id: &str) -> Option<RemoteProfile> {
        self.profiles.read().iter().find(|p| p.id == id).cloned()
    }

    fn file_name(id: &str) -> String {
        format!("{}.json", id.replace(':', "--"))
    }

    /// 写侧校验后落盘；`id` 为空则现场派生（uuid v7 主体）。
    pub fn save(&self, mut profile: RemoteProfile) -> Result<RemoteProfile, FileError> {
        if profile.id.is_empty() {
            profile.id = profile_id_of(&uuid::Uuid::now_v7().simple().to_string());
        }
        validate_profile(&profile)?;
        let name = Self::file_name(&profile.id);
        let path = self.dir.join(&name);
        let tmp = self.dir.join(format!("{name}.tmp"));
        let bytes = serde_json::to_vec_pretty(&profile).map_err(|e| FileError::Remote {
            code: crate::error::FILE_REMOTE_FIELD,
            msg: e.to_string(),
        })?;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        let mut v = self.profiles.write();
        match v.iter_mut().find(|p| p.id == profile.id) {
            Some(slot) => *slot = profile.clone(),
            None => v.push(profile.clone()),
        }
        Ok(profile)
    }

    /// 幂等删除：档案不存在回 `Ok(false)`，不 Err 不 panic。
    pub fn delete(&self, id: &str) -> Result<bool, FileError> {
        let existed = {
            let mut v = self.profiles.write();
            let before = v.len();
            v.retain(|p| p.id != id);
            before != v.len()
        };
        if !existed {
            return Ok(false);
        }
        let path = self.dir.join(Self::file_name(id));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_file_profile_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample(id: &str) -> RemoteProfile {
        RemoteProfile {
            id: id.into(),
            label: "测试站".into(),
            protocol: RemoteProtocol::WebDav,
            host: "dav.example.com".into(),
            port: 443,
            user: "me".into(),
            base_path: "/remote".into(),
            auth: AuthKind::PromptEachTime,
            preset_id: None,
            last_used_ms: 1_700_000_000_000,
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-1）字面测试名优先于 rustc 命名惯例
    fn profileStore_roundTripPersistedAndReopened() {
        let d = tmpdir("roundtrip");
        let store = ProfileStore::open(&d).unwrap();
        let mut a = sample("remote:alpha");
        a.last_used_ms = 200;
        let mut b = sample("remote:beta");
        b.protocol = RemoteProtocol::Sftp;
        b.auth = AuthKind::SshKey {
            key_path: "C:/keys/id_ed25519".into(),
        };
        b.last_used_ms = 100;
        store.save(a.clone()).unwrap();
        store.save(b.clone()).unwrap();

        let reopened = ProfileStore::open(&d).unwrap();
        let got = reopened.list();
        assert_eq!(got.len(), 2, "重开后两档仍可查");
        assert_eq!(got[0], a, "last_used_ms 保真且新使用者排前");
        assert_eq!(got[1], b);
        assert_eq!(reopened.get("remote:beta").unwrap().auth, b.auth);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-1）字面测试名优先于 rustc 命名惯例
    fn profileStore_emptyDir_isNotAnError() {
        // 正对照防空洞：新目录列表为 Ok(vec![]) 而非 Err
        let d = tmpdir("empty");
        let store = ProfileStore::open(&d.join("fresh")).unwrap();
        assert!(store.list().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-1）字面测试名优先于 rustc 命名惯例
    fn profileSave_noCredentialField_serializedKeySetExactlyMatches() {
        let d = tmpdir("nosecret");
        let store = ProfileStore::open(&d).unwrap();
        let saved = store.save(sample("remote:shape")).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "auth",
                "base_path",
                "host",
                "id",
                "label",
                "last_used_ms",
                "port",
                "preset_id",
                "protocol",
                "user",
            ]
        );
        assert!(json.get("auth").is_some(), "正对照：auth 键确在场");
        let on_disk = std::fs::read_to_string(d.join("remote--shape.json")).expect("落盘文件应在");
        for word in ["password", "secret", "token"] {
            assert!(!on_disk.contains(word), "盘上档案含禁词 {word}");
            assert!(!serde_json::to_string(&saved).unwrap().contains(word));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-1）字面测试名优先于 rustc 命名惯例
    fn profileSave_rejectsReservedLocalId() {
        let d = tmpdir("reserved");
        let store = ProfileStore::open(&d).unwrap();
        // 保留 id："local" → FILE_REMOTE_002 且点名
        let e = store.save(sample("local")).unwrap_err();
        assert!(
            matches!(&e, FileError::Remote { code, msg } if *code == crate::error::FILE_REMOTE_ID && msg.contains("local")),
            "保留 id 须以 FILE_REMOTE_002 点名，实得 {e}"
        );
        // 其余坏 id 形状同样结构性排除（消息点名坏值）
        for bad in ["remote:UPPER", "webdav", "remote:"] {
            let e = store.save(sample(bad)).unwrap_err();
            assert!(
                matches!(&e, FileError::Remote { code, msg } if *code == crate::error::FILE_REMOTE_ID && msg.contains(bad)),
                "id {bad:?} 应被点名拒绝，实得 {e}"
            );
        }
        assert!(store.list().is_empty(), "被拒档案不得半落盘");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-1）字面测试名优先于 rustc 命名惯例
    fn profileSave_badHostOrPort_rejectedNamingField() {
        let d = tmpdir("badfields");
        let store = ProfileStore::open(&d).unwrap();
        let mut cases = Vec::new();
        {
            let mut q = sample("remote:h1");
            q.host = "http://dav.example.com".into();
            cases.push((q, "host"));
            let mut q = sample("remote:h2");
            q.host = "a b.com".into();
            cases.push((q, "host"));
            let mut q = sample("remote:h3");
            q.host = "ok.com\r\nEHLO".into();
            cases.push((q, "host"));
            let mut q = sample("remote:h4");
            q.port = 0;
            cases.push((q, "port"));
            let mut q = sample("remote:h5");
            q.base_path = "relative/dir".into();
            cases.push((q, "base_path"));
        }
        for (p, expect) in cases {
            let e = store.save(p).unwrap_err();
            assert!(
                matches!(&e, FileError::Remote { code, msg } if *code == crate::error::FILE_REMOTE_FIELD && msg.contains(expect)),
                "{expect} 应被点名，实得 {e}"
            );
        }
        // 明文 FTP + 非回环 host：档案闸**不拒**（形状合法即保存成功）
        let mut ftp = sample("remote:ftp1");
        ftp.protocol = RemoteProtocol::Ftp;
        ftp.host = "files.example.org".into();
        ftp.port = 21;
        store.save(ftp).unwrap();
        // 坏形状 + 明文 FTP：消息点名运行期总闸（两闸分工互相点名）
        let mut badftp = sample("remote:ftp2");
        badftp.protocol = RemoteProtocol::Ftp;
        badftp.host = "ftp://files.example.org".into();
        let e = store.save(badftp).unwrap_err().to_string();
        assert!(
            e.contains("insecure_plaintext") && e.contains("FILE_REMOTE_006"),
            "两闸分工须在消息里互相点名: {e}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-1）字面测试名优先于 rustc 命名惯例
    fn profileDelete_missingId_returnsFalseNotError() {
        let d = tmpdir("delete");
        let store = ProfileStore::open(&d).unwrap();
        store.save(sample("remote:gone")).unwrap();
        assert!(store.delete("remote:gone").unwrap());
        assert!(
            !store.delete("remote:gone").unwrap(),
            "幂等删除回 false 而非 Err"
        );
        assert!(!store.delete("remote:never").unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-8）字面测试名优先于 rustc 命名惯例
    fn authKind_serializedHasNoSecretKeys() {
        // 各臂序列化键集 == 声明键集（承重⑪ 的机检面：auth 是 tag enum，
        // 任何一臂都不可能带出凭据位）。两枚指针臂是正对照重点。
        let cases: Vec<(AuthKind, &[&str])> = vec![
            (AuthKind::Anonymous, &["kind"]),
            (
                AuthKind::SshKey {
                    key_path: "C:/keys/id_ed25519".into(),
                },
                &["kind", "key_path"],
            ),
            (
                AuthKind::VaultEntry {
                    entry_id: "0198f2c7-3a4e-7a10-9b6a-2f1c8d5e4b3a".into(),
                },
                &["kind", "entry_id"],
            ),
            (AuthKind::PromptEachTime, &["kind"]),
            (AuthKind::SessionPassword, &["kind"]),
        ];
        for (kind, want_keys) in cases {
            let json = serde_json::to_value(&kind).unwrap();
            let obj = json.as_object().unwrap();
            let mut got: Vec<&str> = obj.keys().map(String::as_str).collect();
            got.sort_unstable();
            let mut want = want_keys.to_vec();
            want.sort_unstable();
            assert_eq!(got, want, "臂 {kind:?} 的序列化键集须恰等声明键集");
            // 禁词扫在"键名 + kind 位之外的值"上：tag 判别值 "session_password"
            // 是档位名不是凭据位（键集恰等声明集已排除凭据键，这里排除的只有
            // 声明内的 tag 字符串本身——判据不放宽，口径说清楚）
            for (k, v) in obj {
                for word in ["password", "secret", "token", "header"] {
                    assert!(!k.contains(word), "auth 序列化键名泄露禁词 {word}: {k}");
                    if k != "kind" {
                        let text = v.to_string();
                        assert!(
                            !text.to_lowercase().contains(word),
                            "auth 值面不得携带禁词 {word}: {text}"
                        );
                    }
                }
            }
        }
        // 指针形状断言（vault_entry_pointer 红线的档案侧一半）：entry_id 是
        // 条目 id 的形状（uuid 主体、无 base64 填充位），不是密文容器的把手
        let id = "0198f2c7-3a4e-7a10-9b6a-2f1c8d5e4b3a";
        assert!(
            uuid::Uuid::parse_str(id).is_ok(),
            "夹具须是合法条目 id 形状"
        );
        assert!(
            !id.contains('=') && id.len() <= 64,
            "指针不得是 base64 blob 形状"
        );
    }
}
