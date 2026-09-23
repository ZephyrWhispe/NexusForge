//! HTTPS 下载腿（09 §6.2 T-B6-4）：Range 续传只在对端声明时承诺 + 错误脱敏唯一口。
//!
//! 红线（改码前先读）：
//! - 承重⑨：本驱动**只有下载腿**——`list` 一律 `Err(FILE_REMOTE_005)` 点名
//!   "HTTP 下载源不支持浏览"，拒绝而非空表（假就绪=空表，拒才是诚实）。
//! - 流式只走 `Response::chunk()`：reqwest 的 `stream` feature 不扩也不引
//!   blocking 门面（判据 grep 扫全 file-core 源码，红线注释亦不用字面 token，
//!   T-B6-3 判据同款教训）。
//! - 续传的诚实纪律：只有对端真回了 206 才判 `Range`；200 即便带
//!   `Accept-Ranges: bytes` 也判 `Whole`——没真用 Range 就不能承诺续传。
//! - `remote_error_message` 是远端错误**唯一对外消息口**：`OpProgress.error`
//!   与 pending_ops 的 error 面从此不含 URL query 凭据与响应体原文。

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use regex::Regex;

use crate::error::{FileError, FILE_REMOTE_MISSING};
use crate::module::FileConfig;
use crate::profile::{looks_like_loopback, RemoteProfile};

use super::webdav::join_remote_url;
use super::{remote_block_on, remote_enter, remote_rt, ThrottleGate};

pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// 续传档位（T-B6-11 上移至 host-core::storage——`DriverCapabilities` 引用它，
/// D-02 同谱；此处再导出保持 `crate::remote::Resumable` 的既有读口与 wire 形状）
pub use host_core::storage::Resumable;

/// 续传起点唯一裁决（[`classify_resume`] 的下游）：`Whole` 档续传从 0 起重取，
/// 禁拿着旧 `have` 假装断点成立（"显示已续传实际重跑"的谎报位在此收口）。
pub fn resume_offset(resumable: Resumable, have: u64) -> u64 {
    match resumable {
        Resumable::Range | Resumable::Append => have,
        Resumable::Whole => 0,
    }
}

/// 唯一裁决口：只有真 206 才承诺 Range；无事实源即 Whole。
pub fn classify_resume(status: u16, headers: &reqwest::header::HeaderMap) -> Resumable {
    if status == 206 {
        return Resumable::Range;
    }
    let accept_ranges = headers
        .get(reqwest::header::ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // 反直觉臂（判据红线钉死）：200 + Accept-Ranges 仍 Whole——对端**声明**支持
    // 不等于本次响应**用了** Range，拿声明承诺续传就是假就绪。
    if status == 200 && accept_ranges == "bytes" {
        tracing::debug!("对端声明 Accept-Ranges: bytes 但本次未按 Range 应答，按 Whole 处理");
    }
    Resumable::Whole
}

/// Range 计划纯算术：`have >= total` → `None`（已齐，无事可做）。
pub fn range_plan(total: u64, have: u64) -> Option<(u64, u64)> {
    if have >= total {
        return None;
    }
    Some((have, total - 1))
}

/// 单连接预算 = 总闸 / 活跃传输数（防单传输饿死另一传输）。
/// 两臂显式定义：总闸 `0` = 不限（返回 0）；`active == 0` 不除零（预算无从摊，
/// 返回总闸值）。注意下限钳到 1：返回 0 会被下游误读成"不限"。
pub fn throttle_share_kbps(total_kbps: u32, active: usize) -> u32 {
    if total_kbps == 0 {
        return 0;
    }
    if active == 0 {
        return total_kbps;
    }
    (total_kbps / active as u32).max(1)
}

/// 远端错误**唯一对外消息口**：掩 `?auth=`/`?X-Amz-*=`/`password=` 的值与
/// `Bearer` 令牌，CRLF 折叠为单空格（防日志注入），整条 ≤200 char，
/// 截断时以"…（原因原文见应用日志）"收尾（原文只进应用日志，不进消息面）。
pub fn remote_error_message(raw: &str) -> String {
    static PAIR: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static BEARER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static NL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let pair = PAIR.get_or_init(|| {
        // 键前的 [?&] 可选：错误消息里的裸 `password=` 同样要掩
        Regex::new(r"(?i)([?&]?(?:auth|password|X-Amz-[0-9A-Za-z]+)=)[^&\s]*").unwrap()
    });
    let bearer = BEARER.get_or_init(|| Regex::new(r"(?i)Bearer\s+\S+").unwrap());
    let nl = NL.get_or_init(|| Regex::new(r"[\r\n]+").unwrap());
    let masked_once = pair.replace_all(raw, "${1}***");
    let masked = bearer.replace_all(&masked_once, "Bearer ***");
    let folded = nl.replace_all(&masked, " ");
    const TAIL: &str = "…（原因原文见应用日志）";
    let folded: String = folded.split_whitespace().collect::<Vec<_>>().join(" ");
    if folded.chars().count() <= 200 {
        return folded;
    }
    let keep = 200 - TAIL.chars().count();
    let head: String = folded.chars().take(keep).collect();
    format!("{head}{TAIL}")
}

/// 一次下载的结果（面板/断点消费的是**实际发生的**起点与字节，不是计划）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadOutcome {
    pub resumable: Resumable,
    pub start_offset: u64,
    pub bytes_done: u64,
}

