//! 远程连接预设（09 §6.2 T-B6-6）：**数据文件非代码**——内置预设住
//! `presets/default.json`，用户扩展站点的预设是 `{app_data}/file/presets/*.json`，
//! 两路走**同一个校验口** [`parse_preset_file`]（内置若绕过校验口，它就是没测过的代码）。
//!
//! 红线：
//! - 整份裁决：一条坏条目 ⇒ 整个文件不注册，消息点名文件与条目
//!   （禁"跳过坏条目继续"——半份预设比没有预设更难排查）；
//! - 预设零凭据：`auth_kind` 只有 `anonymous`/`prompt_each_time` 两枚枚举位，
//!   类型层面就装不下口令（承重⑪在预设面的延伸）；
//! - 预设不是档案：它不建连接、不入 [`crate::profile::ProfileStore`]，
//!   只是"填表的草稿纸"——落档仍需用户显式保存（T-B6-10 UI 行接线）。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::FileError;
use crate::profile::RemoteProtocol;

/// 预设的认证位（指针/标记的预设安全子集：存管/会话/密钥指针不进预设文件——
/// 那是档案面才有的概念，预设只描述"这个站点长什么样"）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetAuthKind {
    Anonymous,
    PromptEachTime,
}

fn default_base_path() -> String {
    "/".to_owned()
}

fn default_notes() -> String {
    String::new()
}

/// 一条连接预设（键集 = 任务书点名形状，deny_unknown_fields 拒野键）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RemotePreset {
    pub id: String,
    pub label: String,
    pub protocol: RemoteProtocol,
    pub default_host: String,
    pub port: u16,
    #[serde(default = "default_base_path")]
    pub base_path: String,
    pub auth_kind: PresetAuthKind,
    #[serde(default = "default_notes")]
    pub notes: String,
}

/// 内置预设（随二进制发行；与用户文件同口校验——构造期即测）
pub const BUILTIN_PRESETS_JSON: &str = include_str!("../presets/default.json");

fn preset_err(msg: String) -> FileError {
    FileError::Config(msg)
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// 单条裁决：全部问题收集完再回（点名要全，修一处报一处的校验器是折腾人）
fn validate_preset(p: &RemotePreset, seen: &mut Vec<String>, errs: &mut Vec<String>) {
    let slug_ok = !p.id.is_empty()
        && p.id.len() <= 64
        && p.id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !slug_ok {
        errs.push(format!(
            "id {:?} 非法：只许小写字母/数字/-/_，长度 1..=64",
            p.id
        ));
    } else if seen.contains(&p.id) {
        errs.push(format!("id {:?} 在同一文件内重复", p.id));
    } else {
        seen.push(p.id.clone());
    }
    if p.label.trim().is_empty() {
        errs.push(format!("预设 {:?} 的 label 不得为空", p.id));
    } else if has_control(&p.label) {
        errs.push(format!("预设 {:?} 的 label 含控制字符", p.id));
    }
    if p.default_host.is_empty() {
        errs.push(format!("预设 {:?} 的 default_host 不得为空", p.id));
    } else if [
        "http://",
        "https://",
        "ftp://",
        "dav://",
        "sftp://",
        "webdav://",
    ]
    .iter()
    .any(|pre| p.default_host.starts_with(pre))
    {
        errs.push(format!(
            "预设 {:?} 的 default_host {:?} 含 scheme 前缀：协议由 protocol 字段表达",
            p.id, p.default_host
        ));
    } else if p
        .default_host
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        errs.push(format!(
            "预设 {:?} 的 default_host 不得含空白或控制字符（CRLF 注入面）",
            p.id
        ));
    }
    if p.port == 0 {
        errs.push(format!("预设 {:?} 的 port 不得为 0", p.id));
    }
    if !p.base_path.starts_with('/') {
        errs.push(format!(
            "预设 {:?} 的 base_path {:?} 须以 `/` 开头",
            p.id, p.base_path
        ));
    }
    if has_control(&p.notes) {
        errs.push(format!("预设 {:?} 的 notes 含控制字符", p.id));
    }
}

