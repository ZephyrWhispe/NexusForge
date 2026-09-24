//! 远端文件名字符映射（T-B7-26，09 §6.3 档 f）：把"名字到了对端协议就传败"
//! 的裁决从**执行时静默失败**前移到**入队时显式三档**（Ask 预览 / AutoRename
//! 带建议入队 / Reject 点名拒）。红线=静默改名与传败同罪：任何一臂要么把
//! 原名→新名的裁决结果如实交出去，要么 Err 点名冲突字符——没有第三种。
//!
//! 逐协议非法字符表只登记**有事实源**的字符（承重：禁把非 ASCII 整批当非法，
//! 那是最常见假阳性）；FTP 服务端惯例集与全角映射表是**数据文件**
//! （`<app_data>/file/namefix/ftp_banned.json` / `char_map.json`，存在即整段
//! 替换内置表）——坏文件 fail-closed：整段拒载并挂账，远程臂的入队/执行
//! 一律 Err 点名该文件（镜像 process_guard 裁决族：映射坏=改名可能毁文件，
//! 比历史数据严重一档，静默回落内置表=拿旧表冒充用户的表）。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::profile::RemoteProtocol;

/// 远端名裁决三档（ConfigStore file 段 `remote_name_fix` 的取值域，缺省 Ask）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixPolicy {
    /// 入队前返回预览（原名/建议名/冲突字符），用户确认后才带 AutoRename 重投
    #[default]
    Ask,
    /// 探测即改：建议名入队（`OpSpec::name_overrides`），回执如实复述原名→新名
    AutoRename,
    /// 有非法字符即 Err 点名，不入队
    Reject,
}

/// 单枚冲突字符及其归因（reason 逐字点名协议侧事实源，禁共文案）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameIssue {
    pub char: String,
    pub reason: String,
}

/// 入队前置闸对**一个名字**的裁决回执（Ask 预览行 = AutoRename 复述行，
/// 两臂共用同一形状：原名、冲突字符、建议名〔无干净建议则 None〕）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct NameFixItem {
    pub name: String,
    pub bad: Vec<NameIssue>,
    pub suggested: Option<String>,
}

#[derive(Deserialize)]
struct BanEntry {
    char: String,
    reason: String,
}

#[derive(Deserialize)]
struct MapEntry {
    from: String,
    to: String,
}

/// 两枚数据文件装载后的表段（FTP 惯例禁集 + 全角映射表）。
/// 结构不导出字段读权限——消费全走 [`NameFixTables::ftp_banned_for`] /
/// [`NameFixTables::map_of`]，形状漂移在装载口即拒。
#[derive(Clone, Debug, Default)]
pub struct NameFixTables {
    ftp_banned: Vec<NameIssue>,
    char_map: BTreeMap<char, char>,
}