/// HTTPS 下载源驱动：无浏览面、无写面，只有 `download_to` 一条腿。
pub struct HttpsDriver {
    profile: RemoteProfile,
    origin: String,
    client: reqwest::Client,
    /// 配置真源句柄（T-B6-6）：`download_kbps` 总闸的**唯一直读点**——每次下载
    /// 起算时读一次现值，改闸不重连即生效。与 FileService 共享同一个 Arc。
    config: Arc<parking_lot::RwLock<FileConfig>>,
}

impl HttpsDriver {
    pub(crate) fn new(
        profile: RemoteProfile,
        config: Arc<parking_lot::RwLock<FileConfig>>,
    ) -> Self {
        // 承重⑬：非回环 host 强制 https；回环 http 仅供本地联调与测试桩
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
            origin,
            client,
            config,
        }
    }

    pub(crate) fn profile(&self) -> &RemoteProfile {
        &self.profile
    }

    pub(crate) fn driver_label(&self) -> String {
        format!("HTTPS {}", self.profile.label)
    }

    pub(crate) fn base_path(&self) -> String {
        join_remote_url(&self.profile.base_path, "")
    }

    /// 唯一拼接口（与 WebDAV 腿共用 `join_remote_url`）：绝对 = 原样规范化，
    /// 相对 = 挂 base_path
    pub fn url_for(&self, remote_path: &str) -> String {
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

    /// 真流式读腿（T-B6-11）：GET（`offset>0` 带 `Range` 且**必须**收到 206，
    /// 否则 Err 点名——读流恰从 offset 起是 trait 契约，禁静默从头给字节）。
    /// 与 [`download_to`](Self::download_to) 共用 [`super::stream_get`] 一条腿，
    /// 不另起第二份请求装配。
    pub(crate) fn get_stream(
        &self,
        remote_path: &str,
        offset: u64,
    ) -> Result<Box<dyn std::io::Read + Send>, FileError> {
        let url = self.url_for(remote_path);
        let req = self.client.get(&url).timeout(REQUEST_TIMEOUT);
        super::stream_get(req, &url, offset)
    }

    /// 下载一条腿：`have` 为调用方声称的本地已有字节；对端回 206 才从 `have`
    /// 续写，回 200 即整取重下（截断重写，见 [`resume_offset`]）。
    /// `max_kbps` 为调用方显式预算（0 = 交给配置总闸）：没显式预算时按
    /// `download_kbps / max(活跃并发, 1)` 摊（[`throttle_share_kbps`]），
    /// 总闸默认 0 = 不限——配置位与调用位在这一个口合流，禁两处各限一次。
    pub fn download_to(
        &self,
        remote_path: &str,
        dst: &Path,
        have: u64,
        max_kbps: u32,
    ) -> Result<DownloadOutcome, FileError> {
        let url = self.url_for(remote_path);
        let client = self.client.clone();
        let dst = dst.to_path_buf();
        let effective = if max_kbps == 0 {
            let cfg = self.config.read();
            throttle_share_kbps(cfg.download_kbps, cfg.max_concurrent.max(1))
        } else {
            max_kbps
        };
        remote_block_on(async move { download_pump(&client, &url, &dst, have, effective).await })
    }
}