/// 一个预设文件的整份裁决（**唯一校验口**，内置与用户文件共用）。
/// `file` 只用于消息点名。任何坏条目 ⇒ 整份 `Err`（禁跳过坏条目继续注册）。
pub fn parse_preset_file(
    file: &str,
    raw: &serde_json::Value,
) -> Result<Vec<RemotePreset>, FileError> {
    let items = raw
        .get("presets")
        .and_then(|v| v.as_array())
        .ok_or_else(|| preset_err(format!("预设文件 {file} 缺 `presets` 数组根键")))?;
    let mut out = Vec::with_capacity(items.len());
    let mut errs: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        // 条目级反序列化失败也要点名（野键/缺必填/枚举外值都落在这里）；
        // 坏条目不进 out——但 errs 非空已在下面把整份拦死，不存在"半注册"
        match serde_json::from_value::<RemotePreset>(item.clone()) {
            Ok(p) => {
                let before = errs.len();
                validate_preset(&p, &mut seen, &mut errs);
                if errs.len() == before {
                    out.push(p);
                }
            }
            Err(e) => {
                let hinted = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<无 id>")
                    .to_owned();
                errs.push(format!("第 {i} 条（id {hinted:?}）结构非法: {e}"));
            }
        }
    }
    if !errs.is_empty() {
        return Err(preset_err(format!(
            "预设文件 {file} 整份不注册（{} 处问题）：{}",
            errs.len(),
            errs.join("；")
        )));
    }
    Ok(out)
}

/// 预设注册表：内置 + 用户目录（fail-closed：任一用户文件坏 ⇒ 加载 `Err`，
/// 静默丢掉一份坏预设 = 用户下次看见"少了两站"而不知为何）
#[derive(Clone, Debug, Default)]
pub struct PresetStore {
    presets: Vec<RemotePreset>,
}

impl PresetStore {
    /// 仅内置（测试与降级展示用；构造即过同一校验口）
    pub fn builtin() -> Result<Self, FileError> {
        let raw: serde_json::Value = serde_json::from_str(BUILTIN_PRESETS_JSON)
            .map_err(|e| preset_err(format!("内置预设 JSON 解析失败: {e}")))?;
        Ok(Self {
            presets: parse_preset_file("builtin:presets/default.json", &raw)?,
        })
    }

