//! K1 设备发现：UDP 组播心跳（docs/impl/05 K1）。
//!
//! 协议：组播 239.255.42.98:49800，每 1s 发送 JSON 心跳
//! `{device_id, device_name, pubkey_fingerprint, tcp_port, sync_port, caps, seq}`；
//! 5s 未收到 → 判离线；同 device_id 以 seq（unix_ms）最新为准。
//! seq 用 unix 毫秒：服务重启后单调不减，天然防重放误拒。
//!
//! socket 经 socket2 设 SO_REUSEADDR：同机多实例（验收/测试）可同端口共存。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use host_core::error::AppError;
use host_core::ports::ScreenRect;

/// 组播组地址（docs/impl/05 K1 规约）
pub const MULTICAST_V4: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 98);
/// 默认心跳/监听端口
pub const DEFAULT_PORT: u16 = 49800;
/// 对端**未宣告** sync 端口时按此端口组合地址（09 §10.2 T-B5-7）。
///
/// 值与 `sync_core::DEFAULT_SYNC_PORT` 同：这是"两端同默认端口"的既有运维约定，
/// 不是从谁那里读来的。为什么不读：依赖方向上 kvm-core 与 sync-core 互不相识
/// （sync 不 import kvm，kvm 也不 import sync），为一枚常量开一条依赖边是把耦合
/// 写进 Cargo.toml。猜错的后果是可恢复的：拨不通会记 `last_error`，不静默、不假成功。
/// 等值由 `src-tauri/src/state.rs` 的装配测试钉住（那一层同时看得见两枚常量）。
pub const DEFAULT_SYNC_PORT: u16 = 49820;

/// 心跳载荷（线格式）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Heartbeat {
    pub device_id: String,
    pub device_name: String,
    pub pubkey_fingerprint: String,
    pub tcp_port: u16,
    pub caps: Vec<String>,
    /// unix 毫秒时间戳：同设备以最新者为准
    pub seq: u64,
    /// 本机虚拟桌面矩形（K7 边缘切换：对端需知此屏几何以换算回移坐标）；
    /// `default` 兼容旧端：不参与边缘切换时保持 0
    #[serde(default)]
    pub screen: ScreenRect,
    /// 本机 SYNC 监听端口（09 §10.2 T-B5-7：免手输地址的事实源）；
    /// **0 = 旧端未宣告**（`serde(default)` ⇒ 旧包读成新端得 0、新包读进旧端忽略该键，
    /// 双向兼容，无版本协商面）
    #[serde(default)]
    pub sync_port: u16,
}

/// 已发现的邻居设备
#[derive(Clone, Debug, Serialize)]
pub struct PeerInfo {
    pub device_id: String,
    pub device_name: String,
    pub pubkey_fingerprint: String,
    pub tcp_port: u16,
    pub caps: Vec<String>,
    /// 心跳来源地址（TCP 会话拨号目标）
    pub addr: SocketAddr,
    /// 对端虚拟桌面矩形（K7 边缘回移换算用；旧端心跳无此字段则为 0）
    pub screen: ScreenRect,
    /// 对端宣告的 SYNC 端口（0 = 未宣告；见 `sync_addr`）
    pub sync_port: u16,
}

impl PeerInfo {
    /// 该设备的 sync 拨号地址：IP 取心跳来源（**运行态，永不写进 pairs.json**），
    /// 端口优先用宣告值，未宣告时回落 [`DEFAULT_SYNC_PORT`]。
    pub fn sync_addr(&self) -> String {
        let port = if self.sync_port > 0 {
            self.sync_port
        } else {
            DEFAULT_SYNC_PORT
        };
        // IPv6 来源必须带方括号，否则 "fe80::1:49820" 这种串没法反解
        match self.addr.ip() {
            std::net::IpAddr::V6(v6) => format!("[{v6}]:{port}"),
            other => format!("{other}:{port}"),
        }
    }
}

/// 发现层事件（模块层转 EventBus / UI）
#[derive(Clone, Debug)]
pub enum PeerEvent {
    Online(PeerInfo),
    Offline(String),
}

#[derive(Clone)]
pub struct DiscoveryConfig {
    pub port: u16,
    pub heartbeat_interval: Duration,
    pub offline_after: Duration,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            heartbeat_interval: Duration::from_secs(1),
            offline_after: Duration::from_secs(5),
        }
    }
}