/// 流式泵：`Response::chunk()` 逐块落盘（无 `stream` feature 不扩），
/// 每块过 [`ThrottleGate`]（T-B6-6 统一限速闸，纯核心可单测）。
async fn download_pump(
    client: &reqwest::Client,
    url: &str,
    dst: &Path,
    have: u64,
    max_kbps: u32,
) -> Result<DownloadOutcome, FileError> {
    let mut req = client.get(url).timeout(REQUEST_TIMEOUT);
    if have > 0 {
        req = req.header("Range", format!("bytes={have}-"));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| http_err(format!("HTTPS 请求失败（{url}）: {e}")))?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(http_err(format!(
            "HTTPS 对端返回 {status}（{url}）: {}",
            resp.status().canonical_reason().unwrap_or("未知状态")
        )));
    }
    let resumable = classify_resume(status, resp.headers());
    let start = resume_offset(resumable, have);
    let mut file = if start == 0 {
        // Whole：整取重下，盘上旧字节不算数——重建，禁在旧内容上叠写出脏文件
        std::fs::File::create(dst)
            .map_err(|e| http_err(format!("创建目标文件失败（{}）: {e}", dst.display())))?
    } else {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(dst)
            .map_err(|e| http_err(format!("打开目标文件失败（{}）: {e}", dst.display())))?;
        let md = f
            .metadata()
            .map_err(|e| http_err(format!("目标文件元数据失败: {e}")))?;
        if md.len() < start {
            // 调用方声称的 have 比盘上实际还长：以盘上为事实源拒写空洞，
            // 让上层按断点失真重裁（禁静默截断/填空洞两种谎）
            return Err(http_err(format!(
                "断点失真：声称已有 {start} 字节但盘上只有 {}",
                md.len()
            )));
        }
        f.seek(SeekFrom::Start(start))
            .map_err(|e| http_err(format!("目标文件定位失败: {e}")))?;
        f
    };
    let mut body = resp;
    let mut written = 0u64;
    let mut gate = ThrottleGate::new(max_kbps);
    loop {
        let chunk = tokio::time::timeout(REQUEST_TIMEOUT, body.chunk())
            .await
            .map_err(|_| http_err(format!("HTTPS 流读取超时（{url}）")))?
            .map_err(|e| http_err(format!("HTTPS 流读取失败（{url}）: {e}")))?;
        let Some(bytes) = chunk else { break };
        file.write_all(&bytes)
            .map_err(|e| http_err(format!("目标文件写入失败（{}）: {e}", dst.display())))?;
        written += bytes.len() as u64;
        let pause = gate.allow(bytes.len() as u64);
        if !pause.is_zero() {
            tokio::time::sleep(pause).await;
        }
    }
    file.flush().ok();
    Ok(DownloadOutcome {
        resumable,
        start_offset: start,
        bytes_done: start + written,
    })
}

fn http_err(msg: String) -> FileError {
    FileError::Remote {
        code: FILE_REMOTE_MISSING,
        // 构造即脱敏：错误出口唯一，任何上游拿到的是掩后形状
        msg: remote_error_message(&msg),
    }
}

// ---------------------------------------------------------------------------
// 真流式 GET 腿（T-B6-11）：HTTPS 与 WebDAV 共用这一条——协议装配差异只在
// 调用方递进来的 RequestBuilder（auth 头），请求/验身/泵/通道不存在第二套。
// ---------------------------------------------------------------------------

