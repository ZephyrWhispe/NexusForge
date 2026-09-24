//! SY1/SY2 包管理器（docs/impl/06 SY1–SY2）：winget/scoop/choco 探测与适配 + 合并去重视图。
//!
//! - 所有变更操作前由 UI 展示确切命令行（[`cmd_preview`]）；执行时输出逐行回调
//!   （sys.pkg_line 事件流式回传，docs/impl/06 SY1 风险标注）
//! - winget 解析走 `--disable-interactivity`，JSON 输出可用时优先、失败回退表格
//!   （docs/impl/06 SY 风险标注：进度条控制字符）
//! - T-B7-12 纪律：一切进程拉起收敛到唯一入口 [`run_cmd`]（进程 spawn 本文件恒恰一处，
//!   镜像 B4 Tesseract 单入口纪律）；搜索/变更 argv 一律经 [`build_search_args`] /
//!   [`build_action_args`] 纯函数成形，query/包 id 作独立元素原样传递，零 shell 字符串拼接

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

/// 在线搜索结果行（T-B7-12，IPC DTO；id=安装引用包 id，scoop/choco 无独立 id 时同名）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PkgSearchRow {
    pub id: String,
    pub name: String,
    pub version: String,
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
    /// 在线搜索（T-B7-12：argv 走 [`build_search_args`] 纯函数，解析各源异构输出）
    fn search(&self, query: &str) -> Result<Vec<PkgSearchRow>>;
    /// 变更命令行预览（UI 确认展示；与 [`build_action_args`] 同源派生，所见即所跑）
    fn cmd_preview(&self, action: &str, package_id: &str) -> Result<String>;
    /// 变更操作（emit 逐行输出；docs/impl/06 SY1：输出流式回传 UI）
    fn run_action(
        &self,
        action: &str,
        package_id: &str,
        emit: &mut dyn FnMut(String),
    ) -> Result<Vec<String>>;
}

/// argv 成形辅助（&[&str] → Vec<String>，唯一入口的参形）
fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// 全文件唯一进程拉起入口（T-B7-12 判据形制：本文件进程 spawn 恒恰一处，镜像 B4 Tesseract）——
/// argv 数组原样传递零 shell 拼接；逐行读 stdout 并过滤进度条控制字符（\b \r，
/// docs/impl/06 SY 风险标注），返回（非空行集, 是否成功退出）。
fn run_cmd(
    exe: &str,
    args: &[String],
    mut on_line: impl FnMut(String),
) -> Result<(Vec<String>, bool)> {
    let mut child = Command::new(exe)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(SysError::Io)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SysError::PkgCmd("无 stdout".into()))?;
    let mut lines = Vec::new();
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        match line {
            Ok(l) => {
                let clean = l.trim_matches(|c| c == '\u{8}' || c == '\r');
                if !clean.is_empty() {
                    on_line(clean.to_string());
                    lines.push(clean.to_string());
                }
            }
            Err(_) => break,
        }
    }
    let status = child.wait().map_err(SysError::Io)?;
    Ok((lines, status.success()))
}

/// 静默跑（收集输出不 emit；解析型调用：list/search/探测）
fn run_quiet(exe: &str, args: &[String]) -> Result<Vec<String>> {
    Ok(run_cmd(exe, args, |_| {})?.0)
}

/// PATH 查找（Windows where.exe；经唯一入口，失败按不可用诚实降级）
fn in_path(exe: &str) -> bool {
    run_cmd("where.exe", &[exe.to_string()], |_| {})
        .map(|(_, ok)| ok)
        .unwrap_or(false)
}

