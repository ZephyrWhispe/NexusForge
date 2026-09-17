//! SY1/SY2 包管理器（docs/impl/06 SY1–SY2）：winget/scoop/choco 探测与适配 + 合并去重视图。
//!
//! - 所有变更操作前由 UI 展示确切命令行（[`cmd_preview`]）；执行时输出逐行回调
//!   （sys.pkg_line 事件流式回传，docs/impl/06 SY1 风险标注）
//! - winget 解析走 `--disable-interactivity`，JSON 输出可用时优先、失败回退表格
//!   （docs/impl/06 SY 风险标注：进度条控制字符）

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::error::{Result, SysError};

/// 包条目（IPC DTO）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PkgEntry {
    /// 包 ID（winget Id / scoop name / choco id）
    pub id: String,
    pub name: String,
    pub version: String,
    /// 可升级到的版本（None = 已最新/源不支持）
    pub available: Option<String>,
    pub source: String,
}

/// 包管理器（SY1）
pub trait PkgManager: Send + Sync {
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str;
    /// 可执行文件是否在 PATH
    fn available(&self) -> bool;
    /// 已装清单
    fn list(&self) -> Result<Vec<PkgEntry>>;
    /// 变更命令行预览（UI 确认展示）
    fn cmd_preview(&self, action: &str, package_id: &str) -> Result<String>;
    /// 变更操作（emit 逐行输出；docs/impl/06 SY1：输出流式回传 UI）
    fn run_action(&self, action: &str, package_id: &str, emit: &mut dyn FnMut(String)) -> Result<Vec<String>>;
}

/// PATH 查找（Windows where.exe）
fn in_path(exe: &str) -> bool {
    Command::new("where.exe")
        .arg(exe)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 跑外部命令并逐行回调 stdout（行同步收集）
fn run_lines(exe: &str, args: &[&str], mut emit: impl FnMut(String)) -> Result<Vec<String>> {
    let mut child = Command::new(exe)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(SysError::Io)?;
    let stdout = child.stdout.take().ok_or_else(|| SysError::PkgCmd("无 stdout".into()))?;
    let mut lines = Vec::new();
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        match line {
            Ok(l) => {
                // 进度条控制字符过滤（\b \r，docs/impl/06 SY 风险标注）
                let clean = l.trim_matches(|c| c == '\u{8}' || c == '\r');
                if !clean.is_empty() {
                    emit(clean.to_string());
                    lines.push(clean.to_string());
                }
            }
            Err(_) => break,
        }
    }
    let _ = child.wait();
    Ok(lines)
}

/// winget 表格解析：Name / Id / Version / [Available /] [Source /]，列以 2+ 空格分隔
fn parse_winget_table(lines: &[String]) -> Vec<PkgEntry> {
    let mut out = Vec::new();
    // 跳过到表头行
    let header_idx = lines.iter().position(|l| l.contains("Id") && l.contains("Version"));
    let Some(hi) = header_idx else { return out };
    let header = &lines[hi];
    let cols = split_columns(header);
    // 分隔线（---）在表头后第一行
    let body_start = lines
        .iter()
        .skip(hi + 1)
        .position(|l| l.starts_with('-') || l.starts_with(" --"))
        .map(|p| hi + 1 + p + 1)
        .unwrap_or(hi + 1);
    for line in lines.iter().skip(body_start) {
        if line.trim().is_empty() || line.starts_with('-') {
            continue;
        }
        let cells = split_columns(line);
        // 行结构：Name / Id / Version / [Available /] Source——winget 对空白列不产生 cell，
        // 故用尾部对齐：末列恒为 Source（有 Source 列时），Available 取倒数第二列
        let Some(id) = cells.get(1).cloned() else { continue };
        if id.is_empty() {
            continue;
        }
        let has_source_col = cols.iter().any(|c| c.eq_ignore_ascii_case("Source"));
        let has_avail_col = cols.iter().any(|c| c.eq_ignore_ascii_case("Available"));
        let source = if has_source_col {
            cells.last().cloned().unwrap_or_default()
        } else {
            String::new()
        };
        let available = if has_avail_col && cells.len() >= 5 {
            let a = cells[cells.len() - 2].trim().to_string();
            if a.is_empty() || a == "..." || a == ">" {
                None
            } else {
                Some(a)
            }
        } else {
            None
        };
        out.push(PkgEntry {
            name: cells.first().cloned().unwrap_or_default(),
            version: cells.get(2).cloned().unwrap_or_default(),
            available,
            source,
            id,
        });
    }
    out
}