/// 流式 GET 统一口：发送 → 状态验身 → `offset>0` 必须真收到 206（否则 Err
/// 点名"未按 Range"，执行器据此裁决整取重启）→ chunk 泵进远端专用 runtime。
/// 返回的读流**恰从 offset 起**——这是 [`StorageDriver::read_stream`](host_core::storage::StorageDriver::read_stream)
/// 的契约，禁静默从头给字节。
pub(crate) fn stream_get(
    req: reqwest::RequestBuilder,
    url_display: &str,
    offset: u64,
) -> Result<Box<dyn Read + Send>, FileError> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, String>>();
    let url_owned = url_display.to_owned();
    remote_block_on(async move {
        let url_display = url_owned;
        let mut req = req.timeout(REQUEST_TIMEOUT);
        if offset > 0 {
            req = req.header("Range", format!("bytes={offset}-"));
        }
        let resp = req
            .send()
            .await
            .map_err(|e| http_err(format!("HTTP 流请求失败（{url_display}）: {e}")))?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            return Err(http_err(format!(
                "HTTP 对端返回 {status}（{url_display}）: {}",
                resp.status().canonical_reason().unwrap_or("未知状态")
            )));
        }
        if offset > 0 && status != 206 {
            // "未按 Range"是 ops.rs 远端臂的裁决锚点（截断重启从 0），改措辞须两处同改
            return Err(http_err(format!(
                "未按 Range 应答（{url_display}）：请求 bytes={offset}- 而对端回 {status}，拒绝拼接错位字节"
            )));
        }
        remote_rt().handle().spawn(async move {
            let mut body = resp;
            loop {
                match body.chunk().await {
                    Ok(Some(chunk)) => {
                        if tx.send(Ok(chunk.to_vec())).is_err() {
                            break; // 读端已弃 = 取消，泵静默退役
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        // 半途错误必须成形为读 Err，不得伪装 EOF（承重：谎报 EOF = 残缺文件当成功）
                        let _ = tx.send(Err(remote_error_message(&format!(
                            "HTTP 流读取失败（{url_display}）: {e}"
                        ))));
                        break;
                    }
                }
            }
        });
        Ok(())
    })?;
    Ok(Box::new(ChunkReader {
        rx: Some(rx),
        pending: Vec::new(),
        pos: 0,
    }))
}

/// 异步 chunk → 同步 `Read` 的唯一桥（读端线程阻塞在通道上，worker 无 tokio
/// 上下文也可泵字节）
struct ChunkReader {
    rx: Option<std::sync::mpsc::Receiver<Result<Vec<u8>, String>>>,
    pending: Vec<u8>,
    pos: usize,
}

