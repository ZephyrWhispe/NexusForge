//! PR1 内核抽象（docs/impl/05 PR1）：`KernelDriver` trait + `KernelHandle`。
//!
//! - sing-box 首选；内核为 Sidecar（PR2 按需下载，不打包分发）
//! - KernelHandle：停止 / 日志环形缓冲 / 健康检查 / 意外退出回调（守护线程）
//! - 意外退出语义：回调由 [`ProxyService`](crate::service) 挂接 —— 立即还原系统代理
//!   （内核死亡 + 系统代理仍指向死端口 = 用户断网最高危场景）

use std::collections::VecDeque;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{ProxyError, Result};

const LOG_CAP: usize = 500;

type LogCb = Arc<dyn Fn(&str) + Send + Sync>;
type ExitCb = Arc<dyn Fn(i32) + Send + Sync>;

/// 内核日志行（环形缓冲快照 / proxy.log_line 事件载荷）
#[derive(Clone, Debug, serde::Serialize)]
pub struct LogLine {
    pub ts_ms: u64,
    pub text: String,
}

impl LogLine {
    fn now(text: impl Into<String>) -> Self {
        Self {
            ts_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            text: text.into(),
        }
    }
}

/// 内核驱动抽象（PR1）。实现方负责进程 spawn 与日志采集。
pub trait KernelDriver: Send + Sync {
    fn id(&self) -> &'static str;
    fn exe_path(&self) -> PathBuf;
    /// 启动内核。`on_exit` 在进程非预期退出（非 [`stop`](KernelHandle::stop) 触发）时以退出码回调。
    fn start(&self, cfg: &Path, on_exit: ExitCb) -> Result<KernelHandle>;
}

/// 内核句柄：停止 / 存活 / 日志快照 / 健康检查。
pub struct KernelHandle {
    driver_id: &'static str,
    stopped: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    logs: Arc<Mutex<VecDeque<LogLine>>>,
    /// 与日志采集线程共享的回调槽：set_log_cb 写入，采集线程每行读取
    cb_slot: Arc<Mutex<Option<LogCb>>>,
    guard: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl KernelHandle {
    pub fn driver_id(&self) -> &'static str {
        self.driver_id
    }

    /// 挂接日志回调（每行调用；service 内转 proxy.log_line 事件）。启动前后均可挂。
    pub fn set_log_cb(&self, cb: LogCb) {
        *self.cb_slot.lock().expect("日志回调槽锁") = Some(cb);
    }

    /// 停止内核（守护线程 kill + wait 后返回；不触发 on_exit）
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(g) = self.guard.lock().expect("守护线程句柄锁").take() {
            let _ = g.join();
        }
    }

    /// 进程是否仍在运行（守护线程未结束）
    pub fn alive(&self) -> bool {
        !self.finished.load(Ordering::SeqCst)
    }

    pub fn logs_snapshot(&self, limit: usize) -> Vec<LogLine> {
        let logs = self.logs.lock().expect("日志缓冲锁");
        let skip = logs.len().saturating_sub(limit);
        logs.iter().skip(skip).cloned().collect()
    }

    /// 健康检查：入站端口可 TCP 连通即视为可用（v1 轻量探活）
    pub fn health_check(&self, inbound_port: u16) -> bool {
        TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], inbound_port)),
            Duration::from_millis(1000),
        )
        .is_ok()
    }
}

impl Drop for KernelHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// spawn 子进程 + 守护线程 + 日志采集线程的共用装配（sing-box 与测试驱动共用）
pub(super) fn spawn_kernel(
    driver_id: &'static str,
    mut cmd: Command,
    on_exit: ExitCb,
) -> Result<KernelHandle> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child: Child = cmd
        .spawn()
        .map_err(|e| ProxyError::Kernel(format!("内核启动失败: {e}")))?;

    let logs = Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP)));
    let cb_slot: Arc<Mutex<Option<LogCb>>> = Arc::new(Mutex::new(None));
    let stopped = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));

    // 日志采集：stdout/stderr 各一线程 → 环形缓冲（cap 500）+ 回调转发
    if let Some(out) = child.stdout.take() {
        spawn_log_reader("proxy-kernel-log-out", logs.clone(), cb_slot.clone(), out);
    }
    if let Some(err_pipe) = child.stderr.take() {
        spawn_log_reader("proxy-kernel-log-err", logs.clone(), cb_slot.clone(), err_pipe);
    }

    // 守护线程：持有 Child 唯一所有权，50ms 轮询（stop 置位 → kill；自行退出 → on_exit）
    let guard_stopped = stopped.clone();
    let guard_finished = finished.clone();
    let guard = std::thread::Builder::new()
        .name("proxy-kernel-guard".into())
        .spawn(move || {
            guard_finished.store(false, Ordering::SeqCst);
            loop {
                if guard_stopped.load(Ordering::SeqCst) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        guard_finished.store(true, Ordering::SeqCst);
                        if !guard_stopped.load(Ordering::SeqCst) {
                            on_exit(status.code().unwrap_or(-1));
                        }
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break,
                }
            }
            guard_finished.store(true, Ordering::SeqCst);
        })
        .map_err(|e| ProxyError::Kernel(format!("守护线程启动失败: {e}")))?;

    Ok(KernelHandle {
        driver_id,
        stopped,
        finished,
        logs,
        cb_slot,
        guard: Mutex::new(Some(guard)),
    })
}

