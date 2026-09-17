//! 阶段三验收 1（docs/impl/06 尾部清单）：终端 10k 行/秒输出不掉字、不卡 UI；
//! 关闭窗口无残留（kill → EOF → shutdown 序列无 panic）。

use std::time::{Duration, Instant};

use host_core::ports::{ConptyPort, TermCfg};
use win_integration::conpty::ConptyWin;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要真实桌面会话：无头 agent 环境下 ConPTY 渲染链静默失效（进程层正常、conhost 活但零渲染输出）；人工在桌面会话 `cargo test -p win-integration --test conpty_acceptance -- --ignored` 验证"]
async fn conpty_10k_lines_throughput_and_integrity() {
    let conpty = ConptyWin::new();
    let mut handle = conpty
        .spawn(TermCfg {
            shell: "cmd.exe /c for /l %i in (1,1,10000) do @echo LINE_%i".into(),
            cwd: None,
            cols: 120,
            rows: 40,
            env: Default::default(),
        })
        .expect("spawn 失败");

    let started = Instant::now();
    let mut collected: Vec<u8> = Vec::with_capacity(512 * 1024);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        assert!(!remain.is_zero(), "30s 内未收到 EOF（终端无响应）");
        match tokio::time::timeout(remain, handle.output_rx.recv()).await {
            Ok(Some(chunk)) => collected.extend_from_slice(&chunk),
            Ok(None) => break, // EOF：会话结束
            Err(_) => panic!("30s 内未收到 EOF（终端无响应）"),
        }
    }
    let elapsed = started.elapsed();

    // ConPTY 输出为全屏渲染流（行间用 VT 光标定位而非 \r\n），按标记出现次数验证完整性
    let text = String::from_utf8_lossy(&collected);
    assert!(text.contains("LINE_1"), "首行缺失");
    assert!(text.contains("LINE_10000"), "末行缺失（掉字）");
    let line_count = text.matches("LINE_").count();
    assert_eq!(line_count, 10000, "行数不符：{line_count}（掉字）");

    let lines_per_sec = 10_000.0 / elapsed.as_secs_f64().max(0.001);
    assert!(lines_per_sec >= 1_000.0, "吞吐过低：{lines_per_sec:.0} 行/秒（耗时 {elapsed:?}）");
    eprintln!("验收1：{line_count} 行 / {elapsed:?} = {lines_per_sec:.0} 行/秒");
}

#[tokio::test(flavor = "multi_thread")]
async fn conpty_kill_produces_eof_without_hang() {
    // spawn 必须在 tokio 上下文内进行（写/resize worker 需要 runtime）
    let conpty = ConptyWin::new();
    let mut handle = conpty
        .spawn(TermCfg {
            shell: "ping.exe -n 60 127.0.0.1".into(),
            cwd: None,
            cols: 80,
            rows: 24,
            env: Default::default(),
        })
        .expect("spawn 失败");

    // 长驻进程显式终止 → shutdown 序列 → 读端 EOF
    (handle.kill)();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        assert!(!remain.is_zero(), "kill 后 10s 内未 EOF（句柄泄漏/挂起）");
        match tokio::time::timeout(remain, handle.output_rx.recv()).await {
            Ok(None) => break,
            Ok(Some(_)) => continue, // 排空缓冲
            Err(_) => panic!("kill 后通道未在 10s 内 EOF"),
        }
    }
}
