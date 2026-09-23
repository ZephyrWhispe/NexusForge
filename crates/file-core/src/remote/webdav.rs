//! WebDAV 客户端（09 §6.2 T-B6-3）：PROPFIND 列目录 / MKCOL / DELETE / MOVE。
//!
//! 红线（本文件是判据落点，改码前先读）：
//! - 承重①(b)：远端路径恒为 `/` 分隔 String，本文件对本地路径类型**零出现**
//!   （批次判据是对本文件 grep 该类型名计数 == 0；唯一物化边界在
//!   [`super::RemoteEntry::to_file_entry`]）。
//! - 承重⑬：TLS 不旁路——非回环 host 强制 https，本文件不出现任何"忽略/放宽
//!   证书校验"类客户端开关或自建证书校验器（判据同款 grep，含注释）。回环放行
//!   http 仅供本地联调与测试桩（过网凭据的明文总闸归 T-B6-8，回环不过网）。
//! - 凭据只进不出：口令只进 Authorization 头（Basic 现拼），URL 永不携带凭据。

use base64::Engine;
use host_core::storage::WriteCommit;
use quick_xml::events::Event;
use quick_xml::Reader;
use zeroize::Zeroizing;

use crate::error::{FileError, FILE_REMOTE_FIELD, FILE_REMOTE_MISSING};
use crate::profile::{looks_like_loopback, RemoteProfile};
use crate::remote::{remote_block_on, remote_enter, remote_error_message, AuthSecret, RemoteEntry};

/// 请求硬超时（整请求口径）。T-B6-11 起本腿也有断点语义：读腿 GET/Range 真
/// 验身走 [`super::stream_get`]，写腿 spool+PUT 见 [`WebDavPutWriter`]。
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

// ---------------------------------------------------------------------------
// URL 拼接唯一口
// ---------------------------------------------------------------------------

const PCT_HEX: &[u8; 16] = b"0123456789ABCDEF";

/// 百分号解码（非法转义按字面保留；供 `..` 裁决与 name 展示面）
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    // 解码可能切出非法 UTF-8 边界：lossy 保稳，展示名宁缺不崩
    String::from_utf8_lossy(&out).into_owned()
}

fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                out.push('%');
                out.push(PCT_HEX[(byte >> 4) as usize] as char);
                out.push(PCT_HEX[(byte & 0x0F) as usize] as char);
            }
        }
    }
    out
}

/// **唯一拼接口**：恒 `/` 分隔、去重斜杠、拒穿越段（`.` 丢弃、`..` 弹栈，
/// `%2e%2e` 这类编码后穿越同样拒——先解码再裁决）。段一律解码后重编码，
/// 对服务端给回的已编码 href 幂等。
pub fn join_remote_url(base: &str, path: &str) -> String {
    let mut segs: Vec<String> = Vec::new();
    for src in [base, path] {
        for raw in src.split('/') {
            match percent_decode(raw).as_str() {
                "" | "." => {}
                ".." => {
                    segs.pop();
                }
                other => segs.push(encode_segment(other)),
            }
        }
    }
    if segs.is_empty() {
        return "/".to_owned();
    }
    format!("/{}", segs.join("/"))
}

// ---------------------------------------------------------------------------
// PROPFIND 请求体 / 响应解析（纯函数，桩与真机共用）
// ---------------------------------------------------------------------------

/// PROPFIND 正文（RFC 4918 §14.20 Allprop-lite：目标 URI 由请求行承载，正文
/// 不重复 href——任务书签名 `propfind_body(&str /*href*/)` 的 href 参数落不了
/// 地，登记为落地补记，见 09 §6.2 T-B6-3 行）
pub fn propfind_body() -> String {
    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
     <D:propfind xmlns:D=\"DAV:\">\n\
     <D:prop>\n\
     <D:resourcetype/>\n\
     <D:getcontentlength/>\n\
     <D:getlastmodified/>\n\
     </D:prop>\n\
     </D:propfind>\n"
        .to_owned()
}