fn ch(s: &str) -> Option<char> {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

impl NameFixTables {
    /// 内置缺省表：FTP 惯例禁集**空**（未实测登记=无事实源，不进表——
    /// 协议控制字符 \r\n\0 走代码内置臂不在此列）；全角映射表覆盖各协议
    /// 内置禁集中可全角化的 ASCII 形。
    pub fn embedded() -> Self {
        let pairs: [(char, char); 9] = [
            ('?', '？'),
            ('*', '＊'),
            (':', '：'),
            ('<', '＜'),
            ('>', '＞'),
            ('"', '＂'),
            ('|', '｜'),
            ('\\', '＼'),
            ('/', '／'),
        ];
        Self {
            ftp_banned: vec![],
            char_map: pairs.into_iter().collect(),
        }
    }

    /// 从 `<app_data>/file/namefix/` 装载覆盖表（文件缺席=用内置段；存在即
    /// **整段替换**该段）。坏文件 ⇒ Err 点名文件路径（fail-closed，拒整段，
    /// 绝不静默回落内置——调用侧把 Err 挂账成毒态，远程臂带毒必拒）。
    pub fn load_from(dir: &Path) -> Result<Self, String> {
        let base = NameFixTables::embedded();
        let (ftp_banned, char_map) = (
            read_segment(&dir.join("ftp_banned.json"), |raw| {
                serde_json::from_slice::<Vec<BanEntry>>(raw)
                    .map_err(|e| e.to_string())
                    .and_then(|v| {
                        v.into_iter()
                            .map(|e| {
                                if ch(&e.char).is_none() {
                                    return Err("char 须为一字符".to_owned());
                                }
                                Ok(NameIssue {
                                    char: e.char,
                                    reason: e.reason,
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
            })?,
            read_segment(&dir.join("char_map.json"), |raw| {
                serde_json::from_slice::<Vec<MapEntry>>(raw)
                    .map_err(|e| e.to_string())
                    .and_then(|v| {
                        let mut m = BTreeMap::new();
                        for e in v {
                            let (Some(from), Some(to)) = (ch(&e.from), ch(&e.to)) else {
                                return Err("from/to 须各为一字符".to_owned());
                            };
                            m.insert(from, to);
                        }
                        Ok(m)
                    })
            })?,
        );
        Ok(Self {
            ftp_banned: ftp_banned.unwrap_or(base.ftp_banned),
            char_map: char_map.unwrap_or(base.char_map),
        })
    }

    pub fn map_of(&self, from: char) -> Option<char> {
        self.char_map.get(&from).copied()
    }

    /// 协议内置禁集（有事实源的字符，逐个带归因）+ FTP 数据文件段拼接。
    /// SFTP：名即单个路径段，分隔符与 NUL 之外没有协议级禁区；
    /// WebDAV：RFC3986 段规则（gen-delims 残部 + RFC3986 §2 排除字符）加 CRLF；
    /// HTTPS：本域只有下载腿，约束同端点路径段规则；
    /// FTP：协议控制字符（命令行参数无引号转义语义）+ 实测登记的惯例禁集。
    pub fn forbidden(&self, protocol: RemoteProtocol) -> Vec<NameIssue> {
        let mk = |c: char, reason: &str| NameIssue {
            char: c.to_string(),
            reason: reason.to_owned(),
        };
        match protocol {
            RemoteProtocol::Sftp => vec![
                mk('/', "SFTP 路径分隔符：文件名字段里它就是两段"),
                mk('\0', "NUL：路径串的终止符，协议帧里没有它的容身之地"),
            ],
            RemoteProtocol::WebDav => uri_segment_bans(),
            RemoteProtocol::Https => {
                let mut v = uri_segment_bans();
                v.push(NameIssue {
                    char: "\0".into(),
                    reason: "HTTPS 端点路径同样不容 NUL（该档案只有下载腿，写名臂本就无通路）"
                        .into(),
                });
                v
            }
            RemoteProtocol::Ftp => {
                let mut v = vec![
                    mk('\r', "FTP 命令以 CRLF 终止：名字里的回车就是提前交卷"),
                    mk('\n', "FTP 命令以 CRLF 终止：名字里的换行就是注入第二条命令"),
                    mk('\0', "NUL：命令行的终止符"),
                ];
                v.extend(self.ftp_banned.iter().cloned());
                v
            }
        }
    }
}

fn uri_segment_bans() -> Vec<NameIssue> {
    let mk = |c: char, reason: &'static str| NameIssue {
        char: c.to_string(),
        reason: reason.to_owned(),
    };
    vec![
        mk('/', "RFC3986：'/' 是段分隔符，段名里不得有"),
        mk('?', "RFC3986：'?' 终结路径段开启查询"),
        mk('#', "RFC3986：'#' 终结路径开启片段"),
        mk('[', "RFC3986：方括号是保留定界符"),
        mk(']', "RFC3986：方括号是保留定界符"),
        mk('<', "RFC3986 §2 排除字符"),
        mk('>', "RFC3986 §2 排除字符"),
        mk('"', "RFC3986 §2 排除字符"),
        mk('{', "RFC3986 §2 排除字符"),
        mk('}', "RFC3986 §2 排除字符"),
        mk('|', "RFC3986 §2 排除字符"),
        mk('\\', "RFC3986 §2 排除字符（反斜杠在 URL 里从来不是分隔符）"),
        mk('^', "RFC3986 §2 排除字符"),
        mk('`', "RFC3986 §2 排除字符"),
        NameIssue {
            char: "\r".into(),
            reason: "CRLF：HTTP 请求行的终止符".into(),
        },
        NameIssue {
            char: "\n".into(),
            reason: "CRLF：HTTP 请求行的终止符".into(),
        },
        NameIssue {
            char: "\0".into(),
            reason: "NUL：任何路径串都不容它".into(),
        },
    ]
}

/// 读一段可选覆盖文件：Ok(None)=文件缺席用内置；坏文件 Err 点名路径+原因。
fn read_segment<T, E: std::fmt::Display>(
    path: &Path,
    parse: impl Fn(&[u8]) -> Result<T, E>,
) -> Result<Option<T>, String> {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{} 读取失败：{e}", path.display())),
        Ok(raw) => parse(&raw)
            .map(Some)
            .map_err(|why| format!("{} 解析失败（整段拒载，不回落内置）：{why}", path.display())),
    }
}

/// 探测一个远端文件名的冲突字符（按表逐协议裁决）。**只认码点不认宽度**：
/// 中文、emoji、全角标点统统合法（最常见假阳性=把非 ASCII 整批当非法）。
pub fn probe_remote_name(
    name: &str,
    protocol: RemoteProtocol,
    tables: &NameFixTables,
) -> Vec<NameIssue> {
    let banned = tables.forbidden(protocol);
    let mut out: Vec<NameIssue> = vec![];
    for c in name.chars() {
        if let Some(hit) = banned.iter().find(|b| b.char.starts_with(c)) {
            if !out.iter().any(|o| o.char == hit.char) {
                out.push(clone_issue(hit));
            }
        }
    }
    out
}

fn clone_issue(i: &NameIssue) -> NameIssue {
    NameIssue {
        char: i.char.clone(),
        reason: i.reason.clone(),
    }
}

/// 建议改名：逐字符过全角映射表，产出的新名必须**自己先过探测**（映射表是
/// 用户数据，'a'->'?' 这种坏映射在这里也会被拦下）；有任何一枚非法字符
/// 映射不动 ⇒ None（无干净建议=交回用户裁决，禁半截改名）。
pub fn suggest_rename(
    name: &str,
    protocol: RemoteProtocol,
    tables: &NameFixTables,
) -> Option<String> {
    let issues = probe_remote_name(name, protocol, tables);
    if issues.is_empty() {
        return None;
    }
    let mut mapped = String::with_capacity(name.len());
    for c in name.chars() {
        if issues.iter().any(|i| i.char.starts_with(c)) {
            let Some(m) = tables.map_of(c) else {
                return None; // 有字符映射不动 ⇒ 整体无建议（禁半截改名）
            };
            mapped.push(m);
        } else {
            mapped.push(c);
        }
    }
    if probe_remote_name(&mapped, protocol, tables).is_empty() {
        Some(mapped)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nf_namefix_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B7-26）字面测试名优先于 rustc 命名惯例
    fn probeSftp_onlyNulAndSlash_flagged() {
        let t = NameFixTables::embedded();
        let hits = probe_remote_name("报告/子名\0尾", RemoteProtocol::Sftp, &t);
        assert_eq!(hits.len(), 2, "斜杠与 NUL 各一枚，实得 {hits:?}");
        assert!(hits.iter().any(|h| h.char == "/") && hits.iter().any(|h| h.char == "\u{0}"));
        // 正对照（禁假阳性红线）：中文/emoji/全角/空格/百分号统统合法
        for ok in [
            "报告 v2（终稿）.docx",
            "a?b.txt",
            "🎉派对.jpg",
            "50%折扣.txt",
            "",
        ] {
            assert!(
                probe_remote_name(ok, RemoteProtocol::Sftp, &t).is_empty(),
                "SFTP 臂把 {ok:?} 误判为非法"
            );
        }
        // WebDAV 臂的边界：'?' 非法而 '？' 合法；FTP 臂：CRLF 非法、'*' 未登记即合法
        assert!(!probe_remote_name("a?b", RemoteProtocol::WebDav, &t).is_empty());
        assert!(probe_remote_name("a？b", RemoteProtocol::WebDav, &t).is_empty());
        assert!(
            probe_remote_name("a*b", RemoteProtocol::Ftp, &t).is_empty(),
            "无事实源不进表"
        );
        assert!(!probe_remote_name("a\rb", RemoteProtocol::Ftp, &t).is_empty());
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B7-26）字面测试名优先于 rustc 命名惯例
    fn suggestRename_fullwidthMap_dataFileDriven() {
        let d = tmp("map");
        // 改夹具表即改行为（数据文件证明，零重编译语义）：
        // 缺省表下 '?'→'？'（WebDAV 有建议），SFTP 无斜杠名不触发
        let t = NameFixTables::load_from(&d).unwrap();
        assert_eq!(
            suggest_rename("a?b.txt", RemoteProtocol::WebDav, &t).as_deref(),
            Some("a？b.txt")
        );
        assert_eq!(
            suggest_rename("clean.txt", RemoteProtocol::WebDav, &t),
            None,
            "无名可改=无建议"
        );
        // 整段替换：内置 '?'→'？' 被用户的 '?'→'﹖' 顶掉（改夹具表即改行为）；
        // 未被用户表登记的映射（'*'）不再可改 ⇒ None，禁半截改名
        std::fs::write(d.join("char_map.json"), r#"[{"from":"?","to":"﹖"}]"#).unwrap();
        let t2 = NameFixTables::load_from(&d).unwrap();
        assert_eq!(
            suggest_rename("a?b", RemoteProtocol::WebDav, &t2).as_deref(),
            Some("a﹖b")
        );
        assert_eq!(suggest_rename("a*b", RemoteProtocol::WebDav, &t2), None);
        assert_eq!(
            suggest_rename("a:b", RemoteProtocol::WebDav, &t2),
            None,
            "':' 无事实源=合法，无冲突可改"
        );
        // 自毁映射拦下：'?'->'?' 让建议再过探测仍带非法字符 ⇒ 无建议
        std::fs::write(d.join("char_map.json"), r#"[{"from":"?","to":"?"}]"#).unwrap();
        let t3 = NameFixTables::load_from(&d).unwrap();
        assert_eq!(suggest_rename("a?b.txt", RemoteProtocol::WebDav, &t3), None);
        // ftp_banned 段同样数据驱动：登记 '*' 后 FTP 臂才禁它
        std::fs::write(
            d.join("ftp_banned.json"),
            r#"[{"char":"*","reason":"该服务端实测拒"}]"#,
        )
        .unwrap();
        std::fs::write(d.join("char_map.json"), r#"[]"#).unwrap();
        let t4 = NameFixTables::load_from(&d).unwrap();
        assert!(!probe_remote_name("a*b", RemoteProtocol::Ftp, &t4).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B7-26）字面测试名优先于 rustc 命名惯例
    fn badCharMapFile_wholeSegmentRejected() {
        // fail-closed：坏 JSON / 坏形状 ⇒ Err 点名文件，**不回落内置表**
        // （静默用旧表冒充用户的表=毒表改名毁文件，与传败同罪）
        let d = tmp("bad");
        std::fs::write(d.join("char_map.json"), b"{ not json").unwrap();
        let e = NameFixTables::load_from(&d).unwrap_err();
        assert!(e.contains("char_map.json"), "须点名坏文件，实得 {e}");
        assert!(e.contains("整段拒载"), "裁决话术钉死，实得 {e}");
        // 多字符映射（from 是两字串）= 形状坏，同臂拒
        std::fs::write(d.join("char_map.json"), r#"[{"from":"ab","to":"？"}]"#).unwrap();
        assert!(NameFixTables::load_from(&d)
            .unwrap_err()
            .contains("char_map.json"));
        let _ = std::fs::remove_dir_all(&d);
        // 服务侧消费点随批：毒态挂账见 service.rs `nameFixPoison_blocksRemoteEnqueue`
    }
}
