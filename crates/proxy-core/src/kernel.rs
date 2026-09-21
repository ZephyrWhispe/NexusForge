//! PR1 内核抽象（docs/impl/05 PR1）：`KernelDriver` trait + `KernelHandle`。
//!
//! - sing-box 首选；内核为 Sidecar（PR2 按需下载，不打包分发）
//! - KernelHandle：停止 / 日志环形缓冲 / 健康检查 / 意外退出回调（守护线程）
//! - 意外退出语义：回调由 [`ProxyService`](crate::service) 挂接 —— 立即还原系统代理
//!   （内核死亡 + 系统代理仍指向死端口 = 用户断网最高危场景）

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::error::{ProxyError, Result};
use crate::ir::{IrConfig, IrInbound};
use crate::sub::NodeKind;

const LOG_CAP: usize = 500;

/// 内核日志行回调（proxy.log_line 事件转发的源头）
pub type LogCb = Arc<dyn Fn(&str) + Send + Sync>;
/// 内核意外退出回调（非 stop 触发的进程结束）
pub type ExitCb = Arc<dyn Fn(i32) + Send + Sync>;

/// 内核能力表（单一真源：UI 只按 caps 渲染，不支持的参数组整组折叠，禁"能看见点不动"）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelCaps {
    pub tun: bool,
    pub policy_groups: bool,
    pub external_controller: bool,
}

/// 内核日志行（环形缓冲快照 / proxy.log_line 事件载荷）
#[derive(Clone, Debug, serde::Serialize)]
pub struct LogLine {
    pub ts_ms: u64,
    pub text: String,
}

impl LogLine {
    fn now(text: impl Into<String>) -> Self {
        Self {
            ts_ms: host_core::util::now_ms_u64(),
            text: text.into(),
        }
    }
}

/// 内核驱动抽象（B2 T-B2-2 trait v2）。实现方负责方言渲染与进程 spawn 参数；
/// 生命周期装配（渲染→写 `work_dir/cfg_name()`→spawn+守护+日志）由默认 [`start`](KernelDriver::start) 承担。
pub trait KernelDriver: Send + Sync {
    fn id(&self) -> &'static str;
    fn exe_path(&self) -> PathBuf;
    /// UI 展示名（内核卡标题）
    fn display_name(&self) -> &'static str;
    /// 配置文件名（同目录并存互踩的根源参数：sing-box 恒 `config.json` 保旧路径）
    fn cfg_name(&self) -> &'static str;
    /// 该内核可渲染的协议族（换核前协议兼容性预检的输入）
    fn supported_kinds(&self) -> &'static [NodeKind];
    fn caps(&self) -> KernelCaps;
    /// IR → 方言配置文本；Err = 校验失败（渲染即校验，validate_config 不单列 trait 方法）
    fn config_render(&self, ir: &IrConfig) -> Result<String>;
    /// 组装进程命令（`work_dir` 供 mihomo 类 `-d` 语义；`cfg` 为已写盘配置路径）
    fn build_command(&self, work_dir: &Path, cfg: &Path) -> Command;
    /// 健康探活端口（T-B2-5）：默认 = mixed 入站端口（sing-box 直接监听该端口；
    /// xray 的 http 入站恒等于 mixed_port，见 xray.rs `xrayDualInbound_probePortHttp`）。
    /// TUN 入站无本地端口 → 0（service 侧走 alive() 分支，此值不被消费）。
    fn probe_port(&self, ir: &IrConfig) -> u16 {
        match &ir.inbound {
            IrInbound::Mixed { port, .. } => *port,
            IrInbound::Tun { .. } => 0,
        }
    }

    /// 启动内核。`on_exit` 在进程非预期退出（非 [`stop`](KernelHandle::stop) 触发）时以退出码回调。
    fn start(&self, work_dir: &Path, cfg: &Path, on_exit: ExitCb) -> Result<KernelHandle> {
        let exe = self.exe_path();
        if !exe.is_file() {
            return Err(ProxyError::Kernel(format!(
                "内核未安装: {}（请在代理页安装内核）",
                exe.display()
            )));
        }
        let cmd = self.build_command(work_dir, cfg);
        spawn_kernel(self.id(), cmd, on_exit)
    }
}

/// 已注册内核全集（T-B2-5/6 新增驱动 = 此处加臂 + sidecar AssetSpec 同步）
pub const KERNEL_IDS: &[&str] = &["sing-box", "xray", "mihomo"];

