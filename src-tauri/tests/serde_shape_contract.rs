//! D-41 G2 · serde 形制对合门（线/盘上判别式形制：Rust 枚举 ↔ TS 联合）
//!
//! 为什么要这道门（D-41 审查实况）：`SshAuth` 在 Rust 是 `#[serde(tag = "kind")]`
//! 内部判别，TS 侧却写成外部标签形，`term_ssh_connect` 的密码臂报 `unknown variant`；
//! `TermKind` 同族错位让本地/WSL 会话行的 `kind` 永远读不出形状。两半各写各的、
//! 中间没有共享 schema，这类"形制断裂"编译期与运行时都不红——只有把形制本身
//! 钉进门禁才不再复发。
//!
//! 门形制（本文件双扫描器 + 手写配对账本）：
//! - Rust 腿：`crates/**` + `src-tauri/src/**` 里每个 serde 派生（Serialize 或
//!   Deserialize 任一）的枚举，取其判别式形制（untagged / 内部 tag / 外部）与
//!   `rename_all` 或显式 `rename` 施加后的线上字面量集合。
//! - TS 腿：`src/ipc/client.ts` 里每个成员 ≥2 的 `export type` 联合，以及带
//!   `kind`/`op`/`type` 字面量判别臂的 `export interface`，同样取形制与字面量集合。
//! - 账本 `PAIRS`/`RUST_ONLY`/`TS_ONLY` 逐条登记：新增枚举或联合未记账 ⇒ 红
//!   （增量漂移自爆）；账本指向的对象或锚点找不到 ⇒ 红（陈旧账本自爆）；形制或
//!   字面量集合不对合 ⇒ 红（改一半忘一半自爆）。
//! - 正对照地板（在册"扫到零即自曝"纪律）：检出数低于地板直接判红并报扫描脱靶，
//!   绝不允许"零命中假绿"。
//!
//! 锚点口径（有意不用行号）：行号随文件上方任何一次无关插入而漂移，钉进门禁只
//! 制造噪音；故 Rust 腿锚 = 相对文件路径（搬家/改名即红），TS 腿锚 = 声明体内必须
//! 出现的字面片段（形制改写即红）。两者都是"改一处忘一处"的可命中证据。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// 判别式形制
#[derive(Clone, Debug, PartialEq, Eq)]
enum Class {
    /// `#[serde(untagged)]`
    Untagged,
    /// `#[serde(tag = "X")]` 内部判别（相邻 tagging `tag+content` 亦归此类，
    /// 判别键仍可对合）
    Internal(String),
    /// 缺省外部 tagging（含"全 unit ⇒ 裸字符串联合"这一特例）
    External,
    /// TS 腿无法归类：成员是类型引用、裸类型或多键无判别对象
    Mixed,
}

