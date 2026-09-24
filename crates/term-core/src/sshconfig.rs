//! T-B7-3（09 §7.2）OpenSSH `config` **只读导入**：解析用户手写的
//! `~/.ssh/config` 为连接表单预填候选——本模块结构性只读：全文不出现
//! 任何 IO 创建口（机检判据见任务书 grep 断言，本文件即其对象），
//! 永不回送是设计事实而非约定承诺。
//!
//! 语义按 OpenSSH 现行：**首值优先**（同 Host 块重复键取首枚、跨块同名
//! 别名合并同样首值优先）；`Host *`（含通配模式）不入列并点名；
//! 不支持键逐个点名进 ignored 清单；`~` 原样保留（展开留给连接口，
//! 解析器不做路径魔法）。

use serde::Serialize;
use std::path::{Path, PathBuf};

/// 一条 Host 块的导入结果（键缺位 = `None`，连接表单只覆盖有值字段）
#[derive(Debug, Clone, Serialize)]
pub struct SshHostEntry {
    pub alias: String,
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
}

/// 去首尾空白 + 剥引号（`HostName "my host"` 容错形制）
fn unquote(v: &str) -> String {
    let t = v.trim();
    t.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| t.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(t)
        .to_owned()
}

/// `Key Value` 与 `Key=Value` 与 `Key = value` 三式拆分
/// （键大小写不敏感）
fn split_directive(line: &str) -> Option<(String, String)> {
    let (key, rest) = line.split_once([' ', '\t', '='])?;
    let value = rest
        .trim_start()
        .strip_prefix('=')
        .unwrap_or(rest.trim_start());
    Some((key.trim().to_ascii_lowercase(), unquote(value)))
}

/// 追加/找到别名条目（跨 Host 块同名别名合并——OpenSSH 首值优先语义的
/// 载体；通配块调用方不会流到此口）
fn entry_for<'a>(entries: &'a mut Vec<SshHostEntry>, alias: &str) -> &'a mut SshHostEntry {
    if let Some(pos) = entries.iter().position(|e| e.alias == alias) {
        return &mut entries[pos];
    }
    entries.push(SshHostEntry {
        alias: alias.to_owned(),
        host_name: None,
        user: None,
        port: None,
        identity_file: None,
    });
    entries.last_mut().unwrap()
}

/// 首值优先落键：已有值再遇同名键 → 忽略并进 ignored 点名
fn first_wins_str(
    slot: &mut Option<String>,
    value: &str,
    alias: &str,
    key: &str,
    ignored: &mut Vec<String>,
) {
    if slot.is_some() {
        ignored.push(format!("{alias} 的重复 {key}（{value}）被忽略：首值优先"));
    } else {
        *slot = Some(value.to_owned());
    }
}

/// 解析 config 全文：`(可导入条目, ignored 点名清单)`。
/// 通配/不支持键**只跳该块该键，绝不整份拒载**——配置文件是人手维护的
/// 外部资产，不是本协议驱动表（与 B6 presets 整份拒载的裁决分歧在册：
/// 那边坏表=行为不可信，这边局部忽略=其余别名照常可用，拒载才是伤害）。
pub fn parse_ssh_config(text: &str) -> (Vec<SshHostEntry>, Vec<String>) {
    let mut entries: Vec<SshHostEntry> = Vec::new();
    let mut ignored: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new(); // 当前 Host 块的别名（可多枚）

    for raw in text.lines() {
        let line = raw.trim_end();
        let stripped = match line.split_once(" #") {
            Some((before, _)) => before.trim_end(),
            None => line,
        };
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        let Some((key, value)) = split_directive(stripped.trim()) else {
            ignored.push(format!("无法拆分的行已跳过: {stripped}"));
            continue;
        };
        if key == "host" {
            current = Vec::new();
            if value.trim().is_empty() {
                ignored.push(format!("空的 Host 行已跳过: {stripped}"));
                continue;
            }
            let patterns: Vec<&str> = value.split_whitespace().collect();
            if patterns.iter().any(|p| p.contains(['*', '?'])) {
                // 通配模式整行不入列（其内键值因 current 为空落"块外散键"臂点名）
                ignored.push(format!(
                    "Host 行含通配模式（{value}）：匹配面大于导入意图，该块不采（其余条目照常）"
                ));
            } else {
                for p in patterns {
                    entry_for(&mut entries, p);
                    current.push(p.to_owned());
                }
            }
            continue;
        }
        if current.is_empty() {
            ignored.push(format!("Host 块之外的散键已跳过: {stripped}"));
            continue;
        }
        match key.as_str() {
            "hostname" => {
                for alias in &current {
                    let e = entry_for(&mut entries, alias);
                    first_wins_str(&mut e.host_name, &value, alias, "HostName", &mut ignored);
                }
            }
            "user" => {
                for alias in &current {
                    let e = entry_for(&mut entries, alias);
                    first_wins_str(&mut e.user, &value, alias, "User", &mut ignored);
                }
            }
            "identityfile" => {
                for alias in &current {
                    let e = entry_for(&mut entries, alias);
                    // `~` 原样保留——展开是连接口的职责，且必须点名来源
                    first_wins_str(
                        &mut e.identity_file,
                        &value,
                        alias,
                        "IdentityFile",
                        &mut ignored,
                    );
                }
            }
            "port" => {
                for alias in &current {
                    let e = entry_for(&mut entries, alias);
                    match value.parse::<u16>() {
                        Ok(p) => {
                            if e.port.is_some() {
                                ignored.push(format!(
                                    "{alias} 的重复 Port（{value}）被忽略：首值优先"
                                ));
                            } else {
                                e.port = Some(p);
                            }
                        }
                        Err(_) => {
                            ignored.push(format!("{alias} 的 Port 值不可解析（{value}）：该键忽略"))
                        }
                    }
                }
            }
            other => {
                ignored.push(format!(
                    "{} 的不支持键不导入（{other}，值不进预填）",
                    current.join("/")
                ));
            }
        }
    }
    (entries, ignored)
}