/// 内核注册表：id → 驱动实例（exe 可以不存在——installed 由调用方按 exe_path 判定）。
pub fn driver_for(bin_dir: &Path, id: &str) -> Result<Arc<dyn KernelDriver>> {
    match id {
        "sing-box" => Ok(Arc::new(SingBoxDriver::new(bin_dir.join("sing-box.exe")))),
        "xray" => Ok(Arc::new(crate::xray::XrayDriver::new(
            bin_dir.join("xray.exe"),
        ))),
        "mihomo" => Ok(Arc::new(crate::mihomo::MihomoDriver::new(
            bin_dir.join("mihomo.exe"),
        ))),
        other => Err(ProxyError::Kernel(format!("未知内核: {other}"))),
    }
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
        *self.cb_slot.lock() = Some(cb);
    }

    /// 停止内核（守护线程 kill + wait 后返回；不触发 on_exit）
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(g) = self.guard.lock().take() {
            let _ = g.join();
        }
    }

    /// 进程是否仍在运行（守护线程未结束）
    pub fn alive(&self) -> bool {
        !self.finished.load(Ordering::SeqCst)
    }

    pub fn logs_snapshot(&self, limit: usize) -> Vec<LogLine> {
        let logs = self.logs.lock();
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

/// spawn 子进程 + 守护线程 + 日志采集线程的共用装配（各方言驱动与测试驱动共用）
pub fn spawn_kernel(
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
        spawn_log_reader(
            "proxy-kernel-log-err",
            logs.clone(),
            cb_slot.clone(),
            err_pipe,
        );
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
    let _ = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(pipe);
            for line in reader.lines().map_while(std::result::Result::ok) {
                {
                    let mut buf = logs.lock();
                    if buf.len() >= LOG_CAP {
                        buf.pop_front();
                    }
                    buf.push_back(LogLine::now(line.clone()));
                }
                if let Some(cb) = cb_slot.lock().clone() {
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

    fn display_name(&self) -> &'static str {
        "sing-box"
    }

    /// 恒 `config.json`：保 B2 前旧落盘路径一字不动（golden 同链）
    fn cfg_name(&self) -> &'static str {
        "config.json"
    }

    fn supported_kinds(&self) -> &'static [NodeKind] {
        use NodeKind::*;
        // T-B2-7：sing-box 官方主线 7 协议；SSR 恒排除（⑭ 核证：官方不支持）
        &[
            Shadowsocks,
            Vmess,
            Trojan,
            Vless,
            Hysteria2,
            Tuic5,
            WireGuard,
        ]
    }

    fn caps(&self) -> KernelCaps {
        KernelCaps {
            tun: true,
            policy_groups: true,
            external_controller: true,
        }
    }

    fn config_render(&self, ir: &IrConfig) -> Result<String> {
        Ok(serde_json::to_string_pretty(&crate::singbox::render(ir)?)?)
    }

    fn build_command(&self, _work_dir: &Path, cfg: &Path) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.arg("run")
            .arg("-c")
            .arg(cfg)
            // sing-box 1.10+ 关闭彩色输出，日志按行解析更稳
            .arg("--disable-color");
        cmd
    }
}

#[cfg(test)]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
mod tests {
    use super::*;
    use crate::ir::IrConfig;

    /// 测试驱动：跑 cmd 子进程（Windows 专项）；trait v2 只留方言三件事
    struct CmdDriver(Arc<dyn Fn() -> Command + Send + Sync>);