pub struct OwnIdentity {
    pub device_id: String,
    pub device_name: String,
    pub pubkey_fingerprint: String,
    /// 本机 TCP 会话监听端口（随心跳广播）
    pub tcp_port: u16,
    pub caps: Vec<String>,
    /// 本机虚拟桌面矩形（随心跳广播；K7 边缘切换用）
    pub screen: ScreenRect,
    /// 本机 SYNC 监听端口（随心跳广播；0 = 本端不供同步 ⇒ 对端按未宣告处理）
    pub sync_port: u16,
}

struct PeerEntry {
    info: PeerInfo,
    last_seen: std::time::Instant,
    seq: u64,
}

type EventCb = Arc<Mutex<Option<Box<dyn Fn(PeerEvent) + Send + Sync>>>>;

pub struct DiscoveryService {
    own: OwnIdentity,
    config: DiscoveryConfig,
    peers: Mutex<HashMap<String, PeerEntry>>,
    event_cb: EventCb,
    /// D-40：组播不可达的降级原因（None＝发现正常在跑）。bind 失败或连续
    /// SEND_FAIL_THRESHOLD 次心跳发送失败时置位，成功一次即清除（自愈可见）。
    degraded: Mutex<Option<String>>,
    send_failures: std::sync::atomic::AtomicU32,
}

/// D-40：连续心跳发送失败达到此数才判降级（单包瞬断不惊动 UI）
const SEND_FAIL_THRESHOLD: u32 = 3;

/// 运行句柄：Drop 或 [`shutdown`] 停止全部任务
pub struct DiscoveryHandle {
    shutdown_tx: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl DiscoveryHandle {
    pub async fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        for t in self.tasks.drain(..) {
            let _ = t.await;
        }
    }
}

impl Drop for DiscoveryHandle {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        for t in self.tasks.drain(..) {
            t.abort();
        }
    }
}

impl DiscoveryService {
    pub fn new(own: OwnIdentity, config: DiscoveryConfig) -> Arc<Self> {
        Arc::new(Self {
            own,
            config,
            peers: Mutex::new(HashMap::new()),
            event_cb: Arc::new(Mutex::new(None)),
            degraded: Mutex::new(None),
            send_failures: std::sync::atomic::AtomicU32::new(0),
        })
    }

    /// D-40：降级原因（Some＝局域网自动发现不可用，但模块其余能力在线）
    pub fn degraded_reason(&self) -> Option<String> {
        self.degraded.lock().clone()
    }

    /// 设置发现事件回调（在线/离线）；须在 run 之前调用
    pub fn set_event_cb(&self, cb: Box<dyn Fn(PeerEvent) + Send + Sync>) {
        *self.event_cb.lock() = Some(cb);
    }

    /// 邻居快照（按 device_id 排序，供 IPC/UI）
    pub fn peers_snapshot(&self) -> Vec<PeerInfo> {
        let mut list: Vec<PeerInfo> = self.peers.lock().values().map(|e| e.info.clone()).collect();
        list.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        list
    }

    pub fn peer(&self, device_id: &str) -> Option<PeerInfo> {
        self.peers.lock().get(device_id).map(|e| e.info.clone())
    }