/// 默认 config 路径（HOME 未设则 `None`——"读不到"与"没有文件"同为空表非错误）
pub fn default_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    Some(home.join(".ssh").join("config"))
}

/// 按路径装载：文件缺席 = 空表**非错误**（缺文件是常态不是事故）
pub fn load_config_at(path: &Path) -> (Vec<SshHostEntry>, Vec<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_ssh_config(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), Vec::new()),
        Err(e) => {
            tracing::warn!(
                "[sshconfig] 读取失败（{}）: {e}——按空表处理",
                path.display()
            );
            (Vec::new(), Vec::new())
        }
    }
}

/// 装载用户默认 config（`$HOME/.ssh/config` 或 `%USERPROFILE%\.ssh\config`）
pub fn load_default_config() -> (Vec<SshHostEntry>, Vec<String>) {
    match default_config_path() {
        Some(p) => load_config_at(&p),
        None => (Vec::new(), Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-3）字面测试名优先于 rustc 命名惯例
    fn parser_firstValueWins() {
        let text = "\
Host dup
  HostName first.example
  HostName second.example
  User alice
  Port 2200
  Port 3300

Host dup
  HostName other-block.example
  User bob
";
        let (entries, ignored) = parse_ssh_config(text);
        assert_eq!(entries.len(), 1, "同名别名跨块合并");
        let e = &entries[0];
        assert_eq!(
            e.host_name.as_deref(),
            Some("first.example"),
            "重复 HostName 取首"
        );
        assert_eq!(e.user.as_deref(), Some("alice"), "跨块 User 也是首值优先");
        assert_eq!(e.port, Some(2200));
        assert_eq!(
            ignored.iter().filter(|w| w.contains("首值优先")).count(),
            4,
            "四处重复键都要点名（块内 HostName/Port + 跨块 HostName/User），实得 {ignored:?}"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-3）字面测试名优先于 rustc 命名惯例
    fn parser_wildcardSkipped_notWholeFileRejected() {
        // 正对照钉：通配块只跳自己，其余合法条目照常在场——
        // 与 B6 presets 整份拒载相反（config 是外部人写资产，拒载=伤害）
        let text = "\
Host *
  User nobody
  ServerAliveInterval 30

Host good
  HostName g.example
  User carol
";
        let (entries, ignored) = parse_ssh_config(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].alias, "good");
        assert_eq!(entries[0].host_name.as_deref(), Some("g.example"));
        assert_eq!(
            entries[0].user.as_deref(),
            Some("carol"),
            "非通配块不受牵连"
        );
        assert!(
            ignored.iter().any(|w| w.contains("通配")),
            "通配块必须点名，实得 {ignored:?}"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-3）字面测试名优先于 rustc 命名惯例
    fn parser_identityFileTilde_notExpanded() {
        let text = "\
Host tilde
  IdentityFile ~/.ssh/id_ed25519
  User dave
";
        let (entries, _) = parse_ssh_config(text);
        assert_eq!(
            entries[0].identity_file.as_deref(),
            Some("~/.ssh/id_ed25519"),
            "`~` 原样保留——展开留给连接口并点名"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-3）字面测试名优先于 rustc 命名惯例
    fn configHosts_missingFile_isEmptyNotError() {
        let missing = std::env::temp_dir().join("nf_sshconfig_absent_dir/config");
        assert!(
            !missing.exists(),
            "夹具前提：路径必须缺席（并发清 temp 时重跑即红不假绿）"
        );
        let (entries, ignored) = load_config_at(&missing);
        assert!(
            entries.is_empty() && ignored.is_empty(),
            "缺文件 = 空表非错误"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-3）字面测试名优先于 rustc 命名惯例
    fn parser_keyEqualsAndQuotes_forms() {
        let text = "\
Host q
Hostname=\"\\\"quoted host\\\"\"
User = eve
IdentityFile 'C:\\Users\\me\\.ssh\\id_rsa'
  # 纯注释行
Port = notanumber
ProxyJump jumpbox
";
        let (entries, ignored) = parse_ssh_config(text);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        // 引号剥离与 `=` 形制各归其位
        assert_eq!(e.user.as_deref(), Some("eve"));
        assert_eq!(
            e.identity_file.as_deref(),
            Some("C:\\Users\\me\\.ssh\\id_rsa")
        );
        assert!(ignored.iter().any(|w| w.contains("Port 值不可解析")));
        // 不支持键点名用拆小后的键名（内部口径统一小写）
        assert!(
            ignored.iter().any(|w| w.contains("proxyjump")),
            "{ignored:?}"
        );
        // Hostname 值整体带转义引号的畸形形制：原样进首值（不做二次魔法）
        assert!(e.host_name.is_some());
    }
}