impl Class {
    fn label(&self) -> String {
        match self {
            Class::Untagged => "untagged".to_string(),
            Class::Internal(t) => format!("internal:{t}"),
            Class::External => "external".to_string(),
            Class::Mixed => "mixed".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
struct RVari {
    lit: String,
    unit: bool,
}

#[derive(Clone, Debug)]
struct REnum {
    file: String,
    line: usize,
    name: String,
    class: Class,
    vars: Vec<RVari>,
}

impl REnum {
    fn lits(&self) -> BTreeSet<String> {
        self.vars.iter().map(|v| v.lit.clone()).collect()
    }
    fn units(&self) -> BTreeSet<String> {
        self.vars
            .iter()
            .filter(|v| v.unit)
            .map(|v| v.lit.clone())
            .collect()
    }
    fn where_(&self) -> String {
        format!("{}:{} {}", self.file, self.line, self.name)
    }
}

#[derive(Clone, Debug)]
struct TsItem {
    line: usize,
    name: String,
    /// 声明全文（成员续行含在内，用于锚点片段命中）
    stmt: String,
    class: Class,
    lits: BTreeSet<String>,
    units: BTreeSet<String>,
    members: usize,
    /// interface 判别臂是摊平形（`kind: "a" | "b"` + 同层可选字段），unit 性不可判
    flattened: bool,
}

impl TsItem {
    fn where_(&self) -> String {
        format!("src/ipc/client.ts:{} {}", self.line, self.name)
    }
}

// ============================ 账本 ============================

/// Rust 枚举 ↔ TS 联合的配对账本。
///
/// `class` 取 `"kind" | "op" | "type" | "external" | "untagged"`；`subset = true` 表示
/// TS 侧只镜像 Rust 的一部分字面量（诚实局部镜像），此时仅要求 ⊆ 且不比 unit 集合。
/// `note` 恒非空：每条配对都要说清"为什么是这个形制"，账本才不是黑名单。
struct Pair {
    rust: &'static str,
    rust_file: &'static str,
    ts: &'static str,
    ts_anchor: &'static str,
    class: &'static str,
    subset: bool,
    note: &'static str,
}

const PAIRS: &[Pair] = &[
    Pair {
        rust: "RunOutcome",
        rust_file: "crates/automation-core/src/history.rs",
        ts: "RunOutcomeDto",
        ts_anchor: r#"{ kind: "success" }"#,
        class: "kind",
        subset: false,
        note: "自动化执行回执，内部 tag=kind（snake_case）",
    },
    Pair {
        rust: "Trigger",
        rust_file: "crates/automation-core/src/rule.rs",
        ts: "TriggerDto",
        ts_anchor: r#"{ kind: "event"; topic: string }"#,
        class: "kind",
        subset: false,
        note: "规则触发器三臂，内部 tag=kind（snake_case）",
    },
    Pair {
        rust: "CmpOp",
        rust_file: "crates/automation-core/src/rule.rs",
        ts: "CmpOpDto",
        ts_anchor: r#""eq" | "ne" | "gt" | "lt" | "contains""#,
        class: "external",
        subset: false,
        note: "全 unit 外部枚举在线上是裸字符串（snake_case）",
    },
    Pair {
        rust: "Expr",
        rust_file: "crates/automation-core/src/rule.rs",
        ts: "ExprDto",
        ts_anchor: r#"{ op: "leaf""#,
        class: "op",
        subset: false,
        note: "条件树判别键是 op 而非 kind（逐字核实过两侧）",
    },
    Pair {
        rust: "Action",
        rust_file: "crates/automation-core/src/rule.rs",
        ts: "ActionDto",
        ts_anchor: r#"{ kind: "ipc_command""#,
        class: "kind",
        subset: false,
        note: "动作五臂，内部 tag=kind（D6b 的 IPC 动作诚实化挂在此形制上）",
    },
    Pair {
        rust: "ItemKind",
        rust_file: "crates/desktop-core/src/index.rs",
        ts: "DesktopItemKind",
        ts_anchor: r#""app" | "action""#,
        class: "external",
        subset: false,
        note: "启动器条目类别（lowercase unit 联合）",
    },
    Pair {
        rust: "EncodingKindDto",
        rust_file: "crates/editor-core/src/session.rs",
        ts: "EditorEncodingKind",
        ts_anchor: r#""utf8" | "utf8bom""#,
        class: "external",
        subset: false,
        note: "编辑器编码走 Dto 臂（lowercase）；域内 EncodingKind 不上线，见 RUST_ONLY",
    },
    Pair {
        rust: "Eol",
        rust_file: "crates/editor-core/src/session.rs",
        ts: "EditorEol",
        ts_anchor: r#""crlf" | "lf""#,
        class: "external",
        subset: false,
        note: "换行制式（lowercase unit 联合）",
    },
    Pair {
        rust: "EolChoice",
        rust_file: "crates/editor-core/src/session.rs",
        ts: "EditorEolChoice",
        ts_anchor: r#""preserve" | "lf" | "crlf""#,
        class: "external",
        subset: false,
        note: "换行用户选项多出 preserve 第三臂",
    },
    Pair {
        rust: "SortKey",
        rust_file: "crates/file-core/src/browse.rs",
        ts: "FileSortKey",
        ts_anchor: r#""name" | "size" | "modified" | "type""#,
        class: "external",
        subset: false,
        note: "浏览排序键（snake_case unit 联合）",
    },
    Pair {
        rust: "ConflictPolicy",
        rust_file: "crates/file-core/src/conflict.rs",
        ts: "ConflictPolicyDto",
        ts_anchor: r#""ask" | "skip" | "overwrite" | "rename""#,
        class: "external",
        subset: false,
        note: "冲突策略：rename 臂带载荷，TS 侧以同名单键对象镜像",
    },
    Pair {
        rust: "FixPolicy",
        rust_file: "crates/file-core/src/namefix.rs",
        ts: "NameFixPolicyDto",
        ts_anchor: r#""ask" | "auto_rename" | "reject""#,
        class: "external",
        subset: false,
        note: "文件名修复策略（snake_case unit 联合）",
    },
    Pair {
        rust: "OpKind",
        rust_file: "crates/file-core/src/ops.rs",
        ts: "FileOpKind",
        ts_anchor: r#""copy" | "move" | "delete""#,
        class: "external",
        subset: false,
        note: "文件操作类别（snake_case unit 联合）",
    },
    Pair {
        rust: "OpState",
        rust_file: "crates/file-core/src/ops.rs",
        ts: "OpStateDto",
        ts_anchor: r#""queued" | "running" | "paused""#,
        class: "external",
        subset: false,
        note: "队列状态机六态（snake_case unit 联合）",
    },
    Pair {
        rust: "OpEndpoint",
        rust_file: "crates/file-core/src/ops.rs",
        ts: "FileEndpointDto",
        ts_anchor: "{ driver_id: string; path: string }",
        class: "untagged",
        subset: false,
        note: "untagged：本地臂=裸 PathBuf、远端臂=多键对象，故 TS 对应形必为无判别 Mixed（比臂数不比字面量）",
    },
    Pair {
        rust: "TransferDirection",
        rust_file: "crates/file-core/src/ops.rs",
        ts: "TransferDirectionDto",
        ts_anchor: r#""local" | "upload" | "download""#,
        class: "external",
        subset: false,
        note: "传输方向（snake_case unit 联合）",
    },
    Pair {
        rust: "Preview",
        rust_file: "crates/file-core/src/preview.rs",
        ts: "PreviewDto",
        ts_anchor: r#"{ kind: "text"; content: string"#,
        class: "kind",
        subset: false,
        note: "预览四臂，内部 tag=kind",
    },
    Pair {
        rust: "RemoteProtocol",
        rust_file: "crates/file-core/src/profile.rs",
        ts: "RemoteProtocolDto",
        ts_anchor: r#""web_dav" | "https" | "sftp" | "ftp""#,
        class: "external",
        subset: false,
        note: "远端协议（snake_case；web_dav 的下划线由 rename_all 生成）",
    },
    Pair {
        rust: "AuthKind",
        rust_file: "crates/file-core/src/profile.rs",
        ts: "AuthKindDto",
        ts_anchor: r#"{ kind: "ssh_key"; key_path: string }"#,
        class: "kind",
        subset: false,
        note: "凭据来源五臂，内部 tag=kind（盘上/线上同一形制）",
    },
    Pair {
        rust: "AuthSource",
        rust_file: "crates/file-core/src/profile.rs",
        ts: "AuthSourceDto",
        ts_anchor: r#""anonymous" | "key_file" | "vault_entry""#,
        class: "external",
        subset: false,
        note: "凭据来源粗分类（snake_case unit 联合，仅 Serialize 侧上线）",
    },
    Pair {
        rust: "CaseMode",
        rust_file: "crates/file-core/src/rename.rs",
        ts: "RenameCaseDto",
        ts_anchor: r#""none" | "lower" | "upper""#,
        class: "external",
        subset: false,
        note: "大小写改写档（snake_case unit 联合）",
    },
    Pair {
        rust: "AppError",
        rust_file: "crates/host-core/src/error.rs",
        ts: "AppErrorDto",
        ts_anchor: r#"kind: "Module" | "Storage""#,
        class: "kind",
        subset: false,
        note: "错误契约（M1 冻结）：相邻 tagging tag=kind/content=data，无 rename_all 故 Pascal 字面",
    },
    Pair {
        rust: "ModuleState",
        rust_file: "crates/host-core/src/module.rs",
        ts: "ModuleState",
        ts_anchor: r#""Uninitialized" | "Stopped""#,
        class: "external",
        subset: false,
        note: "全 unit 外部枚举且无 rename_all ⇒ 线上是 Pascal 裸串（双侧同名）",
    },
    Pair {
        rust: "ClipContent",
        rust_file: "crates/host-core/src/ports.rs",
        ts: "ClipContentDto",
        ts_anchor: "{ Text: { text: string",
        class: "external",
        subset: false,
        note: "剪切板内容三臂：外部 tagging 单键对象，键名 Pascal 逐字（无 rename_all）",
    },
    Pair {
        rust: "Resumable",
        rust_file: "crates/host-core/src/storage.rs",
        ts: "ResumableDto",
        ts_anchor: r#""range" | "append" | "whole""#,
        class: "external",
        subset: false,
        note: "续传策略（snake_case unit 联合）",
    },
    Pair {
        rust: "TweakAction",
        rust_file: "crates/sys-core/src/winops.rs",
        ts: "WinopsRegistryActionDto",
        ts_anchor: r#"type: "registry""#,
        class: "type",
        subset: true,
        note: "TS 只精确镜像 registry 臂，其余动作走 WinopsOpaqueActionDto 的 type: string 兜底 ⇒ 局部镜像（subset 档）",
    },
    Pair {
        rust: "ScanState",
        rust_file: "crates/sys-core/src/winops.rs",
        ts: "WinopsScanState",
        ts_anchor: r#""applied" | "not_applied" | "needs_admin""#,
        class: "external",
        subset: false,
        note: "调整项扫描态（snake_case unit 联合）",
    },
    Pair {
        rust: "BindAddr",
        rust_file: "crates/term-core/src/forward.rs",
        ts: "ForwardBindAddrDto",
        ts_anchor: r#"| "127.0.0.1""#,
        class: "external",
        subset: false,
        note: "绑定白名单：Loopback 显式 rename 成 \"127.0.0.1\" 且是 unit ⇒ 裸串，Other 臂带 snake 键（unit 集合对合是本案关键）",
    },
    Pair {
        rust: "ForwardKind",
        rust_file: "crates/term-core/src/forward.rs",
        ts: "ForwardKindDto",
        ts_anchor: "{ local: { listen_port: number",
        class: "external",
        subset: false,
        note: "-L/-R/-D 三臂（snake_case 外部单键对象；载荷字段名逐字镜像 Rust snake）",
    },
    Pair {
        rust: "ForwardState",
        rust_file: "crates/term-core/src/forward.rs",
        ts: "ForwardStateDto",
        ts_anchor: "| \"closed\"",
        class: "external",
        subset: false,
        note: "listening/refused 带载荷、closed 是 unit ⇒ 裸串与包形混合（unit 集合对合防把 closed 写成包形）",
    },
    Pair {
        rust: "TermKind",
        rust_file: "crates/term-core/src/session.rs",
        ts: "TermKindDto",
        ts_anchor: r#"{ kind: "local" }"#,
        class: "kind",
        subset: false,
        note: "D-41 D2 案发现场：内部 tag=kind，TS 侧曾误写外部包形",
    },
    Pair {
        rust: "SshAuth",
        rust_file: "crates/term-core/src/ssh.rs",
        ts: "SshAuthDto",
        ts_anchor: r#"kind: "password" | "key""#,
        class: "kind",
        subset: false,
        note: "D-41 D1 案发现场：内部 tag=kind，TS 侧曾误写外部标签形致 unknown variant",
    },
    Pair {
        rust: "FieldKind",
        rust_file: "crates/vault-core/src/model.rs",
        ts: "FieldKindDto",
        ts_anchor: r#""password" | "url" | "note" | "otp" | "text""#,
        class: "external",
        subset: false,
        note: "密码库字段类别（lowercase unit 联合）",
    },
];

/// Rust 侧有、线上无对应 TS 判别联合的枚举（多为盘上/内核内部形，或 TS 只用摊平
/// 对象与字符串字段）。登记即视为"已知不上线"，摘登记即红。
struct RustOnly {
    rust: &'static str,
    rust_file: &'static str,
    note: &'static str,
}

const RUST_ONLY: &[RustOnly] = &[
    RustOnly {
        rust: "BackupItem",
        rust_file: "crates/sys-core/src/winops.rs",
        note: "备份台账条目只进 WinopsApplyReportDto.backup（C4 已收成 unknown[]），无判别联合",
    },
    RustOnly {
        rust: "Edge",
        rust_file: "crates/kvm-core/src/edge.rs",
        note: "KVM 边缘方向以 lowercase 字符串参数下发，TS 侧写 string 而非联合",
    },
    RustOnly {
        rust: "EncodingKind",
        rust_file: "crates/editor-core/src/session.rs",
        note: "域内编码枚举；上线走同字面量的 EncodingKindDto（见 PAIRS）",
    },
    RustOnly {
        rust: "NodeKind",
        rust_file: "crates/proxy-core/src/sub.rs",
        note: "订阅节点协议在 TS 侧是字符串字段（解析失败即整节点降级），无判别联合",
    },
    RustOnly {
        rust: "PresetAuthKind",
        rust_file: "crates/file-core/src/preset.rs",
        note: "预设凭据档只用于远端预设文件内部，webview 侧无对应联合",
    },
    RustOnly {
        rust: "RawInput",
        rust_file: "crates/host-core/src/ports.rs",
        note: "KVM 输入事件是内核侧端口形制，不过 IPC",
    },
    RustOnly {
        rust: "RegValue",
        rust_file: "crates/host-core/src/ports.rs",
        note: "注册表值形制在 TS 侧摊成 WinopsRegistryActionDto.data 的可选字段并集",
    },
    RustOnly {
        rust: "RepairKind",
        rust_file: "crates/host-core/src/ports.rs",
        note: "系统修复动作以 snake 字符串参数下发（whitelisted run），无 TS 判别联合",
    },
    RustOnly {
        rust: "RouteMode",
        rust_file: "crates/proxy-core/src/config.rs",
        note: "代理模式三档在 TS 侧是 string 字段（schema 驱动表单），无判别联合",
    },
    RustOnly {
        rust: "StartType",
        rust_file: "crates/host-core/src/ports.rs",
        note: "服务启动类型经 TweakAction.service 的 snake 字符串字段下发",
    },
    RustOnly {
        rust: "SvcAction",
        rust_file: "crates/sys-core/src/winops.rs",
        note: "服务瞬态启停同上（TweakAction.service.action 字段），无独立 TS 联合",
    },
    RustOnly {
        rust: "SyncMsg",
        rust_file: "crates/sync-core/src/transport.rs",
        note: "同步协议帧走 LAN 传输层，不进 webview IPC，故无 TS 对应",
    },
];

/// TS 侧有、Rust 侧无 serde 枚举的判别联合（前端视图态/局部镜像/字符串白名单）。
struct TsOnly {
    ts: &'static str,
    ts_anchor: &'static str,
    note: &'static str,
}

const TS_ONLY: &[TsOnly] = &[
    TsOnly {
        ts: "OcrExportFormat",
        ts_anchor: r#""txt" | "md""#,
        note: "OCR 导出格式在 Rust 侧是 match 分支的字符串字面，非枚举",
    },
    TsOnly {
        ts: "EnqueueDispatch",
        ts_anchor: r#"{ kind: "nameFixPreview""#,
        note: "文件名修复排队反馈是纯前端视图态，后端只发各单项事件",
    },
    TsOnly {
        ts: "WinopsActionDto",
        ts_anchor: "WinopsRegistryActionDto | WinopsOpaqueActionDto",
        note: "TweakAction 的 TS 侧拆成精确 registry 臂 + opaque 兜底臂（pair 记在 WinopsRegistryActionDto）",
    },
    TsOnly {
        ts: "ProxyRuleV2Dto",
        ts_anchor: r#"kind: "domain""#,
        note: "规则 kind 七档在 Rust 是 rules.rs 的字符串白名单校验，非 serde 枚举",
    },
    TsOnly {
        ts: "CanvasNodeDto",
        ts_anchor: r#"kind: "note" | "sticky" | "image" | string"#,
        note: "画布节点类别在 Rust 侧是自由字符串（自定义类型可扩展），联合尾臂 | string 即此意",
    },
];

// ============================ 公版文本工具 ============================

/// 去掉行注释（`//` 起、不在字符串字面量内），保留行号语义
fn strip_line_comment(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_str = false;
    let mut prev_backslash = false;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if prev_backslash {
                prev_backslash = false;
            } else if c == '\\' {
                prev_backslash = true;
            } else if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            break;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// 顶层分隔符切分（()[]{}/ 深度为 0 处才切；字符串字面量内的分隔符不切）
fn split_top(text: &str, sep: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut in_str = false;
    for c in text.chars() {
        if in_str {
            cur.push(c);
            if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                cur.push(c);
            }
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(c);
            }
            c if c == sep && depth == 0 => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
}

/// 取 `key = "值"` / `key: "值"` 里的引号内容（首个 key 出现处之后第一个引号串）
fn quoted_after(hay: &str, key: &str) -> Option<String> {
    let at = hay.find(key)?;
    let rest = &hay[at + key.len()..];
    let open = rest.find('"')?;
    let inner = &rest[open + 1..];
    let close = inner.find('"')?;
    Some(inner[..close].to_string())
}

fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 && b[0] == b'"' && b[b.len() - 1] == b'"' {
        Some(s[1..s.len() - 1].to_string())
    } else {
        None
    }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// camel/Pascal 标识符拆词（连续大写仅在"后接小写"处断开；数字附着于前词）
fn words(ident: &str) -> Vec<String> {
    let cs: Vec<char> = ident.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, c) in cs.iter().enumerate() {
        if c.is_uppercase() && !cur.is_empty() {
            let prev_is_lower = cs[i - 1].is_lowercase() || cs[i - 1].is_numeric();
            let next_is_lower = i + 1 < cs.len() && cs[i + 1].is_lowercase();
            if prev_is_lower || next_is_lower {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(*c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `rename_all` 档位下的线上字面量（档位名逐字对齐 serde，未知档位直接 panic 自曝）
fn wire_lit(ident: &str, rename_all: Option<&str>) -> String {
    match rename_all {
        None => ident.to_string(),
        Some("lowercase") => ident.to_lowercase(),
        Some("UPPERCASE") => ident.to_uppercase(),
        Some("PascalCase") => words(ident).join(""),
        Some("camelCase") => {
            let mut w = words(ident).into_iter();
            let head = w.next().unwrap_or_default().to_lowercase();
            head + &w.map(|s| cap(&s)).collect::<String>()
        }
        Some("snake_case") => words(ident).join("_").to_lowercase(),
        Some("SCREAMING_SNAKE_CASE") => words(ident).join("_").to_uppercase(),
        Some("kebab-case") => words(ident).join("-").to_lowercase(),
        Some(other) => panic!("serde_shape_contract：未知 rename_all 档位 {other}（{ident}）"),
    }
}

// ============================ Rust 腿扫描 ============================

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri 的上一级是仓根")
        .to_path_buf()
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_rs(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }
}

fn leading_ident(code: &str) -> Option<String> {
    let s: String = code
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if s.is_empty() || s.chars().next()?.is_numeric() {
        return None;
    }
    Some(s)
}

/// `enum X` / `pub enum X` / `pub(crate) enum X` / `pub(super) enum X` ⇒ X
fn enum_decl_name(t: &str) -> Option<String> {
    let rest = ["pub(crate) ", "pub(super) ", "pub "]
        .iter()
        .find_map(|p| t.strip_prefix(*p))
        .unwrap_or(t);
    let after = rest.strip_prefix("enum ")?;
    leading_ident(after.trim_start())
}

fn rs_class(attrs: &str) -> Class {
    if attrs.contains("untagged") {
        return Class::Untagged;
    }
    match quoted_after(attrs, "tag") {
        Some(tag) => Class::Internal(tag),
        None => Class::External,
    }
}

fn parse_rs_file(path: &Path, rel: &str) -> Vec<REnum> {
    let src = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<String> = src.lines().map(strip_line_comment).collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        let Some(name) = enum_decl_name(t) else {
            continue;
        };
        // 紧邻其上的属性块/文档注释（空行或代码行即断，断则视为无 serde 属性）
        let mut attrs = String::new();
        let mut j = i as isize - 1;
        while j >= 0 {
            let p = lines[j as usize].trim();
            if p.starts_with("#[") || p.starts_with("///") || p.starts_with("//!") || p == "//" {
                attrs.push_str(p);
                attrs.push('\n');
                j -= 1;
            } else {
                break;
            }
        }
        if !attrs.contains("derive")
            || !(attrs.contains("Serialize") || attrs.contains("Deserialize"))
        {
            continue;
        }
        let class = rs_class(&attrs);
        let rename_all = quoted_after(&attrs, "rename_all");
        // 枚举体：自声明行起做花括号配平（注释已在 lines 阶段剥除）
        let mut body = String::new();
        let mut depth = 0i32;
        let mut started = false;
        'outer: for l in &lines[i..] {
            for c in l.chars() {
                if !started {
                    if c == '{' {
                        started = true;
                        depth = 1;
                    }
                    continue;
                }
                match c {
                    '{' => {
                        depth += 1;
                        body.push(c);
                    }
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break 'outer;
                        }
                        body.push(c);
                    }
                    _ => body.push(c),
                }
            }
            if started {
                body.push('\n');
            }
        }
        let mut vars = Vec::new();
        for seg in split_top(&body, ',') {
            // 变体前的属性才算数（字段级 rename 不得冒充变体 rename）
            let mut seg_attrs = String::new();
            let mut code = String::new();
            let mut in_code = false;
            for l in seg.lines() {
                let lt = l.trim();
                if lt.is_empty() || lt.starts_with("#!") {
                    continue;
                }
                if !in_code && lt.starts_with("#[") {
                    seg_attrs.push_str(lt);
                    seg_attrs.push('\n');
                } else {
                    in_code = true;
                    code.push_str(lt);
                    code.push(' ');
                }
            }
            let code = code.trim().to_string();
            let Some(ident) = leading_ident(&code) else {
                continue;
            };
            let lit = match quoted_after(&seg_attrs, "rename") {
                Some(r) => r,
                None => wire_lit(&ident, rename_all.as_deref()),
            };
            let after = code[ident.len()..].trim_start();
            let unit = !(after.starts_with('(') || after.starts_with('{'));
            vars.push(RVari { lit, unit });
        }
        if vars.is_empty() {
            continue;
        }
        out.push(REnum {
            file: rel.to_string(),
            line: i + 1,
            name,
            class,
            vars,
        });
    }
    out
}

fn rust_enums() -> Vec<REnum> {
    let root = repo_root();
    let mut files = Vec::new();
    collect_rs(&root.join("crates"), &mut files);
    collect_rs(&root.join("src-tauri").join("src"), &mut files);
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let rel = f
            .strip_prefix(&root)
            .unwrap_or(&f)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.contains("/target/") || rel.ends_with("build.rs") {
            continue;
        }
        out.extend(parse_rs_file(&f, &rel));
    }
    out
}

// ============================ TS 腿扫描 ============================

fn ts_src() -> String {
    let p = repo_root().join("src").join("ipc").join("client.ts");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("读不到 {}：{e}", p.display()))
}

/// 对象字面量/类型体的顶层条目（TS 类型体内 `;` 与 `,` 都是分隔符）
fn obj_entries(inner: &str) -> Vec<String> {
    split_top(inner, ';')
        .into_iter()
        .flat_map(|s| split_top(&s, ','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MSort {
    /// 裸字符串字面量（外部枚举的 unit 臂）
    StrLit,
    /// `{ kind|op|type: "lit", ... }` 判别对象
    DiscObj,
    /// `{ 非判别键: {...} }` 单键包形（外部枚举的带载荷臂）
    WrapObj,
    /// 归类不了（类型引用、元组、多键无判别对象）
    Opaque,
}

/// 联合成员归类：(形状, 判别键, 线上字面量, 是否 unit)
fn classify_member(m: &str) -> (MSort, Option<String>, String, bool) {
    let t = m.trim();
    if let Some(lit) = unquote(t) {
        return (MSort::StrLit, None, lit, true);
    }
    if t.starts_with('{') && t.ends_with('}') {
        let entries = obj_entries(&t[1..t.len() - 1]);
        if let Some(first) = entries.first() {
            if let Some(colon) = first.find(':') {
                let raw_key = first[..colon].trim().trim_end_matches('?');
                let value = first[colon + 1..].trim().to_string();
                let key = unquote(raw_key).unwrap_or_else(|| raw_key.to_string());
                if matches!(key.as_str(), "kind" | "op" | "type") {
                    if let Some(lit) = unquote(&value) {
                        return (MSort::DiscObj, Some(key), lit, entries.len() == 1);
                    }
                }
                if entries.len() == 1 {
                    let inner = value
                        .strip_prefix('{')
                        .and_then(|v| v.strip_suffix('}'))
                        .unwrap_or(&value)
                        .trim();
                    let unit = inner.is_empty() || inner == "never";
                    return (MSort::WrapObj, None, key, unit);
                }
            }
        }
    }
    (MSort::Opaque, None, t.to_string(), false)
}

fn decl_name(t: &str, keyword: &str) -> Option<String> {
    let rest = t.strip_prefix(keyword)?.trim_start();
    leading_ident(rest)
}

/// `export type X = A | B | …;`（成员 ≥2 才算联合；单对象/元组/别名不归本门管辖）
fn parse_type_alias(lines: &[String], i: usize, t: &str) -> Option<TsItem> {
    let name = decl_name(t, "export type")?;
    let eq = t.find('=')?;
    let mut stmt = String::new();
    let mut depth = 0i32;
    let mut j = i;
    let mut chunk: &str = &t[eq + 1..];
    loop {
        let mut done = false;
        for c in chunk.chars() {
            match c {
                '(' | '[' | '{' => {
                    depth += 1;
                    stmt.push(c);
                }
                ')' | ']' | '}' => {
                    depth -= 1;
                    stmt.push(c);
                }
                ';' if depth == 0 => {
                    done = true;
                    break;
                }
                _ => stmt.push(c),
            }
        }
        if done {
            break;
        }
        j += 1;
        let Some(next) = lines.get(j) else { break };
        stmt.push(' ');
        chunk = next;
    }
    let stmt = stmt.replace('\n', " ");
    if stmt.trim().is_empty() {
        return None;
    }
    let raw_members = split_top(&stmt, '|');
    let members: Vec<String> = raw_members
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if members.len() < 2 {
        return None;
    }
    let mut lits = BTreeSet::new();
    let mut units = BTreeSet::new();
    let mut tags: BTreeSet<String> = BTreeSet::new();
    let mut all_disc = true;
    let mut all_ext = true;
    for m in &members {
        let (sort, tag, lit, unit) = classify_member(m);
        lits.insert(lit.clone());
        if unit {
            units.insert(lit);
        }
        match sort {
            MSort::DiscObj => {
                if let Some(k) = tag {
                    tags.insert(k);
                }
                all_ext = false;
            }
            MSort::StrLit => all_disc = false,
            MSort::WrapObj => all_disc = false,
            MSort::Opaque => {
                all_disc = false;
                all_ext = false;
            }
        }
    }
    let class = if all_disc && tags.len() == 1 {
        Class::Internal(tags.iter().next().cloned().unwrap())
    } else if all_ext {
        Class::External
    } else {
        Class::Mixed
    };
    Some(TsItem {
        line: i + 1,
        name,
        stmt,
        class,
        lits,
        units,
        members: members.len(),
        flattened: false,
    })
}

/// `export interface X { kind|"op"|"type": "a" | "b"; … }`（摊平判别臂）
fn parse_interface(lines: &[String], i: usize, t: &str) -> Option<TsItem> {
    let name = decl_name(t, "export interface")?;
    let mut depth = 0i32;
    let mut started = false;
    let mut fields: Vec<String> = Vec::new();
    let mut body = String::new();
    for l in lines[i..].iter() {
        let trimmed = l.trim();
        let at_field_level = started && depth == 1;
        if at_field_level && trimmed.contains(':') {
            fields.push(trimmed.to_string());
        }
        body.push_str(trimmed);
        body.push(' ');
        for c in l.chars() {
            match c {
                '{' | '(' | '[' => {
                    depth += 1;
                    if !started && c == '{' && depth == 1 {
                        started = true;
                    }
                }
                '}' | ')' | ']' => depth -= 1,
                _ => {}
            }
        }
        if started && depth == 0 {
            break;
        }
    }
    let mut tag: Option<String> = None;
    let mut lits = BTreeSet::new();
    for f in &fields {
        let Some(colon) = split_top(f, ';').remove(0).find(':') else {
            continue;
        };
        let key = f[..colon].trim().trim_end_matches('?');
        if !matches!(key, "kind" | "op" | "type") {
            continue;
        }
        let found: Vec<String> = f[colon + 1..]
            .trim_end()
            .trim_end_matches(';')
            .trim_end_matches(',')
            .split('|')
            .filter_map(unquote)
            .collect();
        if !found.is_empty() {
            tag = Some(key.to_string());
            lits.extend(found);
        }
    }
    let tag = tag?;
    if lits.is_empty() {
        return None;
    }
    let lit_count = lits.len();
    Some(TsItem {
        line: i + 1,
        name,
        stmt: body,
        class: Class::Internal(tag),
        lits,
        units: BTreeSet::new(),
        members: lit_count,
        flattened: true,
    })
}

fn ts_items() -> Vec<TsItem> {
    let src = ts_src();
    let lines: Vec<String> = src.lines().map(strip_line_comment).collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if let Some(item) = parse_type_alias(&lines, i, t).or_else(|| parse_interface(&lines, i, t))
        {
            out.push(item);
        }
    }
    out
}

// ============================ 对合判据 ============================

fn find_rust(all: &[REnum], name: &str) -> Option<REnum> {
    all.iter().find(|e| e.name == name).cloned()
}

fn find_ts(all: &[TsItem], name: &str) -> Option<TsItem> {
    all.iter().find(|x| x.name == name).cloned()
}

fn declared_class(d: &str) -> Class {
    match d {
        "external" => Class::External,
        "untagged" => Class::Untagged,
        other => Class::Internal(other.to_string()),
    }
}

/// 单条配对的完整对合检查，返回失败原因（空=通过）
fn check_pair(p: &Pair, r: &REnum, t: &TsItem) -> Vec<String> {
    let mut bad = Vec::new();
    if r.file != p.rust_file {
        bad.push(format!(
            "{} 实际在 {}，账本登记 {}",
            p.rust, r.file, p.rust_file
        ));
    }
    if !t.stmt.contains(p.ts_anchor) {
        bad.push(format!(
            "{} 声明体内找不到账本锚点 {:?}（TS 形制被改写而未记正）",
            p.ts, p.ts_anchor
        ));
    }
    if p.note.is_empty() {
        bad.push(format!("{} ↔ {} 账本缺 note", p.rust, p.ts));
    }
    let want = declared_class(p.class);
    if r.class != want {
        bad.push(format!(
            "{} 的 Rust 形制是 {}，账本记 {}",
            r.where_(),
            r.class.label(),
            p.class
        ));
    }
    let class_ok = match &want {
        Class::Untagged => t.class == Class::Mixed,
        Class::External => t.class == Class::External,
        Class::Internal(tag) => t.class == Class::Internal(tag.clone()),
        Class::Mixed => false,
    };
    if !class_ok {
        bad.push(format!(
            "{} 的 TS 形制是 {}，与 Rust 侧 {} 不对合（D-41 D1/D2 的断裂类）",
            t.where_(),
            t.class.label(),
            r.class.label()
        ));
    }
    if want == Class::Untagged {
        // untagged 两侧都无判别键可比，臂数对合是最低可用判据
        if r.vars.len() != t.members {
            bad.push(format!(
                "{} 有 {} 个臂，{} 有 {} 个成员（untagged 臂数错位）",
                r.name,
                r.vars.len(),
                t.name,
                t.members
            ));
        }
        return bad;
    }
    let rl = r.lits();
    if p.subset {
        if !t.lits.is_subset(&rl) {
            bad.push(format!(
                "{} 的字面量 {:?} 不是 {} 的子集",
                p.ts, t.lits, r.name
            ));
        }
    } else if rl != t.lits {
        bad.push(format!(
            "{} 线上字面量集合 {:?} 与 {} 的 {:?} 不等（改一半忘一半）",
            r.where_(),
            rl,
            t.where_(),
            t.lits
        ));
    }
    // unit 集合对合：摊平形（interface）与 subset 档不可判，不比
    if !p.subset && !t.flattened {
        let ru = r.units();
        if ru != t.units {
            bad.push(format!(
                "{} 的 unit 臂 {:?} 与 {} 的 {:?} 不等（裸串/包形错位）",
                r.where_(),
                ru,
                t.where_(),
                t.units
            ));
        }
    }
    bad
}

// ============================ 测试 ============================

#[test]
fn term_ssh_auth_pair_is_internal_kind() {
    let rust = rust_enums();
    let ts = ts_items();
    let r = find_rust(&rust, "SshAuth").expect("term-core 的 SshAuth 必须被扫到");
    let t = find_ts(&ts, "SshAuthDto").expect("client.ts 的 SshAuthDto 必须被扫到");
    assert_eq!(
        r.class,
        Class::Internal("kind".to_string()),
        "SshAuth 应为内部 tag=kind"
    );
    assert_eq!(
        t.class,
        Class::Internal("kind".to_string()),
        "SshAuthDto 应为 kind 判别臂——D-41 D1 曾写成外部标签形"
    );
    let want: BTreeSet<String> = ["password", "key"].into_iter().map(String::from).collect();
    assert_eq!(r.lits(), want);
    assert_eq!(t.lits, want);
}

#[test]
fn term_kind_pair_is_internal_kind() {
    let rust = rust_enums();
    let ts = ts_items();
    let r = find_rust(&rust, "TermKind").expect("term-core 的 TermKind 必须被扫到");
    let t = find_ts(&ts, "TermKindDto").expect("client.ts 的 TermKindDto 必须被扫到");
    assert_eq!(r.class, Class::Internal("kind".to_string()));
    assert_eq!(
        t.class,
        Class::Internal("kind".to_string()),
        "TermKindDto 应为 kind 判别臂——D-41 D2 曾写成外部包形"
    );
    let want: BTreeSet<String> = ["local", "wsl", "ssh"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(r.lits(), want);
    assert_eq!(t.lits, want);
}

#[test]
fn ledger_is_fully_hitted() {
    let rust = rust_enums();
    let ts = ts_items();
    let mut fails = Vec::new();
    for p in PAIRS {
        let r = find_rust(&rust, p.rust);
        let t = find_ts(&ts, p.ts);
        match (r, t) {
            (Some(r), Some(t)) => fails.extend(
                check_pair(p, &r, &t)
                    .into_iter()
                    .map(|m| format!("{} ↔ {}：{m}", p.rust, p.ts)),
            ),
            (None, _) => fails.push(format!("账本过期：Rust 侧找不到 {}", p.rust)),
            (_, None) => fails.push(format!("账本过期：TS 侧找不到 {}", p.ts)),
        }
    }
    for ro in RUST_ONLY {
        match find_rust(&rust, ro.rust) {
            None => fails.push(format!("RUST_ONLY 过期：{} 已不在扫描面", ro.rust)),
            Some(r) => {
                if r.file != ro.rust_file {
                    fails.push(format!(
                        "RUST_ONLY {} 实际在 {}，登记 {}",
                        ro.rust, r.file, ro.rust_file
                    ));
                }
                if ro.note.is_empty() {
                    fails.push(format!("RUST_ONLY {} 缺 note", ro.rust));
                }
            }
        }
    }
    for to in TS_ONLY {
        match find_ts(&ts, to.ts) {
            None => fails.push(format!("TS_ONLY 过期：{} 已不在扫描面", to.ts)),
            Some(t) => {
                if !t.stmt.contains(to.ts_anchor) {
                    fails.push(format!(
                        "TS_ONLY {} 找不到锚点 {:?}（TS 形制被改写而未记正）",
                        to.ts, to.ts_anchor
                    ));
                }
                if to.note.is_empty() {
                    fails.push(format!("TS_ONLY {} 缺 note", to.ts));
                }
            }
        }
    }
    assert!(
        fails.is_empty(),
        "serde 形制账本对合失败 {} 条：\n{}",
        fails.len(),
        fails.join("\n")
    );
}

#[test]
fn no_unregistered_union_escapes_scan() {
    let rust = rust_enums();
    let ts = ts_items();
    let mut fails = Vec::new();
    for e in &rust {
        if !PAIRS.iter().any(|p| p.rust == e.name) && !RUST_ONLY.iter().any(|r| r.rust == e.name) {
            fails.push(format!("未登记的 Rust 判别枚举：{}", e.where_()));
        }
    }
    for t in &ts {
        if !PAIRS.iter().any(|p| p.ts == t.name) && !TS_ONLY.iter().any(|x| x.ts == t.name) {
            fails.push(format!("未登记的 TS 判别联合：{}", t.where_()));
        }
    }
    assert!(
        fails.is_empty(),
        "新增形制未记账（增量漂移自爆）{} 条：\n{}",
        fails.len(),
        fails.join("\n")
    );
}

#[test]
fn enumerated_pairs_meet_floor() {
    let rust = rust_enums();
    let ts = ts_items();
    // 正对照地板：扫描脱靶（零命中假绿）在这里自曝，而不是让账本空转
    assert!(
        PAIRS.len() >= 25,
        "配对账本条目 {} < 25：账本被成批删除？",
        PAIRS.len()
    );
    assert!(
        rust.len() >= 40,
        "Rust 判别枚举检出 {} 枚 < 40：扫描腿或目录形状变了",
        rust.len()
    );
    assert!(
        ts.len() >= 15,
        "TS 判别联合检出 {} 枚 < 15：client.ts 扫描腿脱靶",
        ts.len()
    );
    // 重复登记=账本自相矛盾
    let pair_rust: BTreeSet<&str> = PAIRS.iter().map(|p| p.rust).collect();
    assert_eq!(pair_rust.len(), PAIRS.len(), "PAIRS 里有重复的 Rust 枚举名");
    let pair_ts: BTreeSet<&str> = PAIRS.iter().map(|p| p.ts).collect();
    assert_eq!(pair_ts.len(), PAIRS.len(), "PAIRS 里有重复的 TS 类型名");
    // 同一对象不得既记配对又记单侧
    for r in RUST_ONLY {
        assert!(
            !pair_rust.contains(r.rust),
            "{} 同时在 PAIRS 与 RUST_ONLY",
            r.rust
        );
    }
    for x in TS_ONLY {
        assert!(!pair_ts.contains(x.ts), "{} 同时在 PAIRS 与 TS_ONLY", x.ts);
    }
}