/// HTTP-date（RFC 1123，getlastmodified 的标准形状）→ 毫秒；任何不合格式回 0
/// ——**无事实源就无时间**（与 T-B6-6 MLSD 纪律同源）
pub fn parse_http_date_ms(s: &str) -> i64 {
    fn inner(s: &str) -> Option<i64> {
        let mut it = s.split_whitespace();
        let _ = it.next()?; // weekday
        let day = it.next()?;
        let mon = it.next()?;
        let year = it.next()?;
        let hms = it.next()?;
        let d: i64 = day.parse().ok()?;
        let m = match mon {
            "Jan" => 1,
            "Feb" => 2,
            "Mar" => 3,
            "Apr" => 4,
            "May" => 5,
            "Jun" => 6,
            "Jul" => 7,
            "Aug" => 8,
            "Sep" => 9,
            "Oct" => 10,
            "Nov" => 11,
            "Dec" => 12,
            _ => return None,
        };
        let y: i64 = year.parse().ok()?;
        let mut t = hms.split(':');
        let hh: i64 = t.next()?.parse().ok()?;
        let mm: i64 = t.next()?.parse().ok()?;
        let ss: i64 = t.next()?.parse().ok()?;
        Some((days_from_civil(y, m, d) * 86400 + hh * 3600 + mm * 60 + ss) * 1000)
    }
    inner(s).unwrap_or(0)
}

/// Howard Hinnant days_from_civil：公历日期 → 距 Unix 纪元的天数（无 chrono）。
/// T-B6-6 起 FTP 的 MLSD 时间解析共用此口（无事实源纪律同谱，禁两份实现）
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// 非预期状态的协议裁决（消息点名状态与目标）。本批码表固定五枚
/// （09 §6.2 T-B6-1 行"远端面错误码恒出自这五枚"），认证/网络失败共用
/// FILE_REMOTE_001 的语义扩张在落地补记登记。
fn webdav_status_err(status: u16, target: &str) -> FileError {
    let why = match status {
        401 | 403 => "凭据被拒",
        404 => "目标不存在",
        405 => "方法不被允许",
        409 => "父集合缺失（冲突）",
        _ => "对端返回非预期状态",
    };
    FileError::Remote {
        code: FILE_REMOTE_MISSING,
        msg: format!("WebDAV {why}：状态 {status}，目标 {target}"),
    }
}

fn entry_name_from_href(href: &str) -> String {
    let trimmed = href.trim_end_matches('/');
    percent_decode(trimmed.rsplit('/').next().unwrap_or(""))
}

/// href → 服务端路径（个别服务器回绝对 URL，剥 scheme://host 只留 path 形状）
fn href_to_path(href: &str) -> String {
    match href.find("://") {
        Some(i) => {
            let rest = &href[i + 3..];
            match rest.find('/') {
                Some(j) => rest[j..].to_owned(),
                None => "/".to_owned(),
            }
        }
        None => href.to_owned(),
    }
}