impl Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.pending.len() {
            let Some(rx) = &self.rx else {
                return Ok(0); // 泵正常收尾（sender 已 drop）= EOF
            };
            match rx.recv() {
                Ok(Ok(bytes)) => {
                    self.pending = bytes;
                    self.pos = 0;
                }
                Ok(Err(msg)) => return Err(std::io::Error::other(msg)),
                Err(_) => self.rx = None,
            }
        }
        let n = buf.len().min(self.pending.len() - self.pos);
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::RemoteProtocol;
    use std::io::{BufRead, BufReader};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;

    use host_core::storage::StorageDriver;

    fn profile_for(port: u16) -> RemoteProfile {
        RemoteProfile {
            id: format!("remote:https-{port}"),
            label: "stub".into(),
            protocol: RemoteProtocol::Https,
            host: "127.0.0.1".into(),
            port,
            user: String::new(),
            base_path: "/".into(),
            auth: crate::profile::AuthKind::Anonymous,
            preset_id: None,
            last_used_ms: 0,
        }
    }

    /// 桩记录：请求行 + Range 头（若有）。`honor_range` = false 时对 Range 视而不见
    /// （一律 200 全量），模拟"声明支持却没用"或根本不支持的对端。
    struct StubHit {
        request_line: String,
        range: Option<String>,
    }

    fn serve(body: &'static [u8], honor_range: bool) -> (u16, mpsc::Receiver<StubHit>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let tx = tx.clone();
                std::thread::spawn(move || {
                    if let Err(e) = serve_conn(stream, tx, body, honor_range) {
                        tracing::debug!("https stub 连接收尾: {e}");
                    }
                });
            }
        });
        (port, rx)
    }

    fn serve_conn(
        stream: TcpStream,
        tx: mpsc::Sender<StubHit>,
        body: &'static [u8],
        honor_range: bool,
    ) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let mut range = None;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h)?;
                if h == "\r\n" || h == "\n" || h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    if k.eq_ignore_ascii_case("range") {
                        range = Some(v.trim().to_owned());
                    }
                }
            }
            let range_for_from = range.clone();
            tx.send(StubHit {
                request_line: line.trim_end().to_owned(),
                range,
            })
            .ok();
            let from = if honor_range {
                range_for_from
                    .as_deref()
                    .and_then(|r| r.strip_prefix("bytes="))
                    .and_then(|r| r.split('-').next())
                    .and_then(|s| s.parse::<usize>().ok())
                    .filter(|s| *s < body.len())
                    .unwrap_or(0)
            } else {
                0
            };
            let payload = &body[from..];
            let head = if honor_range && from > 0 {
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\r\n",
                    payload.len()
                )
            } else {
                format!(
                    "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\r\n",
                    payload.len()
                )
            };
            writer.write_all(head.as_bytes())?;
            writer.write_all(payload)?;
            writer.flush()?;
        }
    }

    const BODY: &[u8] = b"0123456789ABCDEFGH";

    fn default_cfg() -> Arc<parking_lot::RwLock<FileConfig>> {
        Arc::new(parking_lot::RwLock::new(FileConfig::default()))
    }

    fn tmp_dst(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("nf_https_{tag}_{}.bin", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-4）字面测试名优先于 rustc 命名惯例
    fn httpsDriver_listRefusesNotEmptyList() {
        // 承重⑨：假就绪 = 空表，拒才是诚实
        let drv = HttpsDriver::new(profile_for(9), default_cfg());
        let e = <HttpsDriver as StorageDriver>::list(&drv, Path::new("/"))
            .expect_err("HTTP 下载源的 list 必须 Err，不得回落空表");
        assert!(
            e.to_string().contains("HTTP 下载源不支持浏览"),
            "拒绝须点名原因: {e}"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn classify_resume_onlyOn206() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::ACCEPT_RANGES, "bytes".parse().unwrap());
        assert_eq!(classify_resume(206, &h), Resumable::Range);
        assert_eq!(
            classify_resume(200, &reqwest::header::HeaderMap::new()),
            Resumable::Whole
        );
        // 反直觉臂：200 + Accept-Ranges 仍 Whole（没真用 Range 就不能承诺续传）
        assert_eq!(classify_resume(200, &h), Resumable::Whole);
        // Append 只能由 SFTP/FTP 腿显式产出——HTTP 裁决口任何输入都不产它
        assert_ne!(classify_resume(416, &h), Resumable::Append);
    }

    #[test]
    #[allow(non_snake_case)]
    fn range_plan_boundaries() {
        assert_eq!(range_plan(100, 0), Some((0, 99)));
        assert_eq!(range_plan(100, 99), Some((99, 99)));
        assert_eq!(range_plan(100, 100), None, "已齐不再要");
        assert_eq!(
            range_plan(100, 137),
            None,
            "have>total 同样无事可做（禁负数下溢）"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn resumeWhole_restartsFromZeroAndStillReportsProgress() {
        // 桩对 Range 视而不见（恒 200 全量）：声称已有 6 字节的续传必须
        // 从 0 重下，且上报的是**实际写齐**的字节数，不是谎报的旧 have。
        let (port, _rx) = serve(BODY, false);
        let drv = HttpsDriver::new(profile_for(port), default_cfg());
        let dst = tmp_dst("whole");
        std::fs::write(&dst, &BODY[..6]).unwrap();
        let out = drv.download_to("/f", &dst, 6, 0).unwrap();
        assert_eq!(out.resumable, Resumable::Whole);
        assert_eq!(out.start_offset, 0, "Whole 档续传起点必须归零");
        assert_eq!(
            out.bytes_done,
            BODY.len() as u64,
            "进度报的是实际写齐的真值"
        );
        assert_eq!(std::fs::read(&dst).unwrap(), BODY, "整取重写后文件完整");
        std::fs::remove_file(&dst).ok();
        // 纯函数臂：三档起点唯一定义
        assert_eq!(resume_offset(Resumable::Whole, 6), 0);
        assert_eq!(resume_offset(Resumable::Range, 6), 6);
    }

    #[test]
    #[allow(non_snake_case)]
    fn remoteError_messageMasksSecretsAndCrlf() {
        let raw = "GET https://h/f?auth=SECRET1&x=1 failed\r\n evil line\nBearer SECRET2 password=SECRET3";
        let m = remote_error_message(raw);
        assert!(!m.contains("SECRET1"), "query 凭据必须掩: {m}");
        assert!(!m.contains("SECRET2"), "Bearer 令牌必须掩: {m}");
        assert!(!m.contains("SECRET3"), "password= 值必须掩: {m}");
        assert!(
            !m.contains('\r') && !m.contains('\n'),
            "CRLF 必须折叠为单空格（防日志注入）"
        );
        assert!(m.contains("x=1"), "正对照：非凭据 query 值原文保留");
        assert!(m.contains("GET"), "正对照：普通成因不吞字");
        // X-Amz 家族整对掩掉、超长截断收尾、≤200 char
        let long = format!(
            "https://h/f?X-Amz-Credential=AKIASECRET&q=tail {}",
            "呀".repeat(300)
        );
        let m2 = remote_error_message(&long);
        assert!(
            !m2.contains("AKIA"),
            "X-Amz 凭据对必须掩: {}",
            &m2[..80.min(m2.len())]
        );
        assert!(m2.contains("q=tail"), "X-Amz 之后的普通键不连坐");
        assert!(m2.chars().count() <= 200);
        assert!(m2.ends_with("…（原因原文见应用日志）"));
    }

    #[test]
    #[allow(non_snake_case)]
    fn httpsThrottle_shareKbpsSplitsAcrossActiveTransfers() {
        assert_eq!(throttle_share_kbps(300, 3), 100, "总闸/活跃数");
        assert_eq!(throttle_share_kbps(0, 3), 0, "0 = 不限臂");
        assert_eq!(throttle_share_kbps(300, 0), 300, "active=0 不除零");
        assert_eq!(
            throttle_share_kbps(2, 3),
            1,
            "商钳 1：返回 0 会被下游误读成不限"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn httpsDownload_againstFakeServer_resumesWithRange() {
        let (port, rx) = serve(BODY, true);
        let drv = HttpsDriver::new(profile_for(port), default_cfg());
        let dst = tmp_dst("range");

        // ① have=0：第一请求无 Range 头（对"假设服务器支持"的防漂移断言）
        let out = drv.download_to("/f", &dst, 0, 0).unwrap();
        assert_eq!(
            out,
            DownloadOutcome {
                resumable: Resumable::Whole,
                start_offset: 0,
                bytes_done: BODY.len() as u64
            }
        );
        let hit = rx.try_recv().unwrap();
        assert_eq!(hit.request_line, "GET /f HTTP/1.1");
        assert!(hit.range.is_none(), "首请求不得凭空发 Range");

        // ② 半截本地文件 + have=6：第二请求 `Range: bytes=6-` 逐字
        std::fs::write(&dst, &BODY[..6]).unwrap();
        let out = drv.download_to("/f", &dst, 6, 0).unwrap();
        assert_eq!(out.resumable, Resumable::Range, "真 206 才承诺 Range");
        assert_eq!(out.start_offset, 6);
        assert_eq!(out.bytes_done, BODY.len() as u64);
        let hit = rx.try_recv().unwrap();
        assert_eq!(
            hit.range.as_deref(),
            Some("bytes=6-"),
            "续传请求逐字校 Range"
        );
        assert_eq!(std::fs::read(&dst).unwrap(), BODY, "续写拼接后文件完整");
        std::fs::remove_file(&dst).ok();
    }

    #[test]
    #[allow(non_snake_case)]
    fn downloadConsumesConfigKbps_whenNoExplicitBudget() {
        // 配置真源直读臂：max_kbps=0 时下载腿必须吃 `download_kbps` 总闸。
        // 1 kbps（1024 B/s）下 16 字节要停约 15ms——下限断言证明闸真的在链路上，
        // 而不是只有纯函数臂在跑。
        let (port, _rx) = serve(BODY, false);
        let config = Arc::new(parking_lot::RwLock::new(FileConfig {
            download_kbps: 1,
            ..FileConfig::default()
        }));
        let drv = HttpsDriver::new(profile_for(port), config);
        let dst = tmp_dst("cfg-kbps");
        let start = std::time::Instant::now();
        let out = drv.download_to("/f", &dst, 0, 0).unwrap();
        assert_eq!(out.bytes_done, BODY.len() as u64);
        assert!(
            start.elapsed() >= Duration::from_millis(5),
            "总闸 1 kbps 下 16 字节不得零暂停放行: {:?}",
            start.elapsed()
        );
        std::fs::remove_file(&dst).ok();
    }
}