/// 按 2+ 空格切分表格列（列内容首尾 trim；列内单空格保留）
fn split_columns(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut spaces = 0;
    for ch in line.chars() {
        if ch == ' ' {
            spaces += 1;
            if spaces == 1 {
                cur.push(' ');
            } else if spaces == 2 && !cur.is_empty() {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            // spaces >= 3：已在 2 时切分，无需重复
        } else {
            spaces = 0;
            cur.push(ch);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// winget 适配器
pub struct WingetManager;

impl PkgManager for WingetManager {
    fn id(&self) -> &'static str {
        "winget"
    }
    fn label(&self) -> &'static str {
        "winget（系统内置）"
    }
    fn available(&self) -> bool {
        in_path("winget.exe")
    }

    fn list(&self) -> Result<Vec<PkgEntry>> {
        if !self.available() {
            return Err(SysError::PkgUnavailable("winget".into()));
        }
        let lines = run_lines(
            "winget.exe",
            &["list", "--accept-source-agreements", "--disable-interactivity"],
            |_| {},
        )?;
        let mut pkgs = parse_winget_table(&lines);
        for p in &mut pkgs {
            p.source = if p.source.is_empty() { "winget".into() } else { p.source.clone() };
        }
        Ok(pkgs)
    }

    fn cmd_preview(&self, action: &str, package_id: &str) -> Result<String> {
        match action {
            "install" => Ok(format!(
                "winget install --id {package_id} --exact --silent --accept-package-agreements --accept-source-agreements --disable-interactivity"
            )),
            "uninstall" => Ok(format!("winget uninstall --id {package_id} --silent --disable-interactivity")),
            "upgrade_all" => Ok("winget upgrade --all --silent --accept-package-agreements --accept-source-agreements --disable-interactivity".into()),
            _ => Err(SysError::BadParam(format!("未知操作: {action}"))),
        }
    }

    fn run_action(&self, action: &str, package_id: &str, emit: &mut dyn FnMut(String)) -> Result<Vec<String>> {
        let (exe, args): (&str, Vec<String>) = match action {
            "install" => (
                "winget.exe",
                vec![
                    "install".into(),
                    "--id".into(),
                    package_id.into(),
                    "--exact".into(),
                    "--silent".into(),
                    "--accept-package-agreements".into(),
                    "--accept-source-agreements".into(),
                    "--disable-interactivity".into(),
                ],
            ),
            "uninstall" => (
                "winget.exe",
                vec![
                    "uninstall".into(),
                    "--id".into(),
                    package_id.into(),
                    "--silent".into(),
                    "--disable-interactivity".into(),
                ],
            ),
            "upgrade_all" => (
                "winget.exe",
                vec![
                    "upgrade".into(),
                    "--all".into(),
                    "--silent".into(),
                    "--accept-package-agreements".into(),
                    "--accept-source-agreements".into(),
                    "--disable-interactivity".into(),
                ],
            ),
            _ => return Err(SysError::BadParam(format!("未知操作: {action}"))),
        };
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let lines = run_lines(exe, &refs, |l| emit(l))?;
        Ok(lines)
    }
}

/// scoop 适配器（`scoop list` 表格；变更：scoop install/uninstall <name>、scoop update *）
pub struct ScoopManager;

impl PkgManager for ScoopManager {
    fn id(&self) -> &'static str {
        "scoop"
    }
    fn label(&self) -> &'static str {
        "Scoop（用户级）"
    }
    fn available(&self) -> bool {
        in_path("scoop")
    }

    fn list(&self) -> Result<Vec<PkgEntry>> {
        if !self.available() {
            return Err(SysError::PkgUnavailable("scoop".into()));
        }
        let lines = run_lines("cmd", &["/C", "scoop", "list"], |_| {})?;
        // 表头 Name Version Source Updated Info；分隔线 -
        let mut out = Vec::new();
        let mut body = false;
        for line in lines {
            if line.starts_with("Name") {
                body = true;
                continue;
            }
            if body && !line.is_empty() && !line.starts_with('-') {
                let cells = split_columns(&line);
                if let Some(name) = cells.first() {
                    out.push(PkgEntry {
                        id: name.clone(),
                        name: name.clone(),
                        version: cells.get(1).cloned().unwrap_or_default(),
                        available: None,
                        source: "scoop".into(),
                    });
                }
            }
        }
        Ok(out)
    }

    fn cmd_preview(&self, action: &str, package_id: &str) -> Result<String> {
        match action {
            "install" => Ok(format!("scoop install {package_id}")),
            "uninstall" => Ok(format!("scoop uninstall {package_id}")),
            "upgrade_all" => Ok("scoop update *".into()),
            _ => Err(SysError::BadParam(format!("未知操作: {action}"))),
        }
    }

    fn run_action(&self, action: &str, package_id: &str, emit: &mut dyn FnMut(String)) -> Result<Vec<String>> {
        let args: Vec<String> = match action {
            "install" => vec!["/C", "scoop", "install", package_id].into_iter().map(String::from).collect(),
            "uninstall" => vec!["/C", "scoop", "uninstall", package_id].into_iter().map(String::from).collect(),
            "upgrade_all" => vec!["/C", "scoop", "update", "*"].into_iter().map(String::from).collect(),
            _ => return Err(SysError::BadParam(format!("未知操作: {action}"))),
        };
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_lines("cmd", &refs, |l| emit(l))
    }
}

