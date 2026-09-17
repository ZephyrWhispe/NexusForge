//! Docker Engine HTTP over named pipe（docs/impl/06 T6）：
//! `\\.\pipe\docker_engine` + HTTP/1.1（Connection: close 短连接）。
//! Content-Length / chunked 两种响应均支持；v1 同步阻塞实现（IPC 层 spawn_blocking）。

use std::os::windows::ffi::OsStrExt;
use std::time::Duration;

use host_core::error::AppError;
use host_core::ports::{DockerPipePort, HttpResp};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::Foundation::GENERIC_READ;

/// Docker Engine named pipe 路径
const PIPE_PATH: &str = r"\\.\pipe\docker_engine";
/// 读上限（防止异常响应撑爆内存）
const MAX_BODY: usize = 32 * 1024 * 1024;

/// named pipe HTTP 客户端（实现 [`DockerPipePort`]）
pub struct DockerPipeWin;

impl DockerPipeWin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DockerPipeWin {
    fn default() -> Self {
        Self::new()
    }
}

impl DockerPipePort for DockerPipeWin {
    fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<HttpResp, AppError> {
        let pipe_name: Vec<u16> = std::ffi::OsStr::new(PIPE_PATH)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let handle = CreateFileW(
                PCWSTR::from_raw(pipe_name.as_ptr()),
                (GENERIC_READ.0 | GENERIC_WRITE.0) as u32,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
            .map_err(|e| {
                AppError::module(
                    "TERM_DOCKER_001",
                    if e.code().0 == -2147024894 {
                        // ERROR_FILE_NOT_FOUND
                        "Docker Engine 不可用（管道不存在，Docker Desktop 未运行？）".into()
                    } else {
                        format!("打开 docker_engine 管道失败: {e}")
                    },
                    None,
                )
            })?;

            // 请求（短连接：读完即关）
            let mut req = format!(
                "{method} {path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\nContent-Type: application/json\r\n"
            );
            let body_bytes = body.unwrap_or("").as_bytes();
            if !body_bytes.is_empty() {
                req.push_str(&format!("Content-Length: {}\r\n", body_bytes.len()));
            }
            req.push_str("\r\n");
            let mut full = req.into_bytes();
            full.extend_from_slice(body_bytes);

            write_all(handle, &full)?;
            let raw = read_all(handle)?;
            let _ = CloseHandle(handle);
            parse_response(&raw)
        }
    }
}

/// HANDLE 直传（本实现全部在单线程调用栈内，无跨线程捕获）
use windows::Win32::Foundation::HANDLE;

fn write_all(handle: HANDLE, data: &[u8]) -> Result<(), AppError> {
    let mut off = 0;
    while off < data.len() {
        let mut n: u32 = 0;
        unsafe {
            WriteFile(handle, Some(&data[off..]), Some(&mut n), None)
                .map_err(|e| AppError::module("TERM_DOCKER_002", format!("写管道失败: {e}"), None))?;
        }
        if n == 0 {
            return Err(AppError::module("TERM_DOCKER_002", "写管道零字节".to_string(), None));
        }
        off += n as usize;
    }
    Ok(())
}

/// 读到 EOF 或解析完 Content-Length/chunked 由调用方截断——这里读至 EOF（server 关流）
fn read_all(handle: HANDLE) -> Result<Vec<u8>, AppError> {
    let mut out = Vec::with_capacity(16 * 1024);
    let mut buf = [0u8; 16384];
    loop {
        let mut n: u32 = 0;
        let ok = unsafe { ReadFile(handle, Some(&mut buf), Some(&mut n), None).is_ok() };
        if !ok || n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
        if out.len() > MAX_BODY {
            return Err(AppError::module("TERM_DOCKER_003", "响应超上限".to_string(), None));
        }
    }
    Ok(out)
}

/// 解析 HTTP 响应：状态码 + body（Content-Length 截断 / chunked 解码 / EOF 语义）
fn parse_response(raw: &[u8]) -> Result<HttpResp, AppError> {
    let header_end = find(raw, b"\r\n\r\n")
        .ok_or_else(|| AppError::module("TERM_DOCKER_004", "响应缺少头部分隔".to_string(), None))?;
    let header = String::from_utf8_lossy(&raw[..header_end]).into_owned();
    let mut lines = header.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| AppError::module("TERM_DOCKER_004", "响应空".to_string(), None))?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AppError::module("TERM_DOCKER_004", format!("状态行异常: {status_line}"), None))?;

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().ok();
        } else if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }
    let body_raw = &raw[header_end + 4..];
    let body = if chunked {
        decode_chunked(body_raw)
    } else if let Some(len) = content_length {
        body_raw[..len.min(body_raw.len())].to_vec()
    } else {
        body_raw.to_vec()
    };
    Ok(HttpResp { status, body })
}

fn decode_chunked(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        // 找行尾读 chunk 大小
        let Some(line_end) = find(data, b"\r\n") else { break };
        let size_str = String::from_utf8_lossy(&data[..line_end]).into_owned();
        let size = usize::from_str_radix(size_str.trim().split(';').next().unwrap_or("0"), 16)
            .unwrap_or(0);
        data = &data[line_end + 2..];
        if size == 0 {
            break;
        }
        let end = size.min(data.len());
        out.extend_from_slice(&data[..end]);
        data = &data[end + 2..]; // 跳过 chunk 后 \r\n
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_content_length_response() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 10\r\n\r\n{\"a\":1234}";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"{\"a\":1234}");
    }

    #[test]
    fn parse_chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"hello world");
    }

    #[test]
    fn parse_status_line_bad() {
        assert!(parse_response(b"garbage").is_err());
    }

    #[test]
    fn docker_pipe_unavailable_error() {
        // 无 Docker 环境时 request 报错而非 panic（TERM_DOCKER_001/002）
        let c = DockerPipeWin::new();
        match DockerPipePort::request(&c, "GET", "/_ping", None) {
            Ok(r) => assert!(r.status >= 200),
            Err(e) => assert!(e.code().starts_with("TERM_DOCKER")),
        }
    }
}
