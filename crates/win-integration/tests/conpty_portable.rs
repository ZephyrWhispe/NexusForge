//! portable-pty（wezterm，生产级 ConPTY 实现）对照实验：验证环境是否可产生渲染流。
//!
//! 分诊诊断探针（DECISIONS D-20）：与 conpty_min 联合定论「本机系统 conhost 路径产不出
//! 渲染流」为已知环境限制（三套独立实现一致失败，IDE 内嵌终端正常）。
//! 默认不参与门禁；复现：`cargo test -p win-integration --test conpty_portable -- --ignored --nocapture`

#[test]
#[ignore = "分诊诊断探针（D-20）：验证本机 conhost 渲染能力的对照实验，仅手动运行"]
fn portable_pty_echo_reference() {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize { rows: 40, cols: 120, ..Default::default() }).expect("openpty");
    let mut cmd = CommandBuilder::new("cmd.exe");
    cmd.args(["/c", "echo PORTABLE_OK"]);
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    let mut reader = pair.master.try_clone_reader().expect("reader");

    // 读 5s
    let mut collected = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    use std::io::Read;
    let mut buf = [0u8; 4096];
    while std::time::Instant::now() < deadline {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => collected.extend_from_slice(&buf[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&collected).to_string();
    println!("[portable] 共 {} 字节: {:?}", collected.len(), text);
    let _ = child.wait();
    assert!(text.contains("PORTABLE_OK"), "portable-pty 也读不到输出（环境问题）—— {} 字节", collected.len());
}