/// Multistatus XML → 条目表（quick-xml reader 手写状态机，零 macro、零新包）。
/// 缺 getcontentlength ⇒ size 0；缺 getlastmodified ⇒ modified_ms 0；
/// `<D:collection/>`（Start/Empty 两形态）判目录；一 response 多 href 时取首个。
pub fn parse_propfind_responses(xml: &str) -> Result<Vec<RemoteEntry>, FileError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = Vec::new();
    let mut capture: Option<&'static str> = None;
    let mut text = String::new();
    // response 累积器
    let mut href = String::new();
    let mut href_taken = false;
    let mut is_dir = false;
    let mut size = 0u64;
    let mut modified_ms = 0i64;
    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| FileError::Remote {
                code: FILE_REMOTE_MISSING,
                msg: format!("PROPFIND 响应 XML 解析失败: {e}"),
            })? {
            Event::Start(e) => {
                let name = e.local_name();
                match name.as_ref() {
                    "response" => {
                        href.clear();
                        href_taken = false;
                        is_dir = false;
                        size = 0;
                        modified_ms = 0;
                    }
                    "href" if !href_taken => {
                        capture = Some("href");
                        text.clear();
                    }
                    "getcontentlength" => {
                        capture = Some("getcontentlength");
                        text.clear();
                    }
                    "getlastmodified" => {
                        capture = Some("getlastmodified");
                        text.clear();
                    }
                    "collection" => is_dir = true,
                    _ => {}
                }
            }
            Event::Empty(e) => {
                if e.local_name().as_ref() == "collection" {
                    is_dir = true;
                }
            }
            Event::Text(t) => {
                if capture.is_some() {
                    text.push_str(t.as_ref());
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                match name.as_ref() {
                    "href" if capture == Some("href") => {
                        href = std::mem::take(&mut text);
                        href_taken = true;
                        capture = None;
                    }
                    "getcontentlength" if capture == Some("getcontentlength") => {
                        size = text.trim().parse().unwrap_or(0);
                        capture = None;
                    }
                    "getlastmodified" if capture == Some("getlastmodified") => {
                        modified_ms = parse_http_date_ms(text.trim());
                        capture = None;
                    }
                    "response" if href_taken => {
                        out.push(RemoteEntry {
                            name: entry_name_from_href(&href),
                            path: href_to_path(&href),
                            is_dir,
                            size,
                            modified_ms,
                        });
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 驱动
// ---------------------------------------------------------------------------

/// WebDAV 驱动：连接态住 [`crate::service`] 与注册表（进程内，不落盘——
/// 重启即"未连接"的真实反映，禁把连接态持久化成假连接）
pub struct WebDavDriver {
    profile: RemoteProfile,
    secret: Option<AuthSecret>,
    origin: String,
    client: reqwest::Client,
}

/// WebDAV 扩展方法名（http::Method 常量只有 10 个标准方法，PROPFIND/MKCOL/MOVE
/// 须现造；构造失败即字面量写错，属编程错误）
fn webdav_method(name: &str) -> reqwest::Method {
    reqwest::Method::from_bytes(name.as_bytes())
        .unwrap_or_else(|_| panic!("非法 HTTP 方法名: {name}"))
}

impl WebDavDriver {
    pub(crate) fn new(profile: RemoteProfile, secret: Option<AuthSecret>) -> Self {
        let scheme = if looks_like_loopback(&profile.host) {
            "http"
        } else {
            "https"
        };
        let origin = format!("{scheme}://{}:{}", profile.host, profile.port);
        let _guard = remote_enter();
        let client = reqwest::Client::builder()
            .user_agent("NexusForge")
            .connect_timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest Client 构建失败");
        Self {
            profile,
            secret,
            origin,
            client,
        }
    }

    pub(crate) fn profile(&self) -> &RemoteProfile {
        &self.profile
    }
    pub(crate) fn driver_label(&self) -> String {
        format!("WebDAV {}", self.profile.label)
    }
    pub(crate) fn base_path(&self) -> String {
        join_remote_url(&self.profile.base_path, "")
    }

    /// 绝对入参 = 服务端路径原样规范化（防穿越唯一口）；相对入参挂 base_path
    fn url_for(&self, remote_path: &str) -> String {
        if remote_path.starts_with('/') {
            format!("{}{}", self.origin, join_remote_url("", remote_path))
        } else {
            format!(
                "{}{}",
                self.origin,
                join_remote_url(&self.profile.base_path, remote_path)
            )
        }
    }

    /// Authorization 值：显式 header 优先，否则 user+password 现拼 Basic。
    /// 口令永不进 URL（`只进不出` 的行为面）。
    fn auth_header(&self) -> Option<Zeroizing<String>> {
        let s = self.secret.as_ref()?;
        if let Some(h) = &s.header {
            return Some(Zeroizing::new(h.to_string()));
        }
        let p = s.password.as_ref()?;
        let pair = Zeroizing::new(format!("{}:{}", self.profile.user, p.as_str()));
        Some(Zeroizing::new(format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(&*pair)
        )))
    }

    fn request(
        &self,
        method: reqwest::Method,
        url: String,
        body: Option<String>,
        headers: Vec<(&'static str, String)>,
    ) -> Result<(u16, String), FileError> {
        let client = self.client.clone();
        let auth = self.auth_header();
        remote_block_on(async move {
            let mut req = client.request(method, &url).timeout(REQUEST_TIMEOUT);
            if let Some(a) = auth {
                req = req.header("Authorization", a.as_str());
            }
            for (k, v) in headers {
                req = req.header(k, v);
            }
            if let Some(b) = body {
                req = req.header("Content-Type", "application/xml").body(b);
            }
            let resp = req.send().await.map_err(|e| FileError::Remote {
                code: FILE_REMOTE_MISSING,
                msg: format!("WebDAV 请求失败（{url}）: {e}"),
            })?;
            let status = resp.status().as_u16();
            let text = resp.text().await.map_err(|e| FileError::Remote {
                code: FILE_REMOTE_MISSING,
                msg: format!("WebDAV 响应体读取失败（{url}）: {e}"),
            })?;
            Ok((status, text))
        })
    }

    pub fn list_entries(&self, path: &str) -> Result<Vec<RemoteEntry>, FileError> {
        let mut url = self.url_for(path);
        if !url.ends_with('/') {
            url.push('/');
        }
        let (status, text) = self.request(
            webdav_method("PROPFIND"),
            url.clone(),
            Some(propfind_body()),
            vec![("Depth", "1".to_owned())],
        )?;
        if status != 207 {
            return Err(webdav_status_err(status, path));
        }
        let entries = parse_propfind_responses(&text)?;
        // 自条目剔除：规范化后与请求路径逐字相等者（根时对根）
        let req_key = url[self.origin.len()..].trim_end_matches('/').to_owned();
        Ok(entries
            .into_iter()
            .filter(|e| join_remote_url("", &e.path).trim_end_matches('/') != req_key)
            .collect())
    }

    pub fn mkdir_remote(&self, path: &str) -> Result<(), FileError> {
        let url = self.url_for(path);
        let (status, _) = self.request(webdav_method("MKCOL"), url, None, vec![])?;
        match status {
            201 => Ok(()),
            // 幂等：目录已在即成功（与本地 create_dir_all 语义对齐）
            405 => Ok(()),
            _ => Err(webdav_status_err(status, path)),
        }
    }

    pub fn remove_remote(&self, path: &str) -> Result<(), FileError> {
        let url = self.url_for(path);
        let (status, _) = self.request(reqwest::Method::DELETE, url, None, vec![])?;
        match status {
            200 | 202 | 204 => Ok(()),
            _ => Err(webdav_status_err(status, path)),
        }
    }

    pub fn rename_remote(&self, from: &str, to: &str) -> Result<(), FileError> {
        let url = self.url_for(from);
        let dest = self.url_for(to);
        let (status, _) = self.request(
            webdav_method("MOVE"),
            url,
            None,
            vec![("Destination", dest.clone())],
        )?;
        match status {
            200 | 201 | 204 => Ok(()),
            _ => Err(webdav_status_err(status, to)),
        }
    }

    // ---- T-B6-11 字节腿（队列远端执行器的消费面）----

    /// 真流式读腿：与 HTTPS 腿共用 [`super::stream_get`]（auth 头装配是本腿
    /// 唯一差异）；`offset>0` 必须真收到 206，错位字节绝不进流。
    pub(crate) fn get_stream(
        &self,
        remote_path: &str,
        offset: u64,
    ) -> Result<Box<dyn std::io::Read + Send>, FileError> {
        let url = self.url_for(remote_path);
        let mut req = self.client.get(&url);
        if let Some(a) = self.auth_header() {
            req = req.header("Authorization", a.as_str());
        }
        super::stream_get(req, &url, offset)
    }

    /// 写腿（spool + 一次性 PUT）：reqwest 的 stream feature 是批次红线不扩
    /// ⇒ 上传先攒 spool、提交时整体送，并带**单件字节限额**——超限 Err 点名
    /// 而不在大文件上赌内存（限额是登记的偏差，09 §6.2 T-B6-11 落地补记）。
    pub(crate) fn put_writer(&self, remote_path: &str) -> Result<Box<dyn WriteCommit>, FileError> {
        let url = self.url_for(remote_path);
        Ok(Box::new(WebDavPutWriter {
            spool: super::Spool::create("webdav-put")?,
            remote: remote_path.to_owned(),
            url,
            client: self.client.clone(),
            auth: self.auth_header(),
        }))
    }
}

/// PUT 提交上限（spool 落盘也要读进内存才能整体送——超限连 spool 都不收）
pub(crate) const WEBDAV_PUT_MAX_BYTES: u64 = 512 * 1024 * 1024;

struct WebDavPutWriter {
    spool: super::Spool,
    remote: String,
    url: String,
    client: reqwest::Client,
    auth: Option<Zeroizing<String>>,
}

impl std::io::Write for WebDavPutWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(self.spool.file_mut(), buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(self.spool.file_mut())
    }
}

impl WriteCommit for WebDavPutWriter {
    fn finish(mut self: Box<Self>) -> Result<(), host_core::error::AppError> {
        let len = self.spool.disk_len()?;
        if len > WEBDAV_PUT_MAX_BYTES {
            return Err(host_core::error::AppError::from(FileError::Remote {
                code: FILE_REMOTE_FIELD,
                msg: format!(
                    "WebDAV 上传单件上限 {WEBDAV_PUT_MAX_BYTES} 字节（PUT 为整体提交腿，spool 超限即拒；本件 {len}）",
                ),
            }));
        }
        let path = self.spool.publish()?;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => return Err(FileError::Io(e).into()),
        };
        let remote = self.remote.clone();
        let e = put_submit(&self.client, &self.url, &remote, self.auth.as_ref(), bytes);
        // 失败与成功都清 spool（临时文件不留残）
        let _ = std::fs::remove_file(&path);
        e
    }
}

/// PUT 提交（auth 头在此进请求——口令只进请求，url 与消息面零凭据位）
fn put_submit(
    client: &reqwest::Client,
    url: &str,
    remote: &str,
    auth: Option<&Zeroizing<String>>,
    bytes: Vec<u8>,
) -> Result<(), host_core::error::AppError> {
    let url = url.to_owned();
    let client = client.clone();
    let auth = auth.map(|a| Zeroizing::new(a.to_string()));
    let result: Result<u16, FileError> = super::remote_block_on(async move {
        let mut req = client.put(&url).timeout(REQUEST_TIMEOUT).body(bytes);
        if let Some(a) = &auth {
            req = req.header("Authorization", a.as_str());
        }
        let resp = req.send().await.map_err(|e| FileError::Remote {
            code: FILE_REMOTE_MISSING,
            msg: super::remote_error_message(&format!("WebDAV PUT 请求失败（{url}）: {e}")),
        })?;
        Ok(resp.status().as_u16())
    });
    match result {
        Ok(200 | 201 | 204) => Ok(()),
        Ok(status) => Err(webdav_status_err(status, remote).into()),
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------------------
// 截图桥最小装配（09 §6.2 T-B6-12）：screenshot-core 与 file-core 互不依赖
// （DESIGN O1），src-tauri 宿主桥闭包消费下面这一对——装配是纯函数（可单测），
// 提交是一条 async 腿。截图侧对 WebDAV 协议零知识；凭据由调用方逐次传入，
// 与本文件 auth_header() 的"只进请求不进字段"同谱。
// ---------------------------------------------------------------------------

/// PUT 装配结果（URL + 请求头形状；body 由提交腿携带，不经过此处——
/// 字节不落进任何可 Debug 的装配体）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebDavPutAssembly {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// 唯一装配口：endpoint_base（绝对地址，可带目录段）+ 单层文件名 →
/// 逐段重编码的目标 URL；`Overwrite: F`（同名不默默覆盖——截图撞名是用户的
/// 资产互踩，409/412 在提交腿点名）。auth_header 逐次传入（None/空 = 匿名 PUT）。
pub fn assemble_put(
    endpoint_base: &str,
    filename: &str,
    auth_header: Option<&str>,
) -> Result<WebDavPutAssembly, String> {
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.chars().any(|c| c.is_control())
    {
        return Err("WebDAV 上传文件名非法（含路径分隔或控制字符）".to_owned());
    }
    let scheme_end = endpoint_base
        .find("://")
        .ok_or_else(|| "WebDAV 上传端点不是绝对地址（缺 scheme://）".to_owned())?;
    let after = &endpoint_base[scheme_end + 3..];
    let path_start = after.find('/').unwrap_or(after.len());
    let (origin, base_path) = endpoint_base.split_at(scheme_end + 3 + path_start);
    if base_path.chars().any(|c| c.is_control()) {
        return Err("WebDAV 上传端点含控制字符".to_owned());
    }
    let url = format!("{origin}{}", join_remote_url(base_path, filename));
    let mut headers = vec![("Overwrite".to_owned(), "F".to_owned())];
    if let Some(h) = auth_header.map(str::trim).filter(|s| !s.is_empty()) {
        if h.chars().any(|c| c == '\r' || c == '\n') {
            return Err("凭据含 CRLF（请求头注入面），本次 PUT 拒发".to_owned());
        }
        headers.push(("Authorization".to_owned(), h.to_owned()));
    }
    Ok(WebDavPutAssembly { url, headers })
}

/// 提交腿：PUT 整字节。合规闸（TLS/本机裁决）**不在这里**——权威口是
/// screenshot-core 的 `validate_upload_endpoint`（桥闭包先裁后到），本处不建
/// 第二张 TLS 闸（09 §6.2 T-B6-12 落地补记）。
pub async fn send_put(assembled: &WebDavPutAssembly, bytes: Vec<u8>) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .user_agent("NexusForge")
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| remote_error_message(&format!("WebDAV 客户端构建失败: {e}")))?;
    let mut req = client.put(&assembled.url).body(bytes);
    for (k, v) in &assembled.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = req
        .send()
        .await
        .map_err(|e| remote_error_message(&format!("WebDAV PUT 失败（{}）: {e}", assembled.url)))?;
    let status = resp.status().as_u16();
    match status {
        200 | 201 | 204 => Ok(()),
        409 | 412 => Err(format!(
            "远端同名已存在，已按 Overwrite: F 拒覆盖（HTTP {status}）：请改端点目录或先清理"
        )),
        s => Err(remote_error_message(&format!(
            "WebDAV PUT 对端返回 {s}（{}）",
            assembled.url
        ))),
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::FILE_REMOTE_MISSING;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn profile_for(host: &str, port: u16, base: &str) -> RemoteProfile {
        RemoteProfile {
            id: crate::profile::profile_id_of("t3stub"),
            label: "桩站".into(),
            protocol: crate::profile::RemoteProtocol::WebDav,
            host: host.into(),
            port,
            user: "me".into(),
            base_path: base.into(),
            auth: crate::profile::AuthKind::PromptEachTime,
            preset_id: None,
            last_used_ms: 0,
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-3）字面测试名优先于 rustc 命名惯例
    fn webdavList_parsesPropfindFixture_intoEntries() {
        // 三臂：href 带百分号 / getcontentlength 缺失 / 404 状态裁决
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/%E6%8A%A5%E5%91%8A%202026.txt</D:href>
    <D:propstat><D:prop>
      <D:resourcetype/>
      <D:getcontentlength>42</D:getcontentlength>
      <D:getlastmodified>Wed, 15 Jun 2022 08:30:15 GMT</D:getlastmodified>
    </D:prop></D:propstat>
  </D:response>
  <D:response>
    <D:href>http://other.example/dav/sub%20dir/</D:href>
    <D:propstat><D:prop>
      <D:resourcetype><D:collection/></D:resourcetype>
    </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
  </D:response>
</D:multistatus>"#;
        let got = parse_propfind_responses(xml).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "报告 2026.txt", "href 百分号只解展示名");
        assert_eq!(
            got[0].path, "/dav/%E6%8A%A5%E5%91%8A%202026.txt",
            "path 保真服务端编码形状"
        );
        assert_eq!(got[0].size, 42);
        assert_eq!(got[0].modified_ms, 1_655_281_815_000);
        assert!(got[1].is_dir);
        assert_eq!(
            got[1].size, 0,
            "getcontentlength 缺失 ⇒ 0（无事实源就无字节）"
        );
        assert_eq!(
            got[1].modified_ms, 0,
            "getlastmodified 缺失 ⇒ 0（无事实源就无时间）"
        );
        assert_eq!(
            got[1].path, "/dav/sub%20dir/",
            "绝对 URL href 剥 scheme+host"
        );
        let e = webdav_status_err(404, "/dav/gone");
        assert!(
            matches!(&e, FileError::Remote { code, msg } if *code == FILE_REMOTE_MISSING && msg.contains("404") && msg.contains("/dav/gone")),
            "404 裁决须点名状态与目标: {e}"
        );
        // 时间解析的反直觉臂：畸形日期不得借尸还魂成"现在"
        assert_eq!(parse_http_date_ms("Wed, 32 Foo 2022 99:99:99 GMT"), 0);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-3）字面测试名优先于 rustc 命名惯例
    fn joinRemote_url_noDoubleSlashAndNoTraversal() {
        // 双斜杠去重
        assert_eq!(join_remote_url("/a", "b//c"), "/a/b/c");
        assert_eq!(join_remote_url("/a//", "//b"), "/a/b");
        // 明文穿越：弹栈不越根
        assert_eq!(join_remote_url("/a/b", "/../c"), "/a/c");
        assert_eq!(join_remote_url("/", "/../../x"), "/x");
        // 编码穿越（%2e%2e）同样裁决
        assert_eq!(join_remote_url("/a/b", "/%2e%2e/%2e%2e/c"), "/c");
        assert_eq!(join_remote_url("/a", "./b"), "/a/b");
        // 空格等字符重编码；已编码 href 幂等
        assert_eq!(join_remote_url("/", "a b"), "/a%20b");
        assert_eq!(join_remote_url("/", "/a%20b/c"), "/a%20b/c");
        assert_eq!(join_remote_url("", ""), "/");
    }

    struct StubHit {
        request_line: String,
        depth: Option<String>,
        auth: Option<String>,
        destination: Option<String>,
    }

    const STUB_LISTING: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response><D:href>/nf%20test/</D:href>
    <D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop></D:propstat>
  </D:response>
  <D:response><D:href>/nf%20test/sub%20dir/</D:href>
    <D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype>
      <D:getlastmodified>Wed, 15 Jun 2022 08:30:15 GMT</D:getlastmodified>
    </D:prop></D:propstat>
  </D:response>
  <D:response><D:href>/nf%20test/file.txt</D:href>
    <D:propstat><D:prop><D:resourcetype/>
      <D:getcontentlength>7</D:getcontentlength>
    </D:prop></D:propstat>
  </D:response>
</D:multistatus>"#;

    fn stub_serve(listener: TcpListener, tx: mpsc::Sender<StubHit>) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = stub_conn(stream, tx) {
                    tracing::debug!("stub 连接收尾: {e}");
                }
            });
        }
    }

    fn stub_conn(stream: std::net::TcpStream, tx: mpsc::Sender<StubHit>) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(()); // 连接关闭
            }
            let mut depth = None;
            let mut auth = None;
            let mut destination = None;
            let mut content_len = 0usize;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h)?;
                if h == "\r\n" || h == "\n" || h.is_empty() {
                    break;
                }
                let name = h.split(':').next().unwrap_or("").to_ascii_lowercase();
                // 头值取原始大小写（Authorization 的 "Basic " 前缀不得被压小写）
                let value = h
                    .split_once(':')
                    .map_or_else(String::new, |x| x.1.trim().to_owned());
                match name.as_str() {
                    "depth" => depth = Some(value),
                    "authorization" => auth = Some(value),
                    "destination" => destination = Some(value),
                    "content-length" => content_len = value.parse().unwrap_or(0),
                    _ => {}
                }
            }
            let mut body = vec![0u8; content_len];
            reader.read_exact(&mut body)?;
            let method = line.split_whitespace().next().unwrap_or("").to_owned();
            tx.send(StubHit {
                request_line: line.trim_end().to_owned(),
                depth,
                auth,
                destination,
            })
            .ok();
            let (head, payload): (String, Vec<u8>) = match method.as_str() {
                "PROPFIND" => {
                    let p = STUB_LISTING.as_bytes().to_vec();
                    (
                        format!(
                            "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\n\r\n",
                            p.len()
                        ),
                        p,
                    )
                }
                "MKCOL" => (
                    "HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n".to_string(),
                    vec![],
                ),
                "DELETE" => ("HTTP/1.1 204 No Response\r\n\r\n".to_string(), vec![]),
                _ => (
                    "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n".to_string(),
                    vec![],
                ),
            };
            writer.write_all(head.as_bytes())?;
            writer.write_all(&payload)?;
            writer.flush()?;
            // keep-alive：reqwest 复用连接，PROPFIND 后继续收下一条
        }
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-3）字面测试名优先于 rustc 命名惯例
    async fn webdavAgainstFakeServer_endToEnd() {
        // 桩断请求比断响应更能防协议漂移：逐字校方法名/路径/Depth 头
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || stub_serve(listener, tx));

        let drv = WebDavDriver::new(
            profile_for("127.0.0.1", port, "/nf test"),
            Some(AuthSecret {
                header: None,
                password: Some(Zeroizing::new("sesame".into())),
            }),
        );

        // ① 列目录（自条目剔除 + name 解码 + path 保真编码）
        let entries = drv.list_entries("/nf test/").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "sub dir");
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].path, "/nf%20test/sub%20dir/");
        assert_eq!(entries[1].name, "file.txt");
        assert_eq!(entries[1].size, 7);

        // ② 建目录（服务端 href 形状直接回用作请求路径 = 浏览→操作闭环）
        drv.mkdir_remote("/nf%20test/newdir").unwrap();
        // ③ 删除
        drv.remove_remote("/nf%20test/sub%20dir").unwrap();

        let hits: Vec<StubHit> = rx.try_iter().collect();
        assert_eq!(hits.len(), 3, "桩侧应逐字收到三请求");
        assert_eq!(hits[0].request_line, "PROPFIND /nf%20test/ HTTP/1.1");
        assert_eq!(hits[0].depth.as_deref(), Some("1"));
        let auth = hits[0].auth.clone().expect("Basic 凭据须过网一次");
        let b64 = auth.strip_prefix("Basic ").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        assert_eq!(decoded, b"me:sesame", "口令只进 Authorization 头");
        assert!(
            !hits[0].request_line.contains("sesame"),
            "URL 永不得携带凭据"
        );
        assert_eq!(hits[1].request_line, "MKCOL /nf%20test/newdir HTTP/1.1");
        assert_eq!(hits[2].request_line, "DELETE /nf%20test/sub%20dir HTTP/1.1");
        assert!(hits[2].destination.is_none());
    }

    #[test]
    fn status_arm_covers_auth_rejection() {
        // 401/403 与 404 分消息同码（本批码表固定的语义扩张臂）
        for code in [401u16, 403] {
            let e = webdav_status_err(code, "/x");
            assert!(
                matches!(&e, FileError::Remote { code: c, msg } if *c == FILE_REMOTE_MISSING
                    && msg.contains("凭据被拒")
                    && msg.contains(&code.to_string())),
                "{code} 臂消息须点名成因: {e}"
            );
        }
    }
}
