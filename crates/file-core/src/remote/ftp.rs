//! FTP 明文驱动（09 §6.2 T-B6-6）：**零新依赖**，只用 `std::net::TcpStream` +
//! `std::io`（自写 RFC 959/3659 最小子集：USER/PASS、TYPE I、PASV、MLSD、MKD、
//! DELE/RMD、RNFR/RNTO）。
//!
//! 红线（本文件是判据落点，改码前先读）：
//! - 明文总闸：非回环 FTP 连接必须先过 [`ftp_plaintext_guard`]——
//!   `insecure_plaintext=false` 时以 FILE_REMOTE_006 拒绝并点名"未授权明文传输"；
//!   存管（VaultEntry）/会话（SessionPassword）口令在明文链路上**无条件**拒（007）：
//!   "只进不出"纪律在明文信道上根本不该被触发，回环也不给这两型开闸。
//! - 口令只进 PASS 命令行，错误消息与日志永不回显（`只进不出` 的行为面）；
//!   一切过网参数（user/口令/路径）先过控制字符裁决——CRLF 命令注入面。
//! - PASV 数据通道 IP 必须等于控制端 IP（[`parse_pasv`] 的 `control_peer` 红线）：
//!   恶意/被劫持的服务器用 `227 (a,b,c,d,..)` 把客户端变成内网端口扫描器，
//!   这是 FTP 客户端最经典的攻击面，回显外站 IP 一律拒。
//! - 无事实源就无时间：MLSD `modify` 缺失或畸形 ⇒ `modified_ms = 0`（与
//!   webdav `parse_http_date_ms` 同源纪律，天数换算共用 `days_from_civil`）。
//! - 传输模式恒 `TYPE I`（[`plan_ascii_or_binary`] 恒回 Binary）：ASCII 模式
//!   改写 CRLF，字节保真与哈希校验都不成立。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpStream};
use std::time::Duration;

use crate::error::{
    FileError, FILE_REMOTE_FIELD, FILE_REMOTE_MISSING, FILE_REMOTE_PLAINTEXT,
    FILE_REMOTE_PLAIN_AUTH, FILE_REMOTE_PLAIN_CONFIRM,
};
use crate::profile::{looks_like_loopback, AuthKind, RemoteProfile};
use crate::remote::webdav::{days_from_civil, percent_decode};
use crate::remote::{AuthSecret, RemoteEntry};

/// 控制连接读超时：明文协议无 multiplexing，宁可失败重发
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
/// 单次列表/传输数据的硬顶（防对端用无限流打爆内存）
const MAX_DATA_BYTES: u64 = 16 * 1024 * 1024;

/// 运行期/协议错误恒出自 FILE_REMOTE_001 的语义扩张（与 T-B6-5 落地补记③同谱：
/// 本批码表固定，协议腿运行时失败共用 001，消息点名成因）。
fn ftp_err(msg: String) -> FileError {
    FileError::Remote {
        code: FILE_REMOTE_MISSING,
        msg,
    }
}

// ---------------------------------------------------------------------------
// 控制应答解析（纯函数，桩与真机共用）
// ---------------------------------------------------------------------------

/// 一条完整控制应答（单行或多行）：code + 各行文本（含首行 code 后的残余）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FtpReply {
    pub code: u16,
    pub text: Vec<String>,
}

impl FtpReply {
    /// 错误消息展示形（多行合一；永不含口令——口令不进 text 的构造侧由
    /// 发送面红线索引，见 [`redact_command`]）
    pub fn display(&self) -> String {
        self.text.join(" / ")
    }
}

/// `(三位码, 分隔符)` 头裁决：分隔符只认 ` `（末行/单行）与 `-`（多行续）
fn reply_head(line: &str) -> Option<(u16, char)> {
    let b = line.as_bytes();
    if b.len() < 4 || !b[0].is_ascii_digit() || !b[1].is_ascii_digit() || !b[2].is_ascii_digit() {
        return None;
    }
    let c = line[3..].chars().next()?;
    if c == ' ' || c == '-' {
        let code = line[..3].parse().ok()?;
        return Some((code, c));
    }
    None
}

fn is_final_line(line: &str, code: u16) -> bool {
    line.starts_with(&format!("{code} "))
}

/// 解析一条完整应答（可能多行，`\n` 分隔，`\r` 已剥）。三臂：
/// - `123 text` 单行；
/// - `123-首行\n…\n123 末行` 多行（RFC 959 §4.2：终止行 = 同码 + 空格）；
/// - 畸形（非三位码、非法分隔符、多行未终止、单行后还挂残行）→ Err 点名原样。
pub fn parse_control_line(s: &str) -> Result<FtpReply, FileError> {
    // 契约自兜：调用方给 `CRLF` 串也守得住——`\r` 只属于线尾，不进正文
    let s = s.replace("\r\n", "\n");
    let mut lines = s.split('\n');
    let first = lines.next().unwrap_or("");
    let Some((code, delim)) = reply_head(first) else {
        return Err(ftp_err(format!("FTP 应答首行畸形: {first:?}")));
    };
    let rest = &first[4..];
    if delim == ' ' {
        let leftover: Vec<&str> = lines.filter(|l| !l.is_empty()).collect();
        if !leftover.is_empty() {
            return Err(ftp_err(format!(
                "FTP 单行应答后挂残行（协议失步）: {:?}",
                leftover[0]
            )));
        }
        return Ok(FtpReply {
            code,
            text: vec![rest.to_owned()],
        });
    }
    let mut text = vec![rest.to_owned()];
    let mut terminated = false;
    for line in lines {
        if is_final_line(line, code) {
            text.push(line[4..].to_owned());
            terminated = true;
            break;
        }
        text.push(line.to_owned());
    }
    if !terminated {
        return Err(ftp_err(format!(
            "FTP 多行应答未收到终止行 {code}<SP>: {:?}",
            text.first()
        )));
    }
    Ok(FtpReply { code, text })
}

