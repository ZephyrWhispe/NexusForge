//! T6 Docker 管理（docs/impl/06 T6）：Engine API over named pipe。
//!
//! 容器列表/启停/日志 tail；日志流解析 Docker multiplexed 帧（非 TTY）。

use host_core::error::AppError;
use host_core::ports::DockerPipePort;
use serde::{Deserialize, Serialize};

use crate::error::{Result, TermError};

/// 容器摘要（IPC DTO）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DockerContainer {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
}

fn map_err(e: AppError) -> TermError {
    TermError::Docker(e.to_string())
}

/// 容器列表（all=true 含已停止）
pub fn containers_list(docker: &dyn DockerPipePort) -> Result<Vec<DockerContainer>> {
    let resp = docker
        .request("GET", "/containers/json?all=1", None)
        .map_err(map_err)?;
    if resp.status != 200 {
        return Err(TermError::Docker(format!(
            "containers/json 返回 {}",
            resp.status
        )));
    }
    #[derive(Deserialize)]
    struct Raw {
        #[serde(rename = "Id")]
        id: String,
        #[serde(rename = "Names")]
        names: Vec<String>,
        #[serde(rename = "Image")]
        image: String,
        #[serde(rename = "State")]
        state: String,
        #[serde(rename = "Status")]
        status: String,
    }
    let raw: Vec<Raw> = serde_json::from_slice(&resp.body)
        .map_err(|e| TermError::Docker(format!("解析容器列表失败: {e}")))?;
    Ok(raw
        .into_iter()
        .map(|c| DockerContainer {
            id: c.id.chars().take(12).collect(),
            name: c
                .names
                .first()
                .map(|n| n.trim_start_matches('/').to_string())
                .unwrap_or_default(),
            image: c.image,
            state: c.state,
            status: c.status,
        })
        .collect())
}

/// 启动/停止容器
pub fn container_lifecycle(docker: &dyn DockerPipePort, id: &str, start: bool) -> Result<()> {
    let path = if start {
        format!("/containers/{id}/start")
    } else {
        format!("/containers/{id}/stop")
    };
    let resp = docker
        .request("POST", &path, None)
        .map_err(map_err)?;
    // 204 成功；304 已是目标状态
    if resp.status == 204 || resp.status == 304 {
        Ok(())
    } else {
        Err(TermError::Docker(format!(
            "{} 容器返回 {}",
            if start { "启动" } else { "停止" },
            resp.status
        )))
    }
}

/// 日志（tail 最近 N 行；解析 multiplexed 帧，TTY 回退原文）
pub fn container_logs(docker: &dyn DockerPipePort, id: &str, tail: u32) -> Result<String> {
    let path = format!(
        "/containers/{id}/logs?stdout=1&stderr=1&tail={tail}&timestamps=0"
    );
    let resp = docker.request("GET", &path, None).map_err(map_err)?;
    if resp.status != 200 {
        return Err(TermError::Docker(format!("logs 返回 {}", resp.status)));
    }
    Ok(parse_log_stream(&resp.body))
}

/// 解析 Docker multiplexed 流帧：[type][0][0][0][len:4BE][payload]；
/// 非 TTY 时适用；若首字节不像帧头（>2 或长度不匹配）按原文返回（TTY 模式）
fn parse_log_stream(body: &[u8]) -> String {
    let mut out = String::new();
    let mut rest = body;
    let mut frames = 0usize;
    while rest.len() >= 8 {
        let stream = rest[0];
        if stream > 2 {
            // 非 multiplexed（TTY 原文流）
            return String::from_utf8_lossy(body).into_owned();
        }
        let len = u32::from_be_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let end = 8 + len;
        if end > rest.len() {
            break;
        }
        out.push_str(&String::from_utf8_lossy(&rest[8..end]));
        rest = &rest[end..];
        frames += 1;
    }
    if frames == 0 && !body.is_empty() {
        String::from_utf8_lossy(body).into_owned()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::ports::HttpResp;
    use std::sync::Mutex;

    /// 内存假 Docker（HTTP 解析已单测，这里测 API 封装语义）
    struct FakeDocker {
        responses: Mutex<Vec<HttpResp>>,
        last_path: std::sync::Mutex<String>,
    }
    impl FakeDocker {
        fn new(status: u16, body: &[u8]) -> Self {
            Self {
                responses: Mutex::new(vec![HttpResp { status, body: body.to_vec() }]),
                last_path: std::sync::Mutex::new(String::new()),
            }
        }
    }
    impl DockerPipePort for FakeDocker {
        fn request(&self, _method: &str, path: &str, _body: Option<&str>) -> std::result::Result<HttpResp, AppError> {
            *self.last_path.lock().unwrap() = path.to_string();
            self.responses
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| AppError::module("TERM_DOCKER_001", "no response", None))
        }
    }

    #[test]
    fn containers_list_parses() {
        let body = br#"[{"Id":"abcdef1234567890","Names":["/web"],"Image":"nginx","State":"running","Status":"Up 2 hours"}]"#;
        let d = FakeDocker::new(200, body);
        let list = containers_list(&d).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "web");
        assert_eq!(list[0].id, "abcdef123456");
        assert_eq!(list[0].state, "running");
        // all=1 参数
        assert!(d.last_path.lock().unwrap().contains("all=1"));
    }

    #[test]
    fn lifecycle_maps_status() {
        let d = FakeDocker::new(204, b"");
        assert!(container_lifecycle(&d, "abc", true).is_ok());
        assert!(d.last_path.lock().unwrap().contains("/start"));
        // 304 = 已是目标状态
        let d = FakeDocker::new(304, b"");
        assert!(container_lifecycle(&d, "abc", false).is_ok());
        // 500 报错
        let d = FakeDocker::new(500, b"boom");
        assert!(container_lifecycle(&d, "abc", true).is_err());
    }

    #[test]
    fn logs_multiplexed_and_tty_fallback() {
        // multiplexed：帧 stdout "hello\n"
        let mut body = vec![0u8, 0, 0, 0];
        body.extend_from_slice(&6u32.to_be_bytes());
        body.extend_from_slice(b"hello\n");
        // 帧 stderr "err"
        body.push(1);
        body.extend_from_slice(&[0, 0, 0]);
        body.extend_from_slice(&3u32.to_be_bytes());
        body.extend_from_slice(b"err");
        assert_eq!(parse_log_stream(&body), "hello\nerr");

        // TTY 原文（首字节 > 2）
        assert_eq!(parse_log_stream(b"raw output"), "raw output");
    }

    #[test]
    fn logs_error_status() {
        let d = FakeDocker::new(404, b"no such container");
        assert!(container_logs(&d, "abc", 100).is_err());
    }
}