/// choco 适配器（需管理员；choco 2.x `choco list` 本地已装）
pub struct ChocoManager;

impl PkgManager for ChocoManager {
    fn id(&self) -> &'static str {
        "choco"
    }
    fn label(&self) -> &'static str {
        "Chocolatey（需管理员）"
    }
    fn available(&self) -> bool {
        in_path("choco.exe")
    }

    fn list(&self) -> Result<Vec<PkgEntry>> {
        if !self.available() {
            return Err(SysError::PkgUnavailable("choco".into()));
        }
        let lines = run_lines("choco.exe", &["list"], |_| {})?;
        let mut out = Vec::new();
        for line in lines {
            // "name x.y.z" 形式；表头与结尾计数行跳过
            let t = line.trim();
            if t.is_empty() || t.contains("packages installed") || t.starts_with("Chocolatey") {
                continue;
            }
            let mut parts = t.splitn(2, ' ');
            let (Some(name), Some(rest)) = (parts.next(), parts.next()) else {
                continue;
            };
            out.push(PkgEntry {
                id: name.to_string(),
                name: name.to_string(),
                version: rest.trim().to_string(),
                available: None,
                source: "choco".into(),
            });
        }
        Ok(out)
    }

    fn cmd_preview(&self, action: &str, package_id: &str) -> Result<String> {
        match action {
            "install" => Ok(format!("choco install {package_id} -y --no-progress")),
            "uninstall" => Ok(format!("choco uninstall {package_id} -y --no-progress")),
            "upgrade_all" => Ok("choco upgrade all -y --no-progress".into()),
            _ => Err(SysError::BadParam(format!("未知操作: {action}"))),
        }
    }

    fn run_action(&self, action: &str, package_id: &str, emit: &mut dyn FnMut(String)) -> Result<Vec<String>> {
        let args: Vec<String> = match action {
            "install" => vec!["install", package_id, "-y", "--no-progress"].into_iter().map(String::from).collect(),
            "uninstall" => vec!["uninstall", package_id, "-y", "--no-progress"].into_iter().map(String::from).collect(),
            "upgrade_all" => vec!["upgrade", "all", "-y", "--no-progress"].into_iter().map(String::from).collect(),
            _ => return Err(SysError::BadParam(format!("未知操作: {action}"))),
        };
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_lines("choco.exe", &refs, |l| emit(l))
    }
}