// ---------------------------------------------------------------------------
// PASV 裁决（数据通道 IP 红线）
// ---------------------------------------------------------------------------

/// 解析 227 应答的 `(h1,h2,h3,h4,p1,p2)` → 数据通道地址。
/// `control_peer` 是控制连接对端 IP（调用方从 socket 拿的事实源）：
/// 回显的 IPv4 与之不符 ⇒ 拒（内网扫描/重定向红线），并在消息里点名两个 IP。
/// 偏离任务书签名（`parse_pasv(&str)`）登记为落地补记：外泄 IP 红线需要
/// 控制端事实源，纯字符串口做不到。
pub fn parse_pasv(s: &str, control_peer: IpAddr) -> Result<(IpAddr, u16), FileError> {
    let inner = s
        .split_once('(')
        .and_then(|(_, r)| r.split_once(')'))
        .map_or("", |(l, _)| l);
    let nums: Vec<u32> = inner
        .split(',')
        .map(|x| x.trim().parse().unwrap_or(u32::MAX))
        .collect();
    if nums.len() != 6 || nums.iter().any(|n| *n > 255) {
        return Err(ftp_err(format!("FTP PASV 应答畸形（无合法六元组）: {s:?}")));
    }
    let data_ip = IpAddr::from([nums[0] as u8, nums[1] as u8, nums[2] as u8, nums[3] as u8]);
    let port = (nums[4] as u16) * 256 + nums[5] as u16;
    if port == 0 {
        return Err(ftp_err(format!("FTP PASV 回显端口 0: {s:?}")));
    }
    if data_ip != control_peer {
        return Err(ftp_err(format!(
            "FTP PASV 数据通道 IP {data_ip} 与控制端 {control_peer} 不一致——拒连（防被用作内网端口扫描器）"
        )));
    }
    Ok((data_ip, port))
}

// ---------------------------------------------------------------------------
// MLSD 解析（RFC 3659）
// ---------------------------------------------------------------------------

/// `modify` 事实 `YYYYMMDDhhmmss(.xx)` → 毫秒；任何不合格式回 0——
/// 无事实源就无时间（与 webdav `parse_http_date_ms` 同纪律）
fn mlsd_time_ms(v: &str) -> i64 {
    let digits = v.split('.').next().unwrap_or("");
    let b = digits.as_bytes();
    if b.len() != 14 || !b.iter().all(|c| c.is_ascii_digit()) {
        return 0;
    }
    let parse = |r: std::ops::Range<usize>| digits[r].parse::<i64>().unwrap_or(-1);
    let (y, mo, d) = (parse(0..4), parse(4..6), parse(6..8));
    let (hh, mm, ss) = (parse(8..10), parse(10..12), parse(12..14));
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return 0;
    }
    if hh > 23 || mm > 59 || ss > 60 {
        return 0;
    }
    (days_from_civil(y, mo, d) * 86400 + hh * 3600 + mm * 60 + ss) * 1000
}