    /// 启动心跳发送 / 接收 / 离线收割三任务。
    ///
    /// D-40：组播 socket 不可用（离线笔记本无 LAN 路由时 join/bind 报
    /// WSAENETUNREACH 等）**不再让整个模块 start 失败**——配对/会话/剪贴板/
    /// 文件全走 TCP 直连，与组播无关。降级为只跑离线收割任务＋degraded 置因，
    /// UI 经 `degraded_reason` 明说"自动发现不可用，可用配对码直连"。
    pub async fn run(self: Arc<Self>) -> Result<DiscoveryHandle, AppError> {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut tasks = Vec::new();

        let bound = match (
            bind_multicast(self.config.port),
            bind_multicast(self.config.port),
        ) {
            (Ok(recv_s), Ok(send_s)) => Some((recv_s, send_s)),
            (Err(e), _) | (_, Err(e)) => {
                let reason = format!("组播发现不可用（socket 绑定失败：{e}）");
                tracing::warn!(error = %e, "KVM 发现降级：组播 socket 不可用，模块继续以直连能力运行");
                *self.degraded.lock() = Some(reason);
                None
            }
        };

        if let Some((socket, send_socket)) = bound {
            // ① 心跳发送
            let sender_self = self.clone();
            let mut sender_shutdown = shutdown_rx.clone();
            tasks.push(tokio::spawn(async move {
                let mut tick = interval(sender_self.config.heartbeat_interval);
                tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
                let mut seq = unix_ms();
                while !*sender_shutdown.borrow_and_update() {
                    let hb = Heartbeat {
                        device_id: sender_self.own.device_id.clone(),
                        device_name: sender_self.own.device_name.clone(),
                        pubkey_fingerprint: sender_self.own.pubkey_fingerprint.clone(),
                        tcp_port: sender_self.own.tcp_port,
                        caps: sender_self.own.caps.clone(),
                        seq: seq.wrapping_add(1),
                        screen: sender_self.own.screen,
                        sync_port: sender_self.own.sync_port,
                    };
                    seq = hb.seq;
                    let payload = serde_json::to_vec(&hb).unwrap_or_default();
                    let dst = SocketAddrV4::new(MULTICAST_V4, sender_self.config.port);
                    match send_socket.send_to(&payload, dst).await {
                        Ok(_) => {
                            // D-40：发送恢复即清除降级（自愈可见；未达阈值的
                            // 瞬断从未置位，这里置 None 也无副作用）
                            if sender_self
                                .send_failures
                                .swap(0, std::sync::atomic::Ordering::Relaxed)
                                >= SEND_FAIL_THRESHOLD
                            {
                                *sender_self.degraded.lock() = None;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "KVM 心跳发送失败");
                            let n = sender_self
                                .send_failures
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                                + 1;
                            if n == SEND_FAIL_THRESHOLD {
                                *sender_self.degraded.lock() = Some(format!(
                                    "组播发送持续失败（连续 {n} 次：{e}），局域网自动发现暂不可用"
                                ));
                            }
                        }
                    }
                    tokio::select! {
                        _ = tick.tick() => {}
                        _ = sender_shutdown.changed() => {}
                    }
                }
            }));

            // ② 接收 + 表维护
            let recv_self = self.clone();
            let mut recv_shutdown = shutdown_rx.clone();
            tasks.push(tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                loop {
                    tokio::select! {
                        res = socket.recv_from(&mut buf) => {
                            match res {
                                Ok((n, src)) => recv_self.on_datagram(&buf[..n], src),
                                Err(e) => {
                                    tracing::warn!(error = %e, "KVM 心跳接收失败");
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                }
                            }
                        }
                        _ = recv_shutdown.changed() => break,
                    }
                }
            }));
        }

        // ③ 离线收割
        let reap_self = self.clone();
        let mut reap_shutdown = shutdown_rx;
        tasks.push(tokio::spawn(async move {
            let reap_every = (reap_self.config.offline_after / 3).max(Duration::from_millis(100));
            let mut tick = interval(reap_every);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = tick.tick() => reap_self.reap(),
                    _ = reap_shutdown.changed() => break,
                }
            }
        }));

        Ok(DiscoveryHandle { shutdown_tx, tasks })
    }

    /// 单个心跳数据报处理：解析 → 过滤自身 → seq 判新 → 更新表/发布在线
    fn on_datagram(&self, payload: &[u8], src: SocketAddr) {
        let Ok(hb) = serde_json::from_slice::<Heartbeat>(payload) else {
            return; // 非法/异版本心跳静默丢弃
        };
        if hb.device_id == self.own.device_id {
            return; // 组播环回的自发文
        }
        let event = {
            let mut peers = self.peers.lock();
            match peers.get(&hb.device_id) {
                Some(existing) if existing.seq >= hb.seq => return, // 乱序旧包
                _ => {}
            }
            let first = !peers.contains_key(&hb.device_id);
            let info = PeerInfo {
                device_id: hb.device_id.clone(),
                device_name: hb.device_name,
                pubkey_fingerprint: hb.pubkey_fingerprint,
                tcp_port: hb.tcp_port,
                caps: hb.caps,
                addr: src,
                screen: hb.screen,
                sync_port: hb.sync_port,
            };
            peers.insert(
                hb.device_id,
                PeerEntry {
                    info: info.clone(),
                    last_seen: std::time::Instant::now(),
                    seq: hb.seq,
                },
            );
            if first {
                Some(PeerEvent::Online(info))
            } else {
                None
            }
        };
        if let Some(ev) = event {
            self.emit(ev);
        }
    }

    /// 离线收割：超时未更新 → 移除 + 发布离线
    fn reap(&self) {
        let dead: Vec<String> = {
            let mut peers = self.peers.lock();
            let deadline = self.config.offline_after;
            let dead: Vec<String> = peers
                .iter()
                .filter(|(_, e)| e.last_seen.elapsed() > deadline)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &dead {
                peers.remove(id);
            }
            dead
        };
        for id in dead {
            tracing::info!(device_id = %id, "KVM 邻居离线");
            self.emit(PeerEvent::Offline(id));
        }
    }

    fn emit(&self, ev: PeerEvent) {
        if let Some(cb) = self.event_cb.lock().as_ref() {
            cb(ev);
        }
    }
}