/// 默认管理器集合（winget / scoop / choco）
pub fn builtin_managers() -> Vec<Box<dyn PkgManager>> {
    vec![Box::new(WingetManager), Box::new(ScoopManager), Box::new(ChocoManager)]
}

/// SY2 合并视图：多源去重，winget 优先（同 name 小写 key 首个胜出）
pub fn merge_installed(sources: Vec<(&'static str, Vec<PkgEntry>)>) -> Vec<PkgEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (_, pkgs) in sources {
        for p in pkgs {
            let key = p.name.to_lowercase();
            if seen.insert(key) {
                out.push(p);
            }
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_columns_basic() {
        let line = "Name                    Id              Version      Available    Source";
        let cols = split_columns(line);
        assert_eq!(cols, vec!["Name", "Id", "Version", "Available", "Source"]);
        // 列内单空格保留
        let cols2 = split_columns("7-Zip 24.08 (x64)        7zip.7zip             24.08");
        assert_eq!(cols2, vec!["7-Zip 24.08 (x64)", "7zip.7zip", "24.08"]);
    }

    #[test]
    fn winget_table_parse() {
        let lines: Vec<String> = vec![
            "以下是部分已安装的包".into(),
            "Name                     Id                    Version      Available  Source".into(),
            "-----------------------------------------------------------------------------".into(),
            "7-Zip 24.08 (x64)        7zip.7zip             24.08                   winget".into(),
            "Git                      Git.Git               2.47.1       2.48.0     winget".into(),
            "".into(),
        ]
        .into_iter()
        .collect();
        let pkgs = parse_winget_table(&lines);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].id, "7zip.7zip");
        assert_eq!(pkgs[0].available, None);
        assert_eq!(pkgs[1].available.as_deref(), Some("2.48.0"));
        assert_eq!(pkgs[1].source, "winget");
    }

    #[test]
    fn merge_prefers_first_source_and_sorts() {
        let pkg = |id: &str, name: &str, ver: &str, src: &'static str| PkgEntry {
            id: id.into(),
            name: name.into(),
            version: ver.into(),
            available: None,
            source: src.into(),
        };
        let a = vec![pkg("git", "Git", "2.1", "winget")];
        let b = vec![pkg("git", "Git", "9.9", "scoop"), pkg("ripgrep", "ripgrep", "14", "scoop")];
        let merged = merge_installed(vec![("winget", a), ("scoop", b)]);
        // 排序按 name 小写："git" < "ripgrep"
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].name, "Git");
        assert_eq!(merged[1].name, "ripgrep");
        assert_eq!(merged[0].source, "winget"); // winget 优先
    }

    #[test]
    fn cmd_previews_show_exact_command() {
        let w = WingetManager;
        assert!(w.cmd_preview("install", "Git.Git").unwrap().contains("winget install --id Git.Git --exact --silent"));
        assert!(w.cmd_preview("uninstall", "Git.Git").unwrap().contains("winget uninstall"));
        assert!(w.cmd_preview("upgrade_all", "").unwrap().contains("winget upgrade --all"));
        assert!(w.cmd_preview("bad", "").is_err());
    }

    #[test]
    fn availability_probe_does_not_panic() {
        // 环境相关：winget 大多数 Win10 1809+ 有；只保证不 panic
        let _ = WingetManager.available();
        let _ = ScoopManager.available();
        let _ = ChocoManager.available();
    }
}