/// MLSD 数据流（`\r\n` 分行，每行 `fact;fact;… 条目名`）→ 条目表。
/// `type` 缺失 ⇒ 整行判畸形 Err（静默丢条目 = 假空表，红线）；
/// `cdir`/`pdir` 按 RFC 跳过；`size`/`modify` 缺失 ⇒ 0（无事实源）。
/// 条目名按 RFC 3659 的 %XX 转义解码（MLSD 里空格等控制性字符是转义形，
/// 命令参数用解码后的字面名——与 HTTP 的 URL 语义相反，见 `join_ftp_path`）。
pub fn parse_mlsd(s: &str) -> Result<Vec<RemoteEntry>, FileError> {
    let mut out = Vec::new();
    for raw in s.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        let Some((facts, name)) = raw.split_once(' ') else {
            return Err(ftp_err(format!("MLSD 行缺少条目名分隔空格: {raw:?}")));
        };
        let mut ty: Option<String> = None;
        let mut size = 0u64;
        let mut modified_ms = 0i64;
        for fact in facts.split(';') {
            if fact.is_empty() {
                continue;
            }
            let Some((k, v)) = fact.split_once('=') else {
                return Err(ftp_err(format!(
                    "MLSD 事实源缺 '='（畸形条目，整行拒）: {raw:?}"
                )));
            };
            match k.to_ascii_lowercase().as_str() {
                "type" => ty = Some(v.to_ascii_lowercase()),
                "size" => size = v.trim().parse().unwrap_or(0),
                "modify" => modified_ms = mlsd_time_ms(v.trim()),
                _ => {}
            }
        }
        let Some(t) = ty else {
            return Err(ftp_err(format!("MLSD 条目缺 type 事实源: {raw:?}")));
        };
        if t == "cdir" || t == "pdir" {
            continue;
        }
        let name = percent_decode(name.trim_end_matches("\r"));
        if name.is_empty() || name.chars().any(|c| c.is_control()) {
            return Err(ftp_err(format!("MLSD 条目名非法: {raw:?}")));
        }
        out.push(RemoteEntry {
            is_dir: t == "dir",
            size,
            modified_ms,
            name: name.clone(),
            // path 由驱动侧接上请求目录；纯函数只给字面名
            path: name,
            // MLSD 事实只有 type/size/mtime——权限位无源即 None（LIST 的 Unix
            // 模式串不解析：无标准，解析=猜）
            mode: None,
            symlink_target: None,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 路径拼接（FTP 参数是字面名不是 URL——与 join_remote_url 的重编码语义相反）
// ---------------------------------------------------------------------------

/// 唯一 FTP 拼接口：`/` 分隔、去重斜杠、`.` 丢弃、`..` 弹栈不越根（先 % 解码
/// 再裁决，`%2e%2e` 同样弹栈）；段以**字面形**保留（不重编码——服务端命令参数
/// 吃的是解码名）。偏离登记：任务书只点名 parse 三件套，本口是 FTP 语义必需。
pub fn join_ftp_path(base: &str, path: &str) -> String {
    let mut segs: Vec<String> = Vec::new();
    for src in [base, path] {
        for raw in src.split('/') {
            match percent_decode(raw).as_str() {
                "" | "." => {}
                ".." => {
                    segs.pop();
                }
                other => segs.push(other.to_owned()),
            }
        }
    }
    if segs.is_empty() {
        return "/".to_owned();
    }
    format!("/{}", segs.join("/"))
}

// ---------------------------------------------------------------------------
// 传输模式裁决（红线：恒二进制）
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FtpMode {
    Ascii,
    Binary,
}

impl FtpMode {
    pub fn command(self) -> &'static str {
        match self {
            FtpMode::Ascii => "TYPE A",
            FtpMode::Binary => "TYPE I",
        }
    }
}

/// 模式规划口：无论什么名字（连 `.txt` 也不）恒 Binary——字节保真与
/// 断点/哈希语义只在 image 模式下成立；ASCII 的 CRLF 改写是数据损坏。
pub fn plan_ascii_or_binary(_name_hint: &str) -> FtpMode {
    FtpMode::Binary
}

// ---------------------------------------------------------------------------
// 明文总闸（纯裁决口，service::connect 在建驱动前调用）
// ---------------------------------------------------------------------------

/// FTP 明文链路三闸裁决（09 §6.2 T-B6-6）：
/// ① 非明文档直接放行（本闸只管 FTP）；
/// ② 存管/会话口令在明文链路上**无条件拒**（007）——即便总闸开着、即便回环；
///    SshKey 配 FTP 是认证形状错（005）；
/// ③ 回环放行（不过网，联调与测试桩）；非回环看 `insecure_plaintext` 总闸，
///    未开 ⇒ 006 点名"未授权明文传输"。
pub fn ftp_plaintext_guard(
    profile: &RemoteProfile,
    insecure_plaintext: bool,
) -> Result<(), FileError> {
    if !profile.protocol.is_plaintext() {
        return Ok(());
    }
    match &profile.auth {
        AuthKind::SessionPassword | AuthKind::VaultEntry { .. } => {
            return Err(FileError::Remote {
                code: FILE_REMOTE_PLAIN_AUTH,
                msg: format!(
                    "明文链路拒收存管/会话口令：档案 {:?} 的 {} 认证要把口令送上未经 TLS 的 FTP 控制连接，本闸不给总闸开豁免（码 FILE_REMOTE_007）",
                    profile.id,
                    match &profile.auth {
                        AuthKind::VaultEntry { .. } => "VaultEntry",
                        _ => "SessionPassword",
                    }
                ),
            })
        }
        AuthKind::SshKey { .. } => {
            return Err(FileError::Remote {
                code: FILE_REMOTE_FIELD,
                msg: format!(
                    "档案 {:?} 是 FTP 腿却配了 SSH 密钥认证：请改用 SFTP 档案（码 FILE_REMOTE_005）",
                    profile.id
                ),
            })
        }
        AuthKind::Anonymous | AuthKind::PromptEachTime => {}
    }
    if looks_like_loopback(&profile.host) {
        return Ok(());
    }
    if insecure_plaintext {
        return Ok(());
    }
    Err(FileError::Remote {
        code: FILE_REMOTE_PLAINTEXT,
        msg: format!(
            "未授权明文传输：档案 {:?} 走 FTP（明文过网），需先在设置中打开 insecure_plaintext 总闸（码 FILE_REMOTE_006；回环联调不受此闸约束）",
            profile.id
        ),
    })
}

/// 明文三闸的第三闸（T-B6-8，09 §6.2）：非回环明文连接的**逐次确认**——
/// 总闸（006）是"这类链路允许存在"的用户决定，本闸是"这一次连接"的用户明示。
/// 无记忆是设计而非缺陷：每次连接都重新过手，不存在"记住这次决定"的档位
/// （面板复述目标地址后经 `confirmAction` 取得明示，再带参数重试）。
/// 回环豁免与 [`ftp_plaintext_guard`] 同口径；非明文的三协议恒 `Ok`（本闸
/// 只管明文腿，SFTP 的对应物是 TOFU，HTTPS/WebDAV 恒 TLS——WebDAV 非回环
/// 只有 https 形制，见 webdav.rs 的 scheme 裁决臂）。
pub fn ftp_confirm_gate(
    profile: &RemoteProfile,
    allow_plaintext_once: bool,
) -> Result<(), FileError> {
    if !profile.protocol.is_plaintext() || looks_like_loopback(&profile.host) {
        return Ok(());
    }
    if allow_plaintext_once {
        return Ok(());
    }
    Err(FileError::Remote {
        code: FILE_REMOTE_PLAIN_CONFIRM,
        msg: format!(
            "明文连接需用户明示（逐次）：档案 {:?} 到 {}:{} 的 FTP 控制连接是非回环明文，\
             本次连接未携带确认参数，已在建连之前拒绝（码 FILE_REMOTE_008）。请面板复述目标\
             地址取得用户明示后重试；本闸每次连接独立生效，不存在一次确认永久放行的出路",
            profile.id, profile.host, profile.port
        ),
    })
}

// ---------------------------------------------------------------------------
// 会话（每操作一条控制连接：最小子集不做连接池——明文长连不做常驻承诺）
// ---------------------------------------------------------------------------

struct Session {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    peer_ip: IpAddr,
}

/// 过网参数红线：拒绝一切控制字符（CRLF 命令注入面）
fn sanitize_arg(s: &str, label: &str) -> Result<(), FileError> {
    if s.is_empty() {
        return Err(ftp_err(format!("FTP {label} 为空，无法登录")));
    }
    if s.chars().any(|c| c.is_control()) {
        return Err(ftp_err(format!(
            "FTP {label} 含控制字符，拒绝过网（CRLF 注入面）"
        )));
    }
    Ok(())
}

/// 错误/日志面的命令回显裁决：PASS 行的内容一律退役为 `<redacted>`——
/// 口令只进不出在 FTP 错误消息面上的落点。
fn redact_command(cmd: &str) -> String {
    if cmd.starts_with("PASS") {
        "PASS <redacted>".to_owned()
    } else {
        cmd.to_owned()
    }
}

impl Session {
    fn open(profile: &RemoteProfile, secret: Option<&AuthSecret>) -> Result<Self, FileError> {
        let stream = TcpStream::connect((profile.host.as_str(), profile.port)).map_err(|e| {
            ftp_err(format!(
                "FTP 控制连接失败（{}:{}）: {e}",
                profile.host, profile.port
            ))
        })?;
        let _ = stream.set_read_timeout(Some(CONTROL_TIMEOUT));
        let peer_ip = stream
            .peer_addr()
            .map_err(|e| ftp_err(format!("FTP 取控制端地址失败: {e}")))?
            .ip();
        let writer = stream
            .try_clone()
            .map_err(|e| ftp_err(format!("FTP 复制控制连接失败: {e}")))?;
        let mut s = Self {
            reader: BufReader::new(stream),
            writer,
            peer_ip,
        };
        let greet = s.read_reply()?;
        if greet.code != 220 {
            return Err(ftp_err(format!(
                "FTP 服务问候非 220: {} {}",
                greet.code,
                greet.display()
            )));
        }
        sanitize_arg(&profile.user, "用户名")?;
        let login = s.command(&format!("USER {}", profile.user), &[230, 331])?;
        if login.code == 331 {
            let pw = secret.and_then(|x| x.password.as_ref()).ok_or_else(|| {
                ftp_err(format!(
                    "FTP 服务器要求口令但本次连接未提供（{}），登录未完成，禁假就绪",
                    profile.id
                ))
            })?;
            sanitize_arg(pw.as_str(), "口令")?;
            s.command(&format!("PASS {}", pw.as_str()), &[230])?;
        }
        Ok(s)
    }

    fn send(&mut self, cmd: &str) -> Result<(), FileError> {
        self.writer
            .write_all(format!("{cmd}\r\n").as_bytes())
            .map_err(|e| ftp_err(format!("FTP 发送 {} 失败: {e}", redact_command(cmd))))
    }

    fn read_reply(&mut self) -> Result<FtpReply, FileError> {
        let mut lines: Vec<String> = Vec::new();
        let mut need_final: Option<u16> = None;
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .map_err(|e| ftp_err(format!("FTP 读应答失败: {e}")))?;
            if n == 0 {
                return Err(ftp_err("FTP 控制连接被对端关闭".into()));
            }
            let line = line.trim_end_matches(['\r', '\n']).to_owned();
            if let Some(code) = need_final {
                if is_final_line(&line, code) {
                    lines.push(line);
                    break;
                }
                lines.push(line);
                if lines.len() > 64 {
                    return Err(ftp_err("FTP 多行应答超长（>64 行未终止），判畸形".into()));
                }
                continue;
            }
            match reply_head(&line) {
                Some((_, ' ')) => {
                    lines.push(line);
                    break;
                }
                Some((code, '-')) => {
                    need_final = Some(code);
                    lines.push(line);
                }
                _ => {
                    return Err(ftp_err(format!("FTP 应答首行畸形: {line:?}")));
                }
            }
        }
        parse_control_line(&lines.join("\n"))
    }

    fn command(&mut self, cmd: &str, expect: &[u16]) -> Result<FtpReply, FileError> {
        self.send(cmd)?;
        let reply = self.read_reply()?;
        if !expect.contains(&reply.code) {
            return Err(ftp_err(format!(
                "FTP {} 期望 {expect:?}，对端回 {}: {}",
                redact_command(cmd),
                reply.code,
                reply.display()
            )));
        }
        Ok(reply)
    }

    /// 被动数据通道上跑一条传输命令（150/125 → 读尽 → 22x 收尾）
    fn transfer<R>(
        &mut self,
        cmd: &str,
        use_: impl FnOnce(&mut TcpStream) -> Result<R, FileError>,
    ) -> Result<R, FileError> {
        self.transfer_at(cmd, None, use_)
    }

    /// [`transfer`](Self::transfer) 加 REST 前置臂（T-B6-11）：`rest = Some(n)`
    /// 时在数据通道就绪后、传输命令前发 `REST n`，**必须**收到 350 才继续——
    /// 对端不应 Range 就 Err 点名，禁静默从头给字节。
    fn transfer_at<R>(
        &mut self,
        cmd: &str,
        rest: Option<u64>,
        use_: impl FnOnce(&mut TcpStream) -> Result<R, FileError>,
    ) -> Result<R, FileError> {
        let pasv = self.command("PASV", &[227])?;
        let (ip, port) = parse_pasv(&pasv.display(), self.peer_ip)?;
        let mut data = TcpStream::connect((ip, port))
            .map_err(|e| ftp_err(format!("FTP 数据通道连接失败（{ip}:{port}）: {e}")))?;
        let _ = data.set_read_timeout(Some(CONTROL_TIMEOUT));
        let _ = data.set_write_timeout(Some(CONTROL_TIMEOUT));
        if let Some(n) = rest {
            self.command(&format!("REST {n}"), &[350])?;
        }
        self.command(cmd, &[125, 150])?;
        let out = use_(&mut data)?;
        drop(data);
        let done = self.read_reply()?;
        if !(done.code == 226 || done.code == 250) {
            return Err(ftp_err(format!(
                "FTP 传输收尾非 226/250: {} {}",
                done.code,
                done.display()
            )));
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// 驱动
// ---------------------------------------------------------------------------

/// FTP 驱动：惰建连（`new` 零网络——connect 分派口只登记事实，握手推迟到
/// 第一条操作；总闸测试因此不需真服务器）
pub struct FtpDriver {
    profile: RemoteProfile,
    secret: Option<AuthSecret>,
}

impl FtpDriver {
    pub(crate) fn new(profile: RemoteProfile, secret: Option<AuthSecret>) -> Self {
        Self { profile, secret }
    }

    pub(crate) fn profile(&self) -> &RemoteProfile {
        &self.profile
    }
    pub(crate) fn driver_label(&self) -> String {
        format!("FTP {}", self.profile.label)
    }
    pub(crate) fn base_path(&self) -> String {
        join_ftp_path(&self.profile.base_path, "")
    }

    /// 绝对入参 = 原样规范化（防穿越唯一口）；相对入参挂 base_path；
    /// 拼出的字面路径再过一次控制字符闸（CRLF 注入面）
    fn server_path(&self, remote_path: &str) -> Result<String, FileError> {
        let joined = if remote_path.starts_with('/') {
            join_ftp_path("", remote_path)
        } else {
            join_ftp_path(&self.profile.base_path, remote_path)
        };
        if joined.chars().any(|c| c.is_control()) {
            return Err(ftp_err(
                "FTP 路径含控制字符，拒绝过网（CRLF 注入面）".into(),
            ));
        }
        Ok(joined)
    }

    fn session(&self) -> Result<Session, FileError> {
        Session::open(&self.profile, self.secret.as_ref())
    }

    pub fn list_entries(&self, path: &str) -> Result<Vec<RemoteEntry>, FileError> {
        let dir = self.server_path(path)?;
        let mut s = self.session()?;
        s.command(plan_ascii_or_binary("").command(), &[200, 250])?;
        let lines = s.transfer(&format!("MLSD {dir}"), |data| {
            let mut buf = Vec::new();
            std::io::Read::by_ref(data)
                .take(MAX_DATA_BYTES)
                .read_to_end(&mut buf)
                .map_err(|e| ftp_err(format!("FTP MLSD 数据读取失败: {e}")))?;
            Ok(buf)
        })?;
        let entries = parse_mlsd(&String::from_utf8_lossy(&lines))?;
        Ok(entries
            .into_iter()
            .map(|mut e| {
                e.path = join_ftp_path(&dir, &e.name);
                e
            })
            .collect())
    }

    pub fn mkdir_remote(&self, path: &str) -> Result<(), FileError> {
        let dir = self.server_path(path)?;
        let mut s = self.session()?;
        s.command(&format!("MKD {dir}"), &[257])?;
        Ok(())
    }

    /// DELE 失败（服务器最常见回 550 因为那是个目录）→ 同会话补试 RMD；
    /// 两刀都被拒才报错，且消息带上 DELE 的原始裁决（诊断诚实）
    pub fn remove_remote(&self, path: &str) -> Result<(), FileError> {
        let p = self.server_path(path)?;
        let mut s = self.session()?;
        match s.command(&format!("DELE {p}"), &[250]) {
            Ok(_) => Ok(()),
            Err(dele) => {
                let dele_msg = dele.to_string();
                s.command(&format!("RMD {p}"), &[250])
                    .map_err(|rmd| {
                        ftp_err(format!(
                            "FTP 删除失败：DELE 与 RMD 均被拒（DELE: {dele_msg}；RMD: {rmd}）"
                        ))
                    })
                    .map(|_| ())
            }
        }
    }

    pub fn rename_remote(&self, from: &str, to: &str) -> Result<(), FileError> {
        let f = self.server_path(from)?;
        let t = self.server_path(to)?;
        let mut s = self.session()?;
        s.command(&format!("RNFR {f}"), &[350])?;
        s.command(&format!("RNTO {t}"), &[250])?;
        Ok(())
    }

    // ---- T-B6-11 字节腿（队列远端执行器经 trait 消费）----

    /// spool 读腿：RETR（`offset>0` 时 REST 真验身 350）→ 数据段整体过
    /// 226 收尾校验后，读柄才交出去——谎报 EOF 在此结构性不可能。
    pub(crate) fn read_spool(
        &self,
        remote_path: &str,
        offset: u64,
    ) -> Result<super::SpoolReader, FileError> {
        let p = self.server_path(remote_path)?;
        let spool_path = super::reserve_spool_path("ftp");
        {
            let mut s = self.session()?;
            s.command(plan_ascii_or_binary(&p).command(), &[200, 250])?;
            let dst = spool_path.clone();
            s.transfer_at(
                &format!("RETR {p}"),
                (offset > 0).then_some(offset),
                |data| {
                    let mut f = std::fs::File::create(&dst)?;
                    let n = std::io::copy(data, &mut f)?;
                    Ok(n)
                },
            )?;
        }
        super::SpoolReader::open(spool_path.clone()).inspect_err(|_| {
            super::SpoolReader::discard(&spool_path);
        })
    }

    /// 流式写腿：STOR 的数据通道直接活在写柄里（finish 关闭数据段后
    /// 收 226 校验——泵完字节 ≠ 传成）；上传无断点：STOR 恒从 0。
    pub(crate) fn stor_writer(
        &self,
        remote_path: &str,
    ) -> Result<Box<dyn host_core::storage::WriteCommit>, FileError> {
        let p = self.server_path(remote_path)?;
        let mut s = self.session()?;
        s.command(plan_ascii_or_binary(&p).command(), &[200, 250])?;
        let pasv = s.command("PASV", &[227])?;
        let (ip, port) = parse_pasv(&pasv.display(), s.peer_ip)?;
        let data = TcpStream::connect((ip, port))
            .map_err(|e| ftp_err(format!("FTP 数据通道连接失败（{ip}:{port}）: {e}")))?;
        let _ = data.set_write_timeout(Some(CONTROL_TIMEOUT));
        s.command(&format!("STOR {p}"), &[125, 150])?;
        Ok(Box::new(FtpStorWriter {
            data: Some(data),
            session: s,
            remote: p,
        }))
    }
}

struct FtpStorWriter {
    data: Option<TcpStream>,
    session: Session,
    remote: String,
}

impl std::io::Write for FtpStorWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match &mut self.data {
            Some(d) => d.write(buf),
            None => Err(std::io::Error::other("FTP STOR 数据通道已关闭")),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.data {
            Some(d) => d.flush(),
            None => Ok(()),
        }
    }
}

impl host_core::storage::WriteCommit for FtpStorWriter {
    fn finish(mut self: Box<Self>) -> Result<(), host_core::error::AppError> {
        if let Some(mut d) = self.data.take() {
            let _ = d.flush();
            drop(d); // 关数据段，服务器随即在控制段回终结应答
        }
        let done = self
            .session
            .read_reply()
            .map_err(host_core::error::AppError::from)?;
        let _ = self.session.send("QUIT");
        if done.code != 226 && done.code != 250 {
            return Err(host_core::error::AppError::from(ftp_err(format!(
                "FTP STOR 收尾非 226/250（{}）: {} {}",
                self.remote,
                done.code,
                done.display()
            ))));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{profile_id_of, RemoteProtocol};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use zeroize::Zeroizing;

    fn profile_for(host: &str, port: u16, auth: AuthKind) -> RemoteProfile {
        RemoteProfile {
            id: profile_id_of("t6stub"),
            label: "明文联调站".into(),
            protocol: RemoteProtocol::Ftp,
            host: host.into(),
            port,
            user: "me".into(),
            base_path: "/pub".into(),
            auth,
            preset_id: None,
            last_used_ms: 0,
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-6）字面测试名优先于 rustc 命名惯例
    fn parseControlLine_multiAndMalformed() {
        // 单行臂
        let r = parse_control_line("230 Login OK.").unwrap();
        assert_eq!(r.code, 230);
        assert_eq!(r.text, vec!["Login OK.".to_owned()]);
        // 多行臂（RFC 959：终止行 = 同码 + 空格）
        let r =
            parse_control_line("150-Here is the list\r\nrow one\r\nrow two\r\n150 End").unwrap();
        assert_eq!(r.code, 150);
        assert_eq!(
            r.text,
            vec!["Here is the list", "row one", "row two", "End"]
        );
        // 畸形臂：非三位码 / 非法分隔符 / 多行未终止 / 单行后残行
        for bad in [
            "abc Not a code",
            "23",
            "230xOK",
            "150-never ends\nstill going",
        ] {
            assert!(
                parse_control_line(bad).is_err(),
                "畸形应答必须 Err: {bad:?}"
            );
        }
        assert!(parse_control_line("230 one line\nstray tail").is_err());
        assert!(parse_control_line("").is_err(), "空应答没有事实源");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-6）字面测试名优先于 rustc 命名惯例
    fn parsePasv_rejectsExternalIp_butAcceptsOrdinaryPassive() {
        let peer: IpAddr = "203.0.113.9".parse().unwrap();
        // 正对照：普通被动模式（数据 IP == 控制端）放行，端口 195*256+80
        let (ip, port) =
            parse_pasv("227 Entering Passive Mode (203,0,113,9,195,80).", peer).unwrap();
        assert_eq!(ip, peer);
        assert_eq!(port, 50000);
        // 红线臂：回显他站 IP（内网/外站都拒）——消息点名两枚 IP
        let e = parse_pasv("227 Entering Passive Mode (10,0,0,1,4,8).", peer).unwrap_err();
        let msg = e.to_string();
        assert!(
            msg.contains("10.0.0.1") && msg.contains("203.0.113.9"),
            "须点名数据 IP 与控制端两枚事实源，实得 {msg}"
        );
        // 形状畸形臂：非六元组 / 字节越界 / 端口 0
        for bad in [
            "227 (1,2,3).",
            "227 (300,0,113,9,195,80).",
            "227 (203,0,113,9,0,0).",
            "227 no parens here",
        ] {
            assert!(parse_pasv(bad, peer).is_err(), "PASV 畸形必须 Err: {bad:?}");
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-6）字面测试名优先于 rustc 命名惯例
    fn parseMlsd_timeAndSizeAndDirType() {
        let stream = "type=cdir;modify=20220615083015; .\r\n\
                      type=dir;modify=20220615083015; sub%20dir\r\n\
                      type=file;size=7;modify=20010203040506; notes.txt\r\n\
                      type=file; plain.dat\r\n";
        let got = parse_mlsd(stream).unwrap();
        assert_eq!(got.len(), 3, "cdir 按 RFC 跳过");
        assert!(got[0].is_dir);
        assert_eq!(got[0].name, "sub dir", "%XX 转义只解字面名");
        assert_eq!(
            got[0].modified_ms, 1_655_281_815_000,
            "YYYYMMDDhhmmss 与 webdav 同日同源（共用 days_from_civil）"
        );
        assert_eq!(got[0].path, "sub dir", "纯函数只给字面名，目录由驱动拼");
        assert_eq!(got[1].size, 7);
        assert_eq!(got[1].modified_ms, 981_173_106_000);
        assert!(!got[2].is_dir);
        assert_eq!(got[2].size, 0, "size 缺失 ⇒ 0（无事实源就无字节）");
        assert_eq!(got[2].modified_ms, 0, "modify 缺失 ⇒ 0（无事实源就无时间）");
        // 时间反直觉臂：畸形 modify 不得借尸还魂成"现在"
        assert_eq!(mlsd_time_ms("2022061508"), 0, "长度不足 14 判无时间");
        assert_eq!(mlsd_time_ms("20221332083015"), 0, "13 月 32 日不是事实源");
        // 整份裁决臂：缺 type / 解码后含控制字符的条目名 ⇒ Err（禁静默丢条目）
        assert!(parse_mlsd("size=5; nameless-facts.txt").is_err());
        assert!(parse_mlsd("type=file; bad%00name").is_err());
        assert!(
            parse_mlsd("type=file;novalue; x").is_err(),
            "事实源缺 '=' 拒"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-6）字面测试名优先于 rustc 命名惯例
    fn plainFtpRefusedUnlessInsecurePlaintextEnabled() {
        // 闸①：非明文档根本不进闸
        let mut p = profile_for("files.example.org", 21, AuthKind::Anonymous);
        p.protocol = RemoteProtocol::WebDav;
        assert!(ftp_plaintext_guard(&p, false).is_ok(), "webdav 不进明文闸");
        // 闸③·拒绝臂：false + 非回环 ⇒ 006 且点名"未授权明文传输"
        p.protocol = RemoteProtocol::Ftp;
        let e = ftp_plaintext_guard(&p, false).unwrap_err();
        assert!(
            matches!(&e, FileError::Remote { code, msg } if *code == FILE_REMOTE_PLAINTEXT
                && msg.contains("未授权明文传输")
                && msg.contains("insecure_plaintext")),
            "006 须点名成因与总闸名，实得 {e}"
        );
        // 闸③·放行臂：总闸开 → 放行（诚实告警由 UI 侧承担，闸只管裁决）
        assert!(ftp_plaintext_guard(&p, true).is_ok());
        // 闸③·回环臂：不过网，总闸关也放行
        let lb = profile_for("127.0.0.1", 2121, AuthKind::Anonymous);
        assert!(ftp_plaintext_guard(&lb, false).is_ok());
        // 闸②：存管/会话口令无条件拒（007）——回环 + 总闸开也救不了
        for auth in [
            AuthKind::SessionPassword,
            AuthKind::VaultEntry {
                entry_id: "x".into(),
            },
        ] {
            let q = profile_for("127.0.0.1", 21, auth.clone());
            let e = ftp_plaintext_guard(&q, true).unwrap_err();
            assert!(
                matches!(&e, FileError::Remote { code, msg } if *code == FILE_REMOTE_PLAIN_AUTH
                    && msg.contains("明文链路拒收存管/会话口令")),
                "007 臂须拒绝且自证理由，实得 {e}"
            );
        }
        // 形状错臂：SshKey 配 FTP → 005 指路 SFTP
        let q = profile_for(
            "127.0.0.1",
            21,
            AuthKind::SshKey {
                key_path: "C:/id".into(),
            },
        );
        let e = ftp_plaintext_guard(&q, true).unwrap_err();
        assert!(
            matches!(&e, FileError::Remote { code, msg } if *code == FILE_REMOTE_FIELD && msg.contains("SFTP")),
            "005 臂须指路 SFTP，实得 {e}"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 非任务书测名沿用本模块测名家族风格
    fn modePlan_isAlwaysBinary() {
        // 红线：连 .txt 也不进 ASCII 模式（CRLF 改写 = 数据损坏）
        assert_eq!(plan_ascii_or_binary("notes.txt"), FtpMode::Binary);
        assert_eq!(plan_ascii_or_binary("data.csv"), FtpMode::Binary);
        assert_eq!(plan_ascii_or_binary(""), FtpMode::Binary);
        assert_eq!(FtpMode::Binary.command(), "TYPE I");
    }

    #[test]
    #[allow(non_snake_case)] // 非任务书测名沿用本模块测名家族风格
    fn ftpPath_joinTraversalAndLiteralSegments() {
        assert_eq!(join_ftp_path("/pub", "docs//a.txt"), "/pub/docs/a.txt");
        assert_eq!(join_ftp_path("/pub/a", "/../b"), "/pub/b");
        assert_eq!(join_ftp_path("/", "/../../x"), "/x");
        // %2e%2e 编码穿越同样弹栈；字面段不重编码（FTP 参数吃解码名）
        assert_eq!(join_ftp_path("/pub", "%2e%2e/x"), "/x");
        assert_eq!(join_ftp_path("/pub", "a b"), "/pub/a b", "空格以字面形过网");
    }

    #[test]
    #[allow(non_snake_case)] // 非任务书测名沿用本模块测名家族风格
    fn redact_hidesPassPassword() {
        assert_eq!(redact_command("PASS sesame"), "PASS <redacted>");
        assert_eq!(redact_command("USER me"), "USER me");
    }

    // ---- 端到端：环回假 FTP 服务器（stub 断请求比断响应更能防协议漂移）----

    const MLSD_FIXTURE: &str = "type=dir;modify=20220615083015; sub%20dir\r\n\
                                 type=file;size=7;modify=20220615083015; file.txt\r\n";

    fn ftp_stub_conn(stream: TcpStream, tx: mpsc::Sender<String>) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;
        writer.write_all(b"220 NexusForge FTP stub\r\n")?;
        let mut data_listener: Option<TcpListener> = None;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let cmd = line.trim_end_matches(['\r', '\n']).to_owned();
            let _ = tx.send(cmd.clone());
            let (head, arg) = cmd.split_once(' ').unwrap_or((cmd.as_str(), ""));
            let resp: Vec<u8> = match head {
                "USER" => b"331 Password required\r\n".to_vec(),
                "PASS" if arg == "sesame" => b"230 Logged in\r\n".to_vec(),
                "PASS" => b"530 Not logged in\r\n".to_vec(),
                "TYPE" => b"200 Type set\r\n".to_vec(),
                "PASV" => {
                    let l = TcpListener::bind("127.0.0.1:0")?;
                    let (ip, port) = (l.local_addr()?.ip(), l.local_addr()?.port());
                    data_listener = Some(l);
                    let b = ip.to_string();
                    let oct: Vec<String> = b.split('.').map(str::to_owned).collect();
                    format!(
                        "227 Entering Passive Mode ({},{},{},{},{},{}).\r\n",
                        oct[0],
                        oct[1],
                        oct[2],
                        oct[3],
                        port >> 8,
                        port & 0xFF
                    )
                    .into_bytes()
                }
                "MLSD" => {
                    let mut out = b"150 Opening data connection\r\n".to_vec();
                    match data_listener.take().map(|l| l.accept()) {
                        Some(Ok((mut data, _))) => {
                            let _ = data.write_all(MLSD_FIXTURE.as_bytes());
                            let _ = data.flush();
                            drop(data);
                            out.extend(b"226 Transfer complete\r\n");
                        }
                        _ => out.extend(b"425 Can't open data connection\r\n"),
                    }
                    out
                }
                "MKD" => b"257 \"/pub/newdir\" created\r\n".to_vec(),
                // DELE 普通文件成功；目录回 550（stub 靠文件名区分两臂）
                "DELE" if arg.ends_with("dir") => b"550 This is a directory\r\n".to_vec(),
                "DELE" => b"250 Deleted\r\n".to_vec(),
                "RMD" => b"250 Directory removed\r\n".to_vec(),
                "RNFR" => b"350 Ready\r\n".to_vec(),
                "RNTO" => b"250 Renamed\r\n".to_vec(),
                "QUIT" => b"221 Bye\r\n".to_vec(),
                _ => b"500 Unknown command\r\n".to_vec(),
            };
            writer.write_all(&resp)?;
            writer.flush()?;
            if head == "QUIT" {
                return Ok(());
            }
        }
    }

    fn ftp_stub_serve(listener: TcpListener, tx: mpsc::Sender<String>) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = ftp_stub_conn(stream, tx) {
                    tracing::debug!("FTP stub 连接收尾: {e}");
                }
            });
        }
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-6）字面测试名延伸：明文腿端到端
    async fn ftpAgainstFakeServer_endToEnd() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || ftp_stub_serve(listener, tx));

        let drv = FtpDriver::new(
            profile_for("127.0.0.1", port, AuthKind::PromptEachTime),
            Some(AuthSecret {
                header: None,
                password: Some(Zeroizing::new("sesame".into())),
            }),
        );

        // ① 列目录：MLSD fixture 两条目，path 由驱动接上请求目录
        let entries = drv.list_entries("/pub").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "sub dir");
        assert!(entries[0].is_dir);
        assert_eq!(
            entries[0].path, "/pub/sub dir",
            "FTP path 是字面名（非 URL 编码形）"
        );
        assert_eq!(entries[1].size, 7);

        // ② 建目录 / ③ 删除（文件臂一次 DELE；目录臂 DELE→550→RMD 兜底）
        drv.mkdir_remote("newdir").unwrap();
        drv.remove_remote("/pub/file.txt").unwrap();
        drv.remove_remote("/pub/sub dir").unwrap();

        let seen: Vec<String> = rx.try_iter().collect();
        let joined = seen.join("\n");
        for probe in [
            "USER me",
            "PASS sesame",
            "TYPE I",
            "PASV",
            "MLSD /pub",
            "MKD /pub/newdir",
        ] {
            assert!(
                joined.contains(probe),
                "stub 侧须逐字收到 {probe}，实际收到:\n{joined}"
            );
        }
        assert!(seen.iter().any(|c| c == "PASS sesame"), "口令须真送达");
        for c in seen.iter().filter(|c| !c.starts_with("PASS ")) {
            assert!(!c.contains("sesame"), "口令只进 PASS 一行，实染: {c}");
        }
        // 相对入参挂 base_path、DELE+RMD 两刀都真发过
        assert!(joined.contains("DELE /pub/file.txt"));
        assert!(joined.contains("DELE /pub/sub dir"));
        assert!(joined.contains("RMD /pub/sub dir"), "目录删除须走 RMD 兜底");
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 非任务书测名沿用本模块测名家族风格
    async fn ftpLogin_withoutPassword_honestFail() {
        // 服务器要口令而本次未提供：Err 点名，不假登录不空转
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, _rx) = mpsc::channel();
        std::thread::spawn(move || ftp_stub_serve(listener, tx));
        let drv = FtpDriver::new(profile_for("127.0.0.1", port, AuthKind::Anonymous), None);
        let e = drv.mkdir_remote("x").unwrap_err();
        assert!(
            e.to_string().contains("禁假就绪"),
            "无口令遇 331 须诚实拒绝，实得 {e}"
        );
    }
}
