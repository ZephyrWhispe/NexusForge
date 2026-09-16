//! K1 设备发现：UDP 组播心跳（docs/impl/05 K1）。
//!
//! 协议：组播 239.255.42.98:49800，每 1s 发送 JSON 心跳
//! `{device_id, device_name, pubkey_fingerprint, tcp_port, caps, seq}`；
//! 5s 未收到 → 判离线；同 device_id 以 seq（unix_ms）最新为准。
//! seq 用 unix 毫秒：服务重启后单调不减，天然防重放误拒。
//!
//! socket 经 socket2 设 SO_REUSEADDR：同机多实例（验收/测试）可同端口共存。

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
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
}

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
        })
    }

    /// 设置发现事件回调（在线/离线）；须在 run 之前调用
    pub fn set_event_cb(&self, cb: Box<dyn Fn(PeerEvent) + Send + Sync>) {
        *self.event_cb.lock().expect("event_cb 锁") = Some(cb);
    }

    /// 邻居快照（按 device_id 排序，供 IPC/UI）
    pub fn peers_snapshot(&self) -> Vec<PeerInfo> {
        let mut list: Vec<PeerInfo> = self
            .peers
            .lock()
            .expect("peers 锁")
            .values()
            .map(|e| e.info.clone())
            .collect();
        list.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        list
    }

    pub fn peer(&self, device_id: &str) -> Option<PeerInfo> {
        self.peers
            .lock()
            .expect("peers 锁")
            .get(device_id)
            .map(|e| e.info.clone())
    }

    /// 启动心跳发送 / 接收 / 离线收割三任务
    pub async fn run(self: Arc<Self>) -> Result<DiscoveryHandle, AppError> {
        let socket = bind_multicast(self.config.port)?;
        let send_socket = bind_multicast(self.config.port)?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let mut tasks = Vec::new();

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
                };
                seq = hb.seq;
                let payload = serde_json::to_vec(&hb).unwrap_or_default();
                let dst = SocketAddrV4::new(MULTICAST_V4, sender_self.config.port);
                if let Err(e) = send_socket.send_to(&payload, dst).await {
                    tracing::warn!(error = %e, "KVM 心跳发送失败");
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
            let mut peers = self.peers.lock().expect("peers 锁");
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
            };
            peers.insert(
                hb.device_id,
                PeerEntry { info: info.clone(), last_seen: std::time::Instant::now(), seq: hb.seq },
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
            let mut peers = self.peers.lock().expect("peers 锁");
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
        if let Some(cb) = self.event_cb.lock().expect("event_cb 锁").as_ref() {
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

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

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
            screen: ScreenRect { x: 0, y: 0, w: 1920, h: 1080 },
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
        svc_b.set_event_cb(Box::new(move |ev| ev_sink.lock().unwrap().push(ev)));

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
        svc_a2.set_event_cb(Box::new(move |ev| ev_sink2.lock().unwrap().push(ev)));
        // 手动注入一条"已存在"的邻居，随后静默
        let fake = PeerInfo {
            device_id: "ghost".into(),
            device_name: "ghost".into(),
            pubkey_fingerprint: String::new(),
            tcp_port: 1,
            caps: vec![],
            addr: format!("127.0.0.1:{port}").parse().unwrap(),
            screen: ScreenRect::default(),
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
            })
            .unwrap(),
            fake.addr,
        );
        let h_a2 = svc_a2.clone().run().await.unwrap();
        tokio::time::sleep(Duration::from_millis(700)).await;
        let evs = events_a2.lock().unwrap();
        assert!(
            evs.iter().any(|e| matches!(e, PeerEvent::Offline(id) if id == "ghost")),
            "应收到 ghost 离线事件，实际: {evs:?}"
        );
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
}