/// 构造组播可用的 UDP socket（SO_REUSEADDR 供同机多实例）
fn bind_multicast(port: u16) -> Result<UdpSocket, AppError> {
    let io_err = |e: std::io::Error| AppError::module("KVM_NET_001", e.to_string(), None);
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .map_err(io_err)?;
    sock.set_reuse_address(true).map_err(io_err)?;
    sock.set_multicast_loop_v4(true).map_err(io_err)?;
    let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port);
    sock.bind(&addr.into()).map_err(io_err)?;
    let std_sock: std::net::UdpSocket = sock.into();
    std_sock
        .join_multicast_v4(&MULTICAST_V4, &Ipv4Addr::UNSPECIFIED)
        .map_err(io_err)?;
    std_sock.set_nonblocking(true).map_err(io_err)?;
    UdpSocket::from_std(std_sock).map_err(|e| AppError::module("KVM_NET_001", e.to_string(), None))
}

use host_core::util::now_ms_u64 as unix_ms;

#[cfg(test)]
mod tests {
    use super::*;

    fn own(id: &str) -> OwnIdentity {
        OwnIdentity {
            device_id: id.to_string(),
            device_name: id.to_string(),
            pubkey_fingerprint: "ff".repeat(16),
            tcp_port: 49900,
            caps: vec!["input".into(), "clip".into(), "file".into()],
            screen: ScreenRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            sync_port: DEFAULT_SYNC_PORT,
        }
    }

    fn test_config(port: u16) -> DiscoveryConfig {
        DiscoveryConfig {
            port,
            heartbeat_interval: Duration::from_millis(60),
            offline_after: Duration::from_millis(300),
        }
    }

    /// 同机双实例（SO_REUSEADDR）互见：A 收到 B 心跳 → Online；B 停发 → Offline
    #[tokio::test(flavor = "multi_thread")]
    async fn two_instances_see_each_other_and_offline() {
        let port = 49911u16;
        let svc_a = DiscoveryService::new(own("device-a"), test_config(port));
        let svc_b = DiscoveryService::new(own("device-b"), test_config(port));

        let events_b: Arc<Mutex<Vec<PeerEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let ev_sink = events_b.clone();
        svc_b.set_event_cb(Box::new(move |ev| ev_sink.lock().push(ev)));

        let _h_a = svc_a.clone().run().await.unwrap();
        let _h_b = svc_b.clone().run().await.unwrap();

        // A 应在 ~1s 内发现 B
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if svc_a.peer("device-b").is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "A 未发现 B");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // 自过滤：A 的 peers 不含自身
        assert!(svc_a.peer("device-a").is_none());

