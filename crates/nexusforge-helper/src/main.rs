//! nexusforge-sys-helper：WinOps 提权辅助进程（docs/impl/08 §4 W3）。
//!
//! 生命周期：主进程 ShellExecuteExW(runas) 拉起（UAC 弹窗）→ 单实例命名管道
//! `\\.\pipe\nexusforge-sys-helper-v1-{parent_pid}` → 逐连接服务 JSON-RPC
//! → 空闲（无连接/无请求）120s 看门狗自退；下次提权操作由主进程重新拉起。
//!
//! 安全（全部强制）：
//! - token：spawn 时经 `NF_HELPER_TOKEN` 环境变量注入，缺失即拒绝启动（防手动拉起）
//! - 握手：首帧 hello 携带 token + `GetNamedPipeClientProcessId` 镜像路径必须与自身同目录
//! - 方法白名单（dispatch.rs）；不写磁盘日志（审计由主进程承担）；崩溃 dump 禁用

#![windows_subsystem = "windows"]

mod dispatch;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use win_integration::helper::{self as h, PipeHandle};

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 命令行参数解析（--flag value 形式）
fn arg_value(args: &[String], flag: &str) -> Option<u32> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok())
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let parent_pid = arg_value(&args, "--parent-pid").ok_or("缺少 --parent-pid 参数")?;
    let idle_secs = arg_value(&args, "--idle-exit").unwrap_or(120);
    // token：spawn 时环境变量注入；缺失即拒绝（非主进程拉起/调试直启）
    let token = std::env::var(h::TOKEN_ENV).map_err(|_| format!("{} 缺失（拒绝启动）", h::TOKEN_ENV))?;
    let self_exe = std::env::current_exe().map_err(|e| format!("current_exe 失败: {e}"))?;
    // 调用方预校验（纵深防御，token 是主要防线）：parent 镜像必须与本 helper 同目录
    if let Ok(parent_image) = h::query_image_path(parent_pid) {
        if !h::same_dir(&self_exe, &parent_image) {
            return Err("调用方镜像与本 helper 不同目录（拒绝服务）".into());
        }
    }
    // 单实例互斥：同名管道已存在 → 创建失败 → 退出
    let server = h::pipe_server_create(&h::pipe_name_for(parent_pid)).map_err(|e| e.to_string())?;
    // 看门狗：last_activity 超 idle_secs → 自退（同步 ConnectNamedPipe 阻塞等待中亦计空闲）
    let last_activity = Arc::new(AtomicU64::new(now_ms()));
    {
        let last = last_activity.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            let idle = now_ms().saturating_sub(last.load(Ordering::Relaxed));
            if idle > idle_secs as u64 * 1000 {
                std::process::exit(0);
            }
        });
    }
    let ops = dispatch::Ops::new();
    loop {
        if h::pipe_accept(server).is_err() {
            h::pipe_disconnect(server);
            last_activity.store(now_ms(), Ordering::Relaxed);
            continue;
        }
        last_activity.store(now_ms(), Ordering::Relaxed);
        let _ = serve_connection(server, &token, &self_exe, &ops);
        h::pipe_disconnect(server);
        last_activity.store(now_ms(), Ordering::Relaxed);
    }
}

/// 单连接服务：握手（hello token + 调用方镜像）→ JSON-RPC 请求循环（EOF/断开 → 正常结束）
fn serve_connection(
    hnd: PipeHandle,
    token: &str,
    self_exe: &std::path::Path,
    ops: &dispatch::Ops,
) -> Result<(), String> {
    // 1. 握手：首帧 hello{token}
    let hello = h::read_frame(hnd).map_err(|e| e.to_string())?;
    let msg: Value = serde_json::from_slice(&hello).map_err(|e| e.to_string())?;
    let token_ok = msg["params"]["token"].as_str() == Some(token);
    let caller_ok = h::client_pid(hnd)
        .ok()
        .and_then(|pid| h::query_image_path(pid).ok())
        .map(|img| h::same_dir(self_exe, &img))
        .unwrap_or(false);
    if !token_ok || !caller_ok {
        let _ = h::write_frame(
            hnd,
            json!({"jsonrpc":"2.0","id":0,"error":{"code":-32001,"message":"FORBIDDEN"}})
                .to_string()
                .as_bytes(),
        );
        // 等 client 读走响应后自行断开（立即 disconnect 会丢弃未读输出缓冲）
        let _ = h::read_frame(hnd);
        return Err("握手失败（token 或调用方校验不通过）".into());
    }
    let _ = h::write_frame(hnd, json!({"jsonrpc":"2.0","id":0,"result":{"ready":true}}).to_string().as_bytes());
    // 2. 请求循环：read → dispatch → write；对端断开即结束本连接
    loop {
        let frame = match h::read_frame(hnd) {
            Ok(f) => f,
            Err(_) => return Ok(()),
        };
        let msg: Value = match serde_json::from_slice(&frame) {
            Ok(m) => m,
            Err(e) => return Err(format!("请求解析失败: {e}")),
        };
        // serde_json 索引缺失返回 Value::Null，无需 unwrap_or
        let id = msg["id"].clone();
        let method = msg["method"].as_str().unwrap_or("").to_string();
        let params = if msg["params"].is_null() { json!({}) } else { msg["params"].clone() };
        let resp = match dispatch::handle_request(ops, &method, params) {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(message) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":message}}),
        };
        h::write_frame(hnd, resp.to_string().as_bytes()).map_err(|e| e.to_string())?;
    }
}