    impl KernelDriver for CmdDriver {
        fn id(&self) -> &'static str {
            "cmd"
        }
        fn exe_path(&self) -> PathBuf {
            PathBuf::from(std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()))
        }
        fn display_name(&self) -> &'static str {
            "cmd 测试驱动"
        }
        fn cfg_name(&self) -> &'static str {
            "config.json"
        }
        fn supported_kinds(&self) -> &'static [NodeKind] {
            &[]
        }
        fn caps(&self) -> KernelCaps {
            KernelCaps::default()
        }
        fn config_render(&self, _ir: &IrConfig) -> Result<String> {
            Ok("{}\n".into())
        }
        fn build_command(&self, _work_dir: &Path, _cfg: &Path) -> Command {
            (self.0)()
        }
    }

    fn cmd_driver(args: &[&str]) -> CmdDriver {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        CmdDriver(Arc::new(move || {
            let mut c = Command::new(std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()));
            c.arg("/C").args(&args);
            c
        }))
    }

    #[test]
    fn registry_unknownKernel_rejected() {
        let dir = std::path::Path::new("");
        match driver_for(dir, "v2ray-classic") {
            Err(ProxyError::Kernel(msg)) => {
                assert!(
                    msg.contains("未知内核") && msg.contains("v2ray-classic"),
                    "{msg}"
                );
            }
            // dyn KernelDriver 无 Debug，match 只拆 Err 侧（ProxyError 有 Debug）
            Err(other) => panic!("必须是 Kernel 形态错误，得 {other:?}"),
            Ok(_) => panic!("未注册内核必须拒绝"),
        }
        // 正对照：sing-box 允许 exe 不存在地构造（installed 判定归调用方）
        // expect 而非 unwrap：dyn KernelDriver 无 Debug，unwrap 的 T: Debug 约束不成立
        let d = driver_for(dir, "sing-box").expect("sing-box 必须可构造");
        assert_eq!(d.id(), "sing-box");
        assert_eq!(d.cfg_name(), "config.json");
        // T-B2-7：sing-box 渲染臂已扩至 7 协议（ss/vmess/trojan/vless/hy2/tuic/wg）
        assert_eq!(d.supported_kinds().len(), 7);
    }

    #[test]
    fn registry_xrayArm_landsSecondKernel() {
        // T-B2-5 完成判据：注册表第二臂（第三臂 mihomo 由 T-B2-6 落地，见下方三臂齐测）
        let dir = std::path::Path::new("");
        assert!(KERNEL_IDS.contains(&"xray"));
        let d = driver_for(dir, "xray").expect("xray 必须可构造");
        assert_eq!(d.id(), "xray");
        assert_eq!(d.display_name(), "Xray-core");
        assert_eq!(d.cfg_name(), "config-xray.json");
        assert_eq!(d.supported_kinds().len(), 4);
        let caps = d.caps();
        assert!(
            !caps.tun && !caps.policy_groups && !caps.external_controller,
            "xray 能力表如实全关（09 §5.1-⑭ 核证）"
        );
    }

    #[test]
    fn registry_mihomoArm_thirdKernelLanded() {
        // T-B2-6 完成判据「三内核注册表齐」：三 id 全可解析
        let dir = std::path::Path::new("");
        assert_eq!(KERNEL_IDS, &["sing-box", "xray", "mihomo"]);
        for id in KERNEL_IDS {
            driver_for(dir, id)
                .unwrap_or_else(|_| panic!("{id} 必须可构造（exe 存在性归 installed 判定）"));
        }
        let d = driver_for(dir, "mihomo").expect("mihomo 必须可构造");
        assert_eq!(d.id(), "mihomo");
        assert_eq!(d.display_name(), "mihomo (Clash 内核)");
        // 09 ④ 裁定：文件名段=config.yaml，目录段=proxy/mihomo/ 子目录
        assert_eq!(d.cfg_name(), "mihomo/config.yaml");
        // T-B2-7：三驱动 supported_kinds 表与渲染臂同步（Meta 七协议 / xray 四协议）
        assert_eq!(d.supported_kinds().len(), 7);
        let x = driver_for(dir, "xray").expect("xray 必须可构造");
        assert_eq!(x.supported_kinds().len(), 4);
        let caps = d.caps();
        assert!(
            caps.tun && caps.policy_groups && caps.external_controller,
            "mihomo 能力表全开（09 §5.2 行字面）"
        );
    }

    #[test]
    fn handle_reports_unexpected_exit_code() {
        let fired = Arc::new(Mutex::new(None::<i32>));
        let fired2 = fired.clone();
        let h = cmd_driver(&["exit 7"])
            .start(
                Path::new("unused"),
                Path::new("unused"),
                Arc::new(move |code| {
                    *fired2.lock() = Some(code);
                }),
            )
            .unwrap();
        // 轮询等待守护线程观察到退出（cmd /C exit 7 立即结束）
        for _ in 0..100 {
            if !h.alive() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!h.alive(), "进程应已退出");
        assert_eq!(*fired.lock(), Some(7), "on_exit 应收到退出码 7");
    }

    #[test]
    fn stop_does_not_fire_on_exit() {
        // ping -n 3 约 2 秒，给 stop 留出窗口
        let fired = Arc::new(Mutex::new(false));
        let fired2 = fired.clone();
        let h = cmd_driver(&["ping", "-n", "3", "127.0.0.1"])
            .start(
                Path::new("unused"),
                Path::new("unused"),
                Arc::new(move |_| {
                    *fired2.lock() = true;
                }),
            )
            .unwrap();
        assert!(h.alive());
        h.stop();
        assert!(!h.alive(), "stop 后守护线程应已结束");
        std::thread::sleep(Duration::from_millis(100));
        assert!(!*fired.lock(), "主动 stop 不应触发 on_exit");
    }

    #[test]
    fn log_ring_buffer_caps_at_500() {
        // echo 3000 行：超过 LOG_CAP，快照应只有最后 500 行
        let h = cmd_driver(&["for /L %i in (1,1,3000) do @echo line%i"])
            .start(Path::new("unused"), Path::new("unused"), Arc::new(|_| {}))
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