fn spawn_log_reader(
    name: &'static str,
    logs: Arc<Mutex<VecDeque<LogLine>>>,
    cb_slot: Arc<Mutex<Option<LogCb>>>,
    pipe: impl std::io::Read + Send + 'static,
) {
    let _ = std::thread::Builder::new().name(name.into()).spawn(move || {
        use std::io::BufRead;
        let reader = std::io::BufReader::new(pipe);
        for line in reader.lines().map_while(std::result::Result::ok) {
            {
                let mut buf = logs.lock().expect("日志缓冲锁");
                if buf.len() >= LOG_CAP {
                    buf.pop_front();
                }
                buf.push_back(LogLine::now(line.clone()));
            }
            if let Some(cb) = cb_slot.lock().expect("日志回调槽锁").clone() {
                cb(&line);
            }
        }
    });
}

/// sing-box 内核驱动（PR1 首选实现）。exe 由 PR2 Sidecar 安装到 `{appData}/proxy/bin/`。
pub struct SingBoxDriver {
    exe: PathBuf,
}

impl SingBoxDriver {
    pub fn new(exe: PathBuf) -> Self {
        Self { exe }
    }
}

impl KernelDriver for SingBoxDriver {
    fn id(&self) -> &'static str {
        "sing-box"
    }

    fn exe_path(&self) -> PathBuf {
        self.exe.clone()
    }

    fn start(&self, cfg: &Path, on_exit: ExitCb) -> Result<KernelHandle> {
        if !self.exe.is_file() {
            return Err(ProxyError::Kernel(format!(
                "内核未安装: {}（请在代理页安装内核）",
                self.exe.display()
            )));
        }
        let mut cmd = Command::new(&self.exe);
        cmd.arg("run")
            .arg("-c")
            .arg(cfg)
            // sing-box 1.10+ 关闭彩色输出，日志按行解析更稳
            .arg("--disable-color");
        spawn_kernel("sing-box", cmd, on_exit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试驱动：跑 cmd 子进程（Windows 专项）
    struct CmdDriver(Arc<dyn Fn() -> Command + Send + Sync>);

    impl KernelDriver for CmdDriver {
        fn id(&self) -> &'static str {
            "cmd"
        }
        fn exe_path(&self) -> PathBuf {
            PathBuf::from("cmd.exe")
        }
        fn start(&self, _cfg: &Path, on_exit: ExitCb) -> Result<KernelHandle> {
            spawn_kernel("cmd", (self.0)(), on_exit)
        }
    }

    fn cmd_driver(args: &[&str]) -> CmdDriver {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        CmdDriver(Arc::new(move || {
            let mut c =
                Command::new(std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()));
            c.arg("/C").args(&args);
            c
        }))
    }

    #[test]
    fn handle_reports_unexpected_exit_code() {
        let fired = Arc::new(Mutex::new(None::<i32>));
        let fired2 = fired.clone();
        let h = cmd_driver(&["exit 7"])
            .start(Path::new("unused"), Arc::new(move |code| {
                *fired2.lock().unwrap() = Some(code);
            }))
            .unwrap();
        // 轮询等待守护线程观察到退出（cmd /C exit 7 立即结束）
        for _ in 0..100 {
            if !h.alive() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!h.alive(), "进程应已退出");
        assert_eq!(*fired.lock().unwrap(), Some(7), "on_exit 应收到退出码 7");
    }

    #[test]
    fn stop_does_not_fire_on_exit() {
        // ping -n 3 约 2 秒，给 stop 留出窗口
        let fired = Arc::new(Mutex::new(false));
        let fired2 = fired.clone();
        let h = cmd_driver(&["ping", "-n", "3", "127.0.0.1"])
            .start(Path::new("unused"), Arc::new(move |_| {
                *fired2.lock().unwrap() = true;
            }))
            .unwrap();
        assert!(h.alive());
        h.stop();
        assert!(!h.alive(), "stop 后守护线程应已结束");
        std::thread::sleep(Duration::from_millis(100));
        assert!(!*fired.lock().unwrap(), "主动 stop 不应触发 on_exit");
    }

    #[test]
    fn log_ring_buffer_caps_at_500() {
        // echo 3000 行：超过 LOG_CAP，快照应只有最后 500 行
        let h = cmd_driver(&["for /L %i in (1,1,3000) do @echo line%i"])
            .start(Path::new("unused"), Arc::new(|_| {}))
            .unwrap();
        for _ in 0..100 {
            if h.logs_snapshot(LOG_CAP).len() >= LOG_CAP {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        h.stop();
        let snap = h.logs_snapshot(LOG_CAP);
        assert!(snap.len() <= LOG_CAP, "环形缓冲不得超过上限");
        if snap.len() == LOG_CAP {
            assert!(snap.last().unwrap().text.starts_with("line"));
        }
    }
}