    /// 内置 + `{dir}/*.json`（dir 不存在 = 首用，非错）
    pub fn load(dir: &Path) -> Result<Self, FileError> {
        let mut all = Self::builtin()?.presets;
        if dir.is_dir() {
            let mut files: Vec<_> = std::fs::read_dir(dir)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
                .collect();
            files.sort(); // 目录枚举序不稳，注册序必须确定性可复现
            for path in files {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)
                    .map_err(|e| preset_err(format!("预设文件 {name} 解析失败: {e}")))?;
                let fresh = parse_preset_file(&name, &raw)?;
                for p in fresh {
                    if all.iter().any(|old| old.id == p.id) {
                        return Err(preset_err(format!(
                            "预设文件 {name} 的 id {:?} 与已注册预设冲突（内置 id 不可覆写）",
                            p.id
                        )));
                    }
                    all.push(p);
                }
            }
        }
        Ok(Self { presets: all })
    }

    pub fn list(&self) -> Vec<RemotePreset> {
        self.presets.clone()
    }

    pub fn get(&self, id: &str) -> Option<&RemotePreset> {
        self.presets.iter().find(|p| p.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PresetAuthKind::Anonymous;

    fn entry(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "label": "示例站", "protocol": "web_dav",
            "default_host": "dav.example.com", "port": 443,
            "auth_kind": "prompt_each_time"
        })
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-6）字面测试名优先于 rustc 命名惯例
    fn presets_unknownProtocol_wholeFileRejected() {
        // 好条目 + 一条 protocol "netdisk"：整份拒，且点名文件与坏条目
        let raw = serde_json::json!({ "presets": [ entry("good-one"), {
            "id": "bad-one", "label": "网盘站", "protocol": "netdisk",
            "default_host": "pan.example.com", "port": 443, "auth_kind": "anonymous"
        }]});
        let e = parse_preset_file("user.json", &raw).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("user.json"), "要点名文件，实得 {msg}");
        assert!(msg.contains("bad-one"), "要点名坏条目 id，实得 {msg}");
        assert!(
            matches!(e, FileError::Config(_)),
            "预设面走 FILE_CONFIG_001，实得 {e:?}"
        );
        assert_eq!(e.code(), "FILE_CONFIG_001");
        // 两枚枚举外的协议同样进这一闸
        for proto in ["smb", "ftp "] {
            let raw = serde_json::json!({ "presets": [
                { "id": "p", "label": "x", "protocol": proto,
                  "default_host": "h.example", "port": 21, "auth_kind": "anonymous" }
            ]});
            assert!(
                parse_preset_file("p.json", &raw).is_err(),
                "协议 {proto:?} 须拒"
            );
        }
        // 整份不注册不是部分注册：坏文件之后没有任何条目入表（Err 即零产出）
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-6）字面测试名优先于 rustc 命名惯例
    fn presets_shapeValidated() {
        // 必填缺省：base_path/notes 有默认，protocol/port/auth_kind/id/label/host 无
        let raw = serde_json::json!({ "presets": [entry("d1")] });
        let got = parse_preset_file("ok.json", &raw).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].base_path, "/", "base_path 缺省 = /");
        assert!(got[0].notes.is_empty(), "notes 缺省 = 空串");
        // 缺必填、野键、枚举外 auth_kind、port 越界、scheme 前缀 host、重复 id——全进整份闸
        let mut missing = entry("m1");
        missing.as_object_mut().unwrap().remove("port");
        let mut foreign = entry("m2");
        foreign
            .as_object_mut()
            .unwrap()
            .insert("password".into(), serde_json::json!("x"));
        let mut bad_auth = entry("m3");
        bad_auth
            .as_object_mut()
            .unwrap()
            .insert("auth_kind".into(), serde_json::json!("vault_entry"));
        let mut port0 = entry("m4");
        port0
            .as_object_mut()
            .unwrap()
            .insert("port".into(), serde_json::json!(0));
        let mut scheme_host = entry("m5");
        scheme_host.as_object_mut().unwrap().insert(
            "default_host".into(),
            serde_json::json!("https://x.example"),
        );
        let raw = serde_json::json!({ "presets": [
            missing, foreign, bad_auth, port0, scheme_host, entry("d1"), entry("d1")
        ]});
        let e = parse_preset_file("bad.json", &raw).unwrap_err();
        let msg = e.to_string();
        for probe in ["port", "password", "vault_entry", "https://", "重复"] {
            assert!(msg.contains(probe), "消息要点名 {probe:?}，实得 {msg}");
        }
        assert!(msg.contains("整份不注册"), "裁决口径要自证: {msg}");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-6）落地补记⑤：预设 8 行数据文件
    fn presets_builtin_coversFourProtocolsWithEightRows() {
        let store = PresetStore::builtin().expect("内置预设必须过同一校验口");
        let all = store.list();
        assert_eq!(
            all.len(),
            8,
            "内置 8 条：WebDAV×3 + SFTP×2 + HTTPS×2 + FTP×1"
        );
        let count = |proto: RemoteProtocol| {
            all.iter()
                .filter(|p| p.protocol == proto)
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(count(RemoteProtocol::WebDav).len(), 3);
        assert_eq!(count(RemoteProtocol::Sftp).len(), 2);
        assert_eq!(count(RemoteProtocol::Https).len(), 2);
        assert_eq!(count(RemoteProtocol::Ftp), vec!["ftp-anonymous"]);
        // 明文档诚实形状：FTP 预设只能是 anonymous（存管/会话位根本不在枚举里）
        let ftp = store.get("ftp-anonymous").unwrap();
        assert_eq!(ftp.auth_kind, Anonymous);
        assert_eq!(ftp.port, 21);
        // 每条都有可复制的默认落点（host/port/base 三者非空即能填出档案草稿）
        for p in &all {
            assert!(!p.default_host.is_empty() && p.port > 0 && !p.base_path.is_empty());
        }
    }

    #[test]
    #[allow(non_snake_case)] // 非任务书测名沿用本模块测名家族风格
    fn presetStore_userDirFailClosedAndIdConflict() {
        let dir = std::env::temp_dir().join(format!("nf_preset_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 好文件先在册
        let good = serde_json::json!({ "presets": [entry("lab-nas")] });
        std::fs::write(dir.join("lab.json"), serde_json::to_string(&good).unwrap()).unwrap();
        let store = PresetStore::load(&dir).unwrap();
        assert_eq!(store.list().len(), 9, "内置 8 + 用户 1");
        // 坏文件进目录 ⇒ 加载整体 Err（禁静默丢文件）
        let bad = serde_json::json!({ "presets": [ { "id": "oops", "label": "x" } ] });
        std::fs::write(dir.join("oops.json"), serde_json::to_string(&bad).unwrap()).unwrap();
        let e = PresetStore::load(&dir).unwrap_err();
        assert!(
            e.to_string().contains("oops.json"),
            "fail-closed 消息要点名坏文件，实得 {e}"
        );
        let _ = std::fs::remove_file(dir.join("oops.json"));
        // 覆写内置 id 同样整份拒
        let hijack = serde_json::json!({ "presets": [{
            "id": "ftp-anonymous", "label": "冒牌", "protocol": "ftp",
            "default_host": "evil.example", "port": 21, "auth_kind": "anonymous"
        }]});
        std::fs::write(
            dir.join("lab.json"),
            serde_json::to_string(&hijack).unwrap(),
        )
        .unwrap();
        let e = PresetStore::load(&dir).unwrap_err();
        assert!(e.to_string().contains("冲突"), "内置 id 不可覆写: {e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