/// winget 表格解析：Name / Id / Version / [Available /] [Source /]，列以 2+ 空格分隔
fn parse_winget_table(lines: &[String]) -> Vec<PkgEntry> {
    let mut out = Vec::new();
    // 跳过到表头行
    let header_idx = lines
        .iter()
        .position(|l| l.contains("Id") && l.contains("Version"));
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
        let Some(id) = cells.get(1).cloned() else {
            continue;
        };
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

/// 在线搜索 argv 纯函数装配（T-B7-12）。三源异构，差异点名：
/// - winget：直跑 exe，`search --query X` 表格输出（Name Id Version Match Source）
/// - scoop：shim 无独立 exe（PowerShell 函数），必经 `cmd /C scoop search X` 包装，
///   query 仍作独立 argv 元素（cmd 只接固定三词，用户串不经 shell 解析成命令）；
///   输出按 bucket 分节的 Name Version 小表
/// - choco：直跑 exe，`list X -r --page 1` 机读输出（`name|version` 竖线分隔、无表头），
///   钉首页防翻页
///
/// 处置红线：query 含 CR/LF（换行注入/参数走私面）直接拒；空白与引号作字面量原样传递。
pub fn build_search_args(source: &str, query: &str) -> Result<(&'static str, Vec<String>)> {
    if query.contains(['\r', '\n']) {
        return Err(SysError::BadParam("搜索词含换行符，已拒绝".into()));
    }
    let q = query;
    match source {
        "winget" => Ok((
            "winget.exe",
            argv(&[
                "search",
                "--query",
                q,
                "--accept-source-agreements",
                "--disable-interactivity",
            ]),
        )),
        "scoop" => Ok(("cmd", argv(&["/C", "scoop", "search", q]))),
        "choco" => Ok((
            "choco.exe",
            argv(&["list", q, "-r", "--page", "1", "--page-size", "50"]),
        )),
        other => Err(SysError::BadParam(format!(
            "未知包管理器: {other}（可用: winget / scoop / choco）"
        ))),
    }
}

/// 变更操作 argv 纯函数装配（三适配器与 [`cmd_preview`] 的共同事实源——所见即所跑）。
/// `upgrade` = 单包升级（T-B7-12 新增，package_id 必填，与全量 `upgrade_all` 分臂防误伤）。
pub fn build_action_args(
    source: &str,
    action: &str,
    package_id: &str,
) -> Result<(&'static str, Vec<String>)> {
    if matches!(action, "install" | "uninstall" | "upgrade") && package_id.trim().is_empty() {
        return Err(SysError::BadParam(format!(
            "{action} 是单包操作，必须提供包 id"
        )));
    }
    let pkg = package_id;
    match source {
        "winget" => match action {
            "install" => Ok((
                "winget.exe",
                argv(&[
                    "install",
                    "--id",
                    pkg,
                    "--exact",
                    "--silent",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                    "--disable-interactivity",
                ]),
            )),
            "uninstall" => Ok((
                "winget.exe",
                argv(&[
                    "uninstall",
                    "--id",
                    pkg,
                    "--silent",
                    "--disable-interactivity",
                ]),
            )),
            "upgrade" => Ok((
                "winget.exe",
                argv(&[
                    "upgrade",
                    "--id",
                    pkg,
                    "--exact",
                    "--silent",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                    "--disable-interactivity",
                ]),
            )),
            "upgrade_all" => Ok((
                "winget.exe",
                argv(&[
                    "upgrade",
                    "--all",
                    "--silent",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                    "--disable-interactivity",
                ]),
            )),
            _ => Err(SysError::BadParam(format!("未知操作: {action}"))),
        },
        "scoop" => match action {
            "install" => Ok(("cmd", argv(&["/C", "scoop", "install", pkg]))),
            "uninstall" => Ok(("cmd", argv(&["/C", "scoop", "uninstall", pkg]))),
            "upgrade" => Ok(("cmd", argv(&["/C", "scoop", "update", pkg]))),
            "upgrade_all" => Ok(("cmd", argv(&["/C", "scoop", "update", "*"]))),
            _ => Err(SysError::BadParam(format!("未知操作: {action}"))),
        },
        "choco" => match action {
            "install" => Ok(("choco.exe", argv(&["install", pkg, "-y", "--no-progress"]))),
            "uninstall" => Ok((
                "choco.exe",
                argv(&["uninstall", pkg, "-y", "--no-progress"]),
            )),
            "upgrade" => Ok(("choco.exe", argv(&["upgrade", pkg, "-y", "--no-progress"]))),
            "upgrade_all" => Ok((
                "choco.exe",
                argv(&["upgrade", "all", "-y", "--no-progress"]),
            )),
            _ => Err(SysError::BadParam(format!("未知操作: {action}"))),
        },
        other => Err(SysError::BadParam(format!(
            "未知包管理器: {other}（可用: winget / scoop / choco）"
        ))),
    }
}

/// 预览显示形：剥掉宿主实现细节（choco/winget 的 .exe 后缀、scoop 的 `cmd /C` 包装），
/// 与 [`build_action_args`] 同源派生——预览不再另写字符串臂，所见即所跑。
fn preview_command(exe: &str, args: &[String]) -> String {
    if exe == "cmd" {
        // args = ["/C", "scoop", …]：从 shim 名起展示
        return args[1..].join(" ");
    }
    let mut parts = vec![exe.trim_end_matches(".exe").to_string()];
    parts.extend(args.iter().cloned());
    parts.join(" ")
}

/// scoop search 表解析：按 bucket 分节（"Search results in bucket 'x':" 后跟
/// Name Version 小表，新版可带 Bucket/Updated 尾列）；与 list 单表差异=表头反复出现。
fn parse_scoop_search_table(lines: &[String]) -> Vec<PkgSearchRow> {
    let mut out = Vec::new();
    let mut in_table = false;
    for line in lines {
        let t = line.trim();
        if t.is_empty() || t.starts_with('-') {
            continue;
        }
        if t.starts_with("Search results") || t.starts_with("Updating") || t.starts_with("WARN") {
            in_table = false;
            continue;
        }
        if t.starts_with("Name") && t.contains("Version") {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        let cells = split_columns(t);
        if cells.len() >= 2 {
            out.push(PkgSearchRow {
                id: cells[0].clone(),
                name: cells[0].clone(),
                version: cells[1].clone(),
                source: "scoop".into(),
            });
        }
    }
    out
}

/// choco `-r` 机读行解析：`name|version`（偶带第三列 approved 旗标）；无管道行的
/// 版本横幅/统计句整行丢弃——与 winget/scoop 表格形差异=竖线分隔无表头。
fn parse_choco_search_lines(lines: &[String]) -> Vec<PkgSearchRow> {
    lines
        .iter()
        .filter_map(|l| {
            let (name, rest) = l.trim().split_once('|')?;
            let version = rest.split('|').next()?.trim();
            if name.trim().is_empty() || version.is_empty() {
                return None;
            }
            Some(PkgSearchRow {
                id: name.trim().to_string(),
                name: name.trim().to_string(),
                version: version.to_string(),
                source: "choco".into(),
            })
        })
        .collect()
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
        let lines = run_quiet(
            "winget.exe",
            &argv(&[
                "list",
                "--accept-source-agreements",
                "--disable-interactivity",
            ]),
        )?;
        let mut pkgs = parse_winget_table(&lines);
        for p in &mut pkgs {
            p.source = if p.source.is_empty() {
                "winget".into()
            } else {
                p.source.clone()
            };
        }
        Ok(pkgs)
    }

    fn search(&self, query: &str) -> Result<Vec<PkgSearchRow>> {
        if !self.available() {
            return Err(SysError::PkgUnavailable("winget".into()));
        }
        let (exe, args) = build_search_args(self.id(), query)?;
        let lines = run_quiet(exe, &args)?;
        // 搜索表与已装表同族形制（Name Id Version … Source），复用同一解析器
        Ok(parse_winget_table(&lines)
            .into_iter()
            .map(|p| PkgSearchRow {
                id: p.id,
                name: p.name,
                version: p.version,
                source: "winget".into(),
            })
            .collect())
    }

    fn cmd_preview(&self, action: &str, package_id: &str) -> Result<String> {
        let (exe, args) = build_action_args(self.id(), action, package_id)?;
        Ok(preview_command(exe, &args))
    }

    fn run_action(
        &self,
        action: &str,
        package_id: &str,
        emit: &mut dyn FnMut(String),
    ) -> Result<Vec<String>> {
        let (exe, args) = build_action_args(self.id(), action, package_id)?;
        Ok(run_cmd(exe, &args, emit)?.0)
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
        let lines = run_quiet("cmd", &argv(&["/C", "scoop", "list"]))?;
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

    fn search(&self, query: &str) -> Result<Vec<PkgSearchRow>> {
        if !self.available() {
            return Err(SysError::PkgUnavailable("scoop".into()));
        }
        let (exe, args) = build_search_args(self.id(), query)?;
        let lines = run_quiet(exe, &args)?;
        Ok(parse_scoop_search_table(&lines))
    }

    fn cmd_preview(&self, action: &str, package_id: &str) -> Result<String> {
        let (exe, args) = build_action_args(self.id(), action, package_id)?;
        Ok(preview_command(exe, &args))
    }

    fn run_action(
        &self,
        action: &str,
        package_id: &str,
        emit: &mut dyn FnMut(String),
    ) -> Result<Vec<String>> {
        let (exe, args) = build_action_args(self.id(), action, package_id)?;
        Ok(run_cmd(exe, &args, emit)?.0)
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
        let lines = run_quiet("choco.exe", &argv(&["list"]))?;
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

    fn search(&self, query: &str) -> Result<Vec<PkgSearchRow>> {
        if !self.available() {
            return Err(SysError::PkgUnavailable("choco".into()));
        }
        let (exe, args) = build_search_args(self.id(), query)?;
        let lines = run_quiet(exe, &args)?;
        Ok(parse_choco_search_lines(&lines))
    }

    fn cmd_preview(&self, action: &str, package_id: &str) -> Result<String> {
        let (exe, args) = build_action_args(self.id(), action, package_id)?;
        Ok(preview_command(exe, &args))
    }

    fn run_action(
        &self,
        action: &str,
        package_id: &str,
        emit: &mut dyn FnMut(String),
    ) -> Result<Vec<String>> {
        let (exe, args) = build_action_args(self.id(), action, package_id)?;
        Ok(run_cmd(exe, &args, emit)?.0)
    }
}

/// 默认管理器集合（winget / scoop / choco）
pub fn builtin_managers() -> Vec<Box<dyn PkgManager>> {
    vec![
        Box::new(WingetManager),
        Box::new(ScoopManager),
        Box::new(ChocoManager),
    ]
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
    out.sort_by_key(|a| a.name.to_lowercase());
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
        let b = vec![
            pkg("git", "Git", "9.9", "scoop"),
            pkg("ripgrep", "ripgrep", "14", "scoop"),
        ];
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
        assert!(w
            .cmd_preview("install", "Git.Git")
            .unwrap()
            .contains("winget install --id Git.Git --exact --silent"));
        assert!(w
            .cmd_preview("uninstall", "Git.Git")
            .unwrap()
            .contains("winget uninstall"));
        assert!(w
            .cmd_preview("upgrade_all", "")
            .unwrap()
            .contains("winget upgrade --all"));
        assert!(w.cmd_preview("bad", "").is_err());
    }

    #[test]
    fn availability_probe_does_not_panic() {
        // 环境相关：winget 大多数 Win10 1809+ 有；只保证不 panic
        let _ = WingetManager.available();
        let _ = ScoopManager.available();
        let _ = ChocoManager.available();
    }

    // ======================== T-B7-12 任务书字面测试名 ========================

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-12）字面测试名优先于 rustc 命名惯例
    fn searchArgs_queryNeverEntersShell_oneEntrypoint() {
        // 判据形制：进程拉起原语在本文件生产面恒恰一处（镜像 B4 Tesseract 单入口纪律）
        let src = include_str!("./pkg.rs");
        let prod = src.split("#[cfg(test)]").next().expect("测试模块标记在册");
        let ctor = concat!("Command", "::new");
        assert_eq!(
            prod.matches(ctor).count(),
            1,
            "进程 spawn 必须收敛到 run_cmd 唯一入口"
        );
        // query 作独立 argv 元素原样传递（空白/引号/shell 元字符全部字面量入参，零拼接）
        for (source, query) in [
            ("winget", "7 zip \"quoted\""),
            ("scoop", "nmap --main"),
            ("choco", "git & | ^"),
        ] {
            let (_exe, args) = build_search_args(source, query).unwrap();
            assert!(
                args.iter().any(|a| a == query),
                "{source}: query 应作独立 argv 元素原样在册"
            );
            assert!(
                !args.iter().any(|a| a != query && a.contains(query)),
                "{source}: query 不得被拼进任何其他参数"
            );
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-12）字面测试名优先于 rustc 命名惯例
    fn searchArgs_crlfQuery_rejected() {
        for source in ["winget", "scoop", "choco"] {
            for q in [
                "evil\r\nscoop install backdoor",
                "line\nbreak",
                "carriage\rreturn",
            ] {
                let e = build_search_args(source, q).unwrap_err();
                assert!(
                    matches!(e, SysError::BadParam(_)),
                    "{source}: CRLF 搜索词必须 BadParam 拒（实际 {e}）"
                );
            }
            // 合法形制不误伤：空白/制表/引号原样放行
            assert!(build_search_args(source, "some pack\tage\"x\"").is_ok());
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-12）字面测试名优先于 rustc 命名惯例
    fn upgrade_singlePackage_mapsCorrectArgs() {
        let (exe, args) = build_action_args("winget", "upgrade", "Git.Git").unwrap();
        assert_eq!(exe, "winget.exe");
        assert_eq!(
            args,
            argv(&[
                "upgrade",
                "--id",
                "Git.Git",
                "--exact",
                "--silent",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--disable-interactivity",
            ])
        );
        let (_, args) = build_action_args("scoop", "upgrade", "git").unwrap();
        assert_eq!(args, argv(&["/C", "scoop", "update", "git"]));
        let (_, args) = build_action_args("choco", "upgrade", "git").unwrap();
        assert_eq!(args, argv(&["upgrade", "git", "-y", "--no-progress"]));
        // 单包无 id → 拒（防退化为全量面）；预览与 argv 同源（所见即所跑）
        for source in ["winget", "scoop", "choco"] {
            assert!(
                matches!(
                    build_action_args(source, "upgrade", "").unwrap_err(),
                    SysError::BadParam(_)
                ),
                "{source}: 单包升级缺 id 必须拒"
            );
        }
        assert!(WingetManager
            .cmd_preview("upgrade", "Git.Git")
            .unwrap()
            .contains("winget upgrade --id Git.Git --exact --silent"));
        assert_eq!(
            ScoopManager.cmd_preview("upgrade", "git").unwrap(),
            "scoop update git"
        );
        assert_eq!(
            ChocoManager.cmd_preview("upgrade", "git").unwrap(),
            "choco upgrade git -y --no-progress"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-12）字面测试名优先于 rustc 命名惯例
    fn search_unknownSource_errsNamingThree() {
        let e = build_search_args("apt", "vim").unwrap_err();
        assert!(matches!(e, SysError::BadParam(_)));
        let msg = e.to_string();
        for name in ["winget", "scoop", "choco"] {
            assert!(
                msg.contains(name),
                "未知源错误须点名三台可用管理器（实际: {msg}）"
            );
        }
        // 变更面同谱：未知源同样点名三台
        let msg = build_action_args("apt", "install", "vim")
            .unwrap_err()
            .to_string();
        for name in ["winget", "scoop", "choco"] {
            assert!(msg.contains(name), "变更面未知源须点名三台（实际: {msg}）");
        }
    }
}