        // B 停发后（收割周期 100ms）→ Offline 事件
        let _ = svc_b; // 仍运行；单独 drop 无 API —— 直接验证收割：不再发心跳即可
        drop(_h_a);
        drop(_h_b);
        // 重新建一对：只跑 A 的收割，B 不启动 → 5×offline_after 内应报 Offline
        let events_a2: Arc<Mutex<Vec<PeerEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let ev_sink2 = events_a2.clone();
        let svc_a2 = DiscoveryService::new(own("device-a"), test_config(port));
        svc_a2.set_event_cb(Box::new(move |ev| ev_sink2.lock().push(ev)));
        // 手动注入一条"已存在"的邻居，随后静默
        let fake = PeerInfo {
            device_id: "ghost".into(),
            device_name: "ghost".into(),
            pubkey_fingerprint: String::new(),
            tcp_port: 1,
            caps: vec![],
            addr: format!("127.0.0.1:{port}").parse().unwrap(),
            screen: ScreenRect::default(),
            sync_port: 0,
        };
        svc_a2.on_datagram(
            &serde_json::to_vec(&Heartbeat {
                device_id: "ghost".into(),
                device_name: "ghost".into(),
                pubkey_fingerprint: String::new(),
                tcp_port: 1,
                caps: vec![],
                seq: unix_ms(),
                screen: ScreenRect::default(),
                sync_port: 0,
            })
            .unwrap(),
            fake.addr,
        );
        let h_a2 = svc_a2.clone().run().await.unwrap();
        tokio::time::sleep(Duration::from_millis(700)).await;
        {
            let evs = events_a2.lock();
            assert!(
                evs.iter()
                    .any(|e| matches!(e, PeerEvent::Offline(id) if id == "ghost")),
                "应收到 ghost 离线事件，实际: {evs:?}"
            );
        }
        h_a2.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stale_seq_ignored() {
        let port = 49912u16;
        let svc = DiscoveryService::new(own("device-a"), test_config(port));
        let hb = Heartbeat {
            device_id: "device-x".into(),
            device_name: "x".into(),
            pubkey_fingerprint: String::new(),
            tcp_port: 1,
            caps: vec![],
            seq: 200,
            screen: ScreenRect::default(),
            sync_port: 0,
        };
        let src: SocketAddr = "127.0.0.1:1000".parse().unwrap();
        svc.on_datagram(&serde_json::to_vec(&hb).unwrap(), src);
        // 旧 seq 包应被忽略（信息保持 seq=200 那份的内容）
        let mut stale = hb.clone();
        stale.seq = 100;
        stale.device_name = "old".into();
        svc.on_datagram(&serde_json::to_vec(&stale).unwrap(), src);
        assert_eq!(svc.peer("device-x").unwrap().device_name, "x");
    }

    /// 心跳线格式向后兼容：旧端样本（无 sync_port 键）解码为 0，新包照常读出。
    /// 断言用**手写 JSON 字面量**而非"序列化后再删键"——后者测的是 serde 的
    /// skip 能力，测不到"线上真的见过没有这个键的字节"这件事。
    #[test]
    #[allow(non_snake_case)]
    fn heartbeat_absentSyncPort_decodesAsZero() {
        let old_wire = br#"{"device_id":"a","device_name":"a","pubkey_fingerprint":"f",
            "tcp_port":49600,"caps":["input"],"seq":7,"screen":{"x":0,"y":0,"w":0,"h":0}}"#;
        let old: Heartbeat = serde_json::from_slice(old_wire).expect("旧端心跳样本必须可解");
        assert_eq!(
            old.sync_port, 0,
            "缺键必须解码为 0（=未宣告），不能是随机值"
        );

