//! K9 M4 验收：同机双实例环回——发现互见 → 配对 → 会话 → 剪贴板/文件传输。
//!
//! 端口与默认 dev 应用隔离：组播 49810，TCP 49811/49812（默认 49800/49801），
//! 数据目录与事件总线各自独立，走 `KvmModule::set_tcp_port` /
//! `set_discovery_port` 注入（K9a 可测化）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use host_core::events::{Event, EventBus};
use host_core::module::{Module, ModuleContext};
use host_core::ports::{ClipContent, Ports};
use kvm_core::module::KvmModule;

const DISCOVERY_PORT: u16 = 49810;
const TCP_A: u16 = 49811;
const TCP_B: u16 = 49812;

fn temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kvm-loop-{tag}-{}", uuid::Uuid::now_v7()))
}

/// 起一个完整实例：独立数据目录 + 独立事件总线，init + start
fn spawn_instance(
    tag: &str,
    tcp_port: u16,
) -> (Arc<KvmModule>, Arc<EventBus>, PathBuf) {
    let dir = temp_dir(tag);
    let bus = Arc::new(EventBus::new());
    let m = Arc::new(KvmModule::new());
    m.set_tcp_port(tcp_port);
    m.set_discovery_port(DISCOVERY_PORT);
    let ctx = Arc::new(ModuleContext {
        app_data_dir: dir.clone(),
        ports: Arc::new(Ports::new()),
        event_bus: bus.clone(),
    });
    m.init(ctx).expect("init");
    m.start().expect("start");
    (m, bus, dir)
}

/// 订阅主题收割到通道（须在目标事件发生前建立）
fn watch(bus: &Arc<EventBus>, topic: &'static str) -> std::sync::mpsc::Receiver<Event> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut sub = bus.subscribe(topic).expect("subscribe");
    std::thread::spawn(move || {
        while let Ok(ev) = sub.blocking_recv() {
            let _ = tx.send(ev);
        }
    });
    rx
}

fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>, what: &str) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "等待超时: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn two_instances_loopback_pair_session_transfer() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let (a, bus_a, dir_a) = spawn_instance("a", TCP_A);
    let (b, bus_b, _dir_b) = spawn_instance("b", TCP_B);

    // ① K1 发现互见（同机组播 loopback，心跳 1s；端口隔离不含 dev 应用）
    let peer_b = wait_for(
        Duration::from_secs(8),
        || {
            a.discovered_peers()
                .ok()?
                .into_iter()
                .find(|p| p.tcp_port == TCP_B)
        },
        "A 发现 B",
    );
    let peer_a = wait_for(
        Duration::from_secs(8),
        || {
            b.discovered_peers()
                .ok()?
                .into_iter()
                .find(|p| p.tcp_port == TCP_A)
        },
        "B 发现 A",
    );

    // ② K2 配对：A 签发一次性码 → B 持码 TCP 握手（双向指纹校验 + 落盘）
    let paired_rx = watch(&bus_a, "kvm.paired");
    let (code, _) = a.issue_pair_code().expect("签发配对码");
    let addr_a: std::net::SocketAddr = format!("127.0.0.1:{TCP_A}").parse().unwrap();
    let peer = b.pair_with(addr_a, &code).expect("配对握手");
    // pair_with 返回对端（A）身份，应与 B 发现的 A 一致
    assert_eq!(peer.device_id, peer_a.device_id, "配对到的设备应与发现的 A 一致");
    // A 侧被配对事件里的 peer 应是 B
    let ev = paired_rx.recv_timeout(Duration::from_secs(5)).expect("A 侧 kvm.paired 事件");
    assert_eq!(ev.payload["paired"], serde_json::json!(true));
    assert_eq!(ev.payload["peer"]["device_id"], serde_json::json!(peer_b.device_id));
    assert_eq!(a.paired_peers().unwrap().len(), 1);
    assert_eq!(b.paired_peers().unwrap().len(), 1);

    // ③ K3 会话：B connect_to → 双侧 established
    let est_a = watch(&bus_a, "kvm.session_state");
    let est_b = watch(&bus_b, "kvm.session_state");
    let a_id = b.connect_to(addr_a).expect("建立会话");
    for rx in [&est_a, &est_b] {
        let ev = rx.recv_timeout(Duration::from_secs(5)).expect("会话建立事件");
        assert_eq!(ev.payload["state"], serde_json::json!("established"));
    }
    assert_eq!(a.session_list().len(), 1);
    assert_eq!(b.session_list().len(), 1);

    // ④ K6 剪贴板：B → A 单帧（kvm.clip_received）
    let clip_rx = watch(&bus_a, "kvm.clip_received");
    b.send_clip(
        &a_id,
        ClipContent::Text { text: "nexusforge-kvm-loopback".into(), html: None },
    )
    .expect("发送剪贴板");
    let ev = clip_rx.recv_timeout(Duration::from_secs(5)).expect("A 收到剪贴板");
    assert_eq!(ev.payload["device_id"], serde_json::json!(peer_b.device_id));
    assert!(ev.payload.to_string().contains("nexusforge-kvm-loopback"));

    // ⑤ K6 文件：B → A 9MiB（约 3 块，4MB/块）+ SHA256 终验 + Ack 回执
    let file_rx = watch(&bus_a, "kvm.file_received");
    let ack_b_rx = watch(&bus_b, "kvm.transfer_ack");
    let src = dir_a.join("loop-payload.bin");
    let payload: Vec<u8> = (0..9 * 1024 * 1024).map(|i| (i * 31 + 7) as u8).collect();
    std::fs::write(&src, &payload).expect("写测试文件");
    b.send_file(&a_id, src.to_string_lossy().into()).expect("发起文件发送");

    let ev = file_rx.recv_timeout(Duration::from_secs(15)).expect("A 收到文件完成事件");
    assert_eq!(ev.payload["name"], serde_json::json!("loop-payload.bin"));
    let final_path = ev.payload["path"].as_str().expect("完成事件含最终路径");
    let received = std::fs::read(final_path).expect("读取接收文件");
    assert_eq!(received.len(), payload.len(), "接收文件大小一致");
    assert!(received == payload, "SHA256 终验后字节一致");

    let ev = ack_b_rx.recv_timeout(Duration::from_secs(5)).expect("B 收到 Ack");
    assert_eq!(ev.payload["ok"], serde_json::json!(true));

    // ⑥ 收尾：stop 收割任务不悬挂
    a.stop().expect("A stop");
    b.stop().expect("B stop");
}