fn main() {
    // 无窗口进程：失败静默退出（错误经退出码传达；不留痕磁盘）
    if run().is_err() {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use win_integration::helper as wh;

    /// 协议集成回环：同进程起 mini server（accept + serve）→ client 走完整握手 + 请求
    #[test]
    fn pipe_protocol_roundtrip() {
        let token = "test-token-0123456789abcdef0123456789abcdef".to_string();
        let pipe = format!(r"\\.\pipe\nexusforge-sys-helper-test-{}", std::process::id());
        let self_exe = std::env::current_exe().unwrap();
        let ops = dispatch::Ops::new();
        // server：线程内创建管道（HANDLE 非 Send 不跨线程），accept 一次并服务一个连接
        let tok = token.clone();
        let exe = self_exe.clone();
        let pipe_s = pipe.clone();
        let server_thread = std::thread::spawn(move || {
            let server = wh::pipe_server_create(&pipe_s).unwrap();
            wh::pipe_accept(server).unwrap();
            let _ = serve_connection(server, &tok, &exe, &ops);
            wh::pipe_disconnect(server);
            wh::close_handle(server);
        });
        // client：连接（自带重试等 server 就绪）→ hello（对 token）→ registry.write → read
        let hnd = wh::pipe_connect(&pipe, 3000).unwrap();
        wh::write_frame(
            hnd,
            json!({"jsonrpc":"2.0","id":0,"method":"hello","params":{"token": token}}).to_string().as_bytes(),
        )
        .unwrap();
        let resp = wh::read_frame(hnd).unwrap();
        let v: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(v["result"]["ready"], true, "握手应通过（测试进程与自身同目录）");
        // 写 → 读（HKCU 测试键）
        let key = format!(r"HKCU\Software\NexusForgeHelperPipeTest\{}", std::process::id());
        wh::write_frame(
            hnd,
            json!({"jsonrpc":"2.0","id":1,"method":"registry.write",
                   "params":{"key": key, "value_name":"v","value":{"dword":42}}})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        let resp = wh::read_frame(hnd).unwrap();
        let v: Value = serde_json::from_slice(&resp).unwrap();
        assert!(v.get("result").is_some(), "写应成功: {v}");
        wh::write_frame(
            hnd,
            json!({"jsonrpc":"2.0","id":2,"method":"registry.read",
                   "params":{"key": key, "value_name":"v"}})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        let resp = wh::read_frame(hnd).unwrap();
        let v: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(v["result"]["existed"], true);
        assert_eq!(v["result"]["value"]["dword"], 42);
        // 白名单外 → error
        wh::write_frame(
            hnd,
            json!({"jsonrpc":"2.0","id":3,"method":"exec.dism","params":{}}).to_string().as_bytes(),
        )
        .unwrap();
        let resp = wh::read_frame(hnd).unwrap();
        let v: Value = serde_json::from_slice(&resp).unwrap();
        assert!(v.get("error").is_some(), "白名单外方法必须拒绝");
        // 清理测试键 + 结束首连接
        wh::write_frame(
            hnd,
            json!({"jsonrpc":"2.0","id":4,"method":"registry.delete",
                   "params":{"key": key, "value_name":"v"}})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        let _ = wh::read_frame(hnd);
        wh::close_handle(hnd);
        server_thread.join().unwrap();
        // 错误 token → 拒绝（独立连接 + 独立 server 实例）
        let pipe2 = format!(r"\\.\pipe\nexusforge-sys-helper-test-bad-{}", std::process::id());
        let tok = "real-token".to_string();
        let exe2 = self_exe.clone();
        let ops2 = dispatch::Ops::new();
        let pipe2_c = pipe2.clone();
        let t2 = std::thread::spawn(move || {
            let server2 = wh::pipe_server_create(&pipe2_c).unwrap();
            wh::pipe_accept(server2).unwrap();
            let _ = serve_connection(server2, &tok, &exe2, &ops2);
            wh::pipe_disconnect(server2);
            wh::close_handle(server2);
        });
        let hnd2 = wh::pipe_connect(&pipe2, 3000).unwrap();
        wh::write_frame(
            hnd2,
            json!({"jsonrpc":"2.0","id":0,"method":"hello","params":{"token":"wrong"}}).to_string().as_bytes(),
        )
        .unwrap();
        let resp = wh::read_frame(hnd2).unwrap();
        let v: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(v["error"]["message"], "FORBIDDEN", "错误 token 必须拒绝");
        wh::close_handle(hnd2);
        t2.join().unwrap();
    }
}