        let new_wire = serde_json::to_vec(&Heartbeat {
            device_id: "b".into(),
            device_name: "b".into(),
            pubkey_fingerprint: "f".into(),
            tcp_port: 49600,
            caps: vec![],
            seq: 8,
            screen: ScreenRect::default(),
            sync_port: 49821,
        })
        .unwrap();
        let new: Heartbeat = serde_json::from_slice(&new_wire).expect("新端心跳可解");
        assert_eq!(new.sync_port, 49821);
        // 反向兼容：旧端读新包＝键在但没人看，serde 默认忽略未知字段 ⇒ 不炸
        #[derive(Deserialize)]
        struct OldPeerView {
            #[allow(dead_code)]
            device_id: String,
        }
        let viewed: OldPeerView =
            serde_json::from_slice(&new_wire).expect("旧端视图读新包必须不失败");
        assert_eq!(viewed.device_id, "b");
    }

    /// 地址组合：优先用宣告端口；宣告 0 时回落默认端口（正反两臂）
    #[test]
    #[allow(non_snake_case)]
    fn peerInfo_syncAddr_prefersAdvertisedPort() {
        let peer = |addr: &str, sync_port: u16| PeerInfo {
            device_id: "d".into(),
            device_name: "d".into(),
            pubkey_fingerprint: String::new(),
            tcp_port: 1,
            caps: vec![],
            addr: addr.parse().unwrap(),
            screen: ScreenRect::default(),
            sync_port,
        };
        assert_eq!(
            peer("192.168.1.10:49911", 49821).sync_addr(),
            "192.168.1.10:49821"
        );
        assert_eq!(
            peer("192.168.1.10:49911", 0).sync_addr(),
            format!("192.168.1.10:{DEFAULT_SYNC_PORT}")
        );
        // IPv6 链路本地必须带方括号，否则端口与地址无法反解
        assert_eq!(
            peer("[fe80::1]:49911", 0).sync_addr(),
            format!("[fe80::1]:{DEFAULT_SYNC_PORT}")
        );
        assert_eq!(
            peer("[fe80::1]:49911", 49822).sync_addr(),
            "[fe80::1]:49822"
        );
        // 宣告端口是从心跳里读的对端事实，不是本机配置：IP 随来源而变
        assert_ne!(
            peer("10.0.0.5:1", 49821).sync_addr(),
            peer("10.0.0.6:1", 49821).sync_addr()
        );
    }

    /// D-40①：组播 socket 绑不上**不得**让 run() 失败——降级原因必须与绑定
    /// 实况严格对应。端口 1 是环境探针：Linux 非 root 必拒（Err 分支实测），
    /// Windows 在线机可绑（None 分支实测），离线笔记本 join 报 10065（本机
    /// 实测正是 Err 分支）——断言按同口径的 bind_multicast 实况自发对齐。
    #[tokio::test(flavor = "multi_thread")]
    #[allow(non_snake_case)]
    async fn d40_bindFailure_degradesInsteadOfFailing() {
        let port = 1u16;
        // run() 内部绑两次且任一 Err 即降级——预言取同形口径
        let bind_ok = bind_multicast(port).is_ok() && bind_multicast(port).is_ok();
        let svc = DiscoveryService::new(own("device-d40"), test_config(port));
        assert!(
            svc.degraded_reason().is_none(),
            "新建服务不得自带降级（初始态钉死）"
        );
        let h = svc
            .clone()
            .run()
            .await
            .expect("D-40：组播不可用不得使 run 失败");
        assert_eq!(
            svc.degraded_reason().is_some(),
            !bind_ok,
            "降级必须恰与绑定实况一致（绑不上⇒有原因，绑得上⇒无原因）"
        );
        if let Some(reason) = svc.degraded_reason() {
            assert!(reason.contains("组播"), "降级原因须点名组播: {reason}");
        }
        h.shutdown().await;
    }

    /// D-40②：心跳发送恢复 → 降级原因自愈清除。预置降级＋失败计数达阈模拟
    /// "带伤重启"；环境绑得上组播则发送任务（60ms 节拍）应在时限内清因，
    /// 绑不上（或持续发送失败）则降级原因必须持续在场且点名"组播"——两臂
    /// 都断言，测试在在线/离线机器上均确定通过且不放松不变量。
    #[tokio::test(flavor = "multi_thread")]
    #[allow(non_snake_case)]
    async fn d40_sendSuccessClearsDegraded() {
        let port = 49913u16;
        let svc = DiscoveryService::new(own("device-d40-clear"), test_config(port));
        let preset = "模拟绑定失败后的遗留降级";
        *svc.degraded.lock() = Some(preset.to_string());
        svc.send_failures
            .store(SEND_FAIL_THRESHOLD, std::sync::atomic::Ordering::Relaxed);
        let h = svc
            .clone()
            .run()
            .await
            .expect("D-40：run 不得因组播失败而失败");
        if svc.degraded_reason().as_deref() == Some(preset) {
            // 绑定成功且未被发送失败覆写 ⇒ 唯一合法去向是被成功发送清成 None
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if svc.degraded_reason().is_none() {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "绑定成功后降级应在心跳节拍内自愈（未自愈即清因逻辑失修）"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } else {
            // 环境不可组播（绑定失败或被发送失败覆写）⇒ 降级必须有因可见
            let reason = svc.degraded_reason().expect("降级态必须有可见原因");
            assert!(reason.contains("组播"), "降级原因须点名组播: {reason}");
        }
        h.shutdown().await;
    }
}
