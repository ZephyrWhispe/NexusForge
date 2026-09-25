//! K2 配对：一次性码 + 公钥指纹校验（docs/impl/05 K2）。
//!
//! 流程：
//! 1. 被配对端 UI 调 [`PairCodeManager::issue`] 展示 6 位一次性码（2 分钟有效、
//!    单次使用、错 5 次作废）；
//! 2. 发起端经 [`PairingService::pair_with`] 连对端 SESSION_PORT，发
//!    PairRequest（含自身 device_id/名称/公钥/指纹 + 用户输入的码）；
//! 3. 对端校验码 → 校验"指纹 = SHA256(公钥)"绑定（防伪造公钥）→ 写入
//!    paired.json 并回 PairAccept；
//! 4. 发起端同样校验对端指纹绑定后落盘。双方 UI 并排展示指纹供人工复核
//!    （蓝牙式比对，防中间人）。
//!
//! 后续会话（K3）以 paired.json 的指纹白名单为准入依据。

use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::watch;

use host_core::device::DeviceIdentity;
use host_core::error::AppError;
use host_core::ports::{KeyboardLedPort, LockStates};
use host_core::wire::{read_frame, write_frame, Frame, MsgType, HANDSHAKE_TIMEOUT};

use crate::locksync::sync_toward;

/// 一次性码有效期
pub const CODE_TTL: Duration = Duration::from_secs(120);
/// 错误尝试上限（达到即作废当前码）
pub const MAX_ATTEMPTS: u8 = 5;
/// SEC-11（D-37 R-I1）：同一来源 IP 的连续失败上限。取 MAX_ATTEMPTS+1——
/// 合法用户打错 5 次先触发码作废重签（既有语义），打不满这个限；连败烧穿
/// 者只剩脚本试探一种画像，临时封禁对合法配对零感知。
const MAX_IP_ATTEMPTS: u32 = 6;
/// 超限后的临时封禁时长（过期自动清零，非永久拉黑）
const IP_BLOCK: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// 一次性码
// ---------------------------------------------------------------------------

struct ActiveCode {
    code: String,
    expires: Instant,
    attempts: u8,
    used: bool,
}

/// 一次性码管理器（单活跃码：重新签发即覆盖旧码）
pub struct PairCodeManager {
    active: Mutex<Option<ActiveCode>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeError {
    /// 无活跃码或码不匹配
    Mismatch,
    /// 已过期
    Expired,
    /// 错误次数超限或已被使用
    Consumed,
}

impl PairCodeManager {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
        }
    }

    /// 签发新码：6 位数字（CSPRNG）。返回 (码, 有效期毫秒)
    pub fn issue(&self) -> (String, u64) {
        let code = format!("{:06}", rand::rngs::OsRng.next_u32() % 1_000_000);
        *self.active.lock() = Some(ActiveCode {
            code: code.clone(),
            expires: Instant::now() + CODE_TTL,
            attempts: 0,
            used: false,
        });
        (code, CODE_TTL.as_millis() as u64)
    }

    /// 校验并消费一次性码。错误尝试累计（达到 MAX_ATTEMPTS 作废）。
    pub fn validate(&self, code: &str) -> Result<(), CodeError> {
        let mut guard = self.active.lock();
        let Some(active) = guard.as_mut() else {
            return Err(CodeError::Mismatch);
        };
        if active.used {
            return Err(CodeError::Consumed);
        }
        if Instant::now() >= active.expires {
            *guard = None;
            return Err(CodeError::Expired);
        }
        if active.attempts >= MAX_ATTEMPTS {
            *guard = None;
            return Err(CodeError::Consumed);
        }
        // v1 直等比较（码仅 6 位且短时限，侧信道收益可忽略；注释声明设计取舍）
        if active.code != code {
            active.attempts += 1;
            if active.attempts >= MAX_ATTEMPTS {
                *guard = None;
                return Err(CodeError::Consumed);
            }
            return Err(CodeError::Mismatch);
        }
        active.used = true;
        Ok(())
    }

    /// 是否存在可用于展示的活跃码（UI 判断是否需要重签）
    pub fn has_active(&self) -> bool {
        self.active
            .lock()
            .as_ref()
            .map(|c| !c.used && Instant::now() < c.expires)
            .unwrap_or(false)
    }
}

impl Default for PairCodeManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 配对设备持久化（D-02：信任根上移至 host-core::device；此处再导出保持 kvm API）
// ---------------------------------------------------------------------------

pub use host_core::device::{b64_decode, b64_encode, PairStore, PairedPeer};

// ---------------------------------------------------------------------------
// 配对握手
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct PairRequestPayload {
    device_id: String,
    device_name: String,
    pubkey_b64: String,
    fingerprint: String,
    code: String,
}

#[derive(Serialize, Deserialize)]
struct PairReplyPayload {
    device_id: String,
    device_name: String,
    pubkey_b64: String,
    fingerprint: String,
    /// Reject 时的原因：code | fingerprint | self | busy | throttled（throttled＝SEC-11 IP 限速，旧发起端按未知原因原样展示，语义兼容）
    reason: Option<String>,
    /// 应答端锁键灯态（T-B7-9 首次配对时机；仅 PairAccept 携带，Reject 恒 None。
    /// 版本偏差登记（§7.3 冒烟清单）：旧对端无此键 ⇒ None=未知不静默对齐；
    /// 新帧多余键旧码 serde 忽略——发起端对齐应答端须双端到位）
    #[serde(default)]
    locks: Option<LockStates>,
}

/// 校验"指纹 = SHA256(公钥)"绑定（防伪造公钥/指纹组合）
fn verify_binding(pubkey: &[u8], fingerprint: &str) -> bool {
    let arr: [u8; 32] = match pubkey.try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    DeviceIdentity::fingerprint_of(&arr) == fingerprint
}

pub struct PairingService {
    identity: Arc<DeviceIdentity>,
    codes: Arc<PairCodeManager>,
    store: Arc<PairStore>,
    /// T-B7-9 修饰键同步端口（init 后经 set_led 注入；None=无键盘灯态面，配对帧不携带 locks）
    led: RwLock<Option<Arc<dyn KeyboardLedPort>>>,
    /// SEC-11（D-37 R-I1）：按 IP 的连续失败簿记（封禁随 IP_BLOCK 自动过期）
    throttle: Mutex<HashMap<IpAddr, IpRecord>>,
}

#[derive(Default)]
struct IpRecord {
    failures: u32,
    blocked_until: Option<Instant>,
}

/// 配对成功回调（服务端接受 / 客户端完成均触发）
pub type PairedCb = Arc<Mutex<Option<Box<dyn Fn(PairedPeer) + Send + Sync>>>>;

pub struct PairingHandle {
    shutdown_tx: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl PairingHandle {
    pub async fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        for t in self.tasks.drain(..) {
            let _ = t.await;
        }
    }
}

impl Drop for PairingHandle {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        for t in self.tasks.drain(..) {
            t.abort();
        }
    }
}

impl PairingService {
    pub fn new(
        identity: Arc<DeviceIdentity>,
        codes: Arc<PairCodeManager>,
        store: Arc<PairStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            identity,
            codes,
            store,
            led: RwLock::new(None),
            throttle: Mutex::new(HashMap::new()),
        })
    }

    /// 注入 LED 端口（kvm init 时经 Ports 查询后调用；既有测试零扰动）
    pub fn set_led(&self, led: Option<Arc<dyn KeyboardLedPort>>) {
        *self.led.write() = led;
    }

    /// 本机锁键灯态（端口缺失 ⇒ None=未知，配对帧不携带、对端不静默对齐）
    fn led_states(&self) -> Option<LockStates> {
        self.led.read().as_ref().map(|p| p.read_lock_states())
    }

    /// SEC-11：该 IP 是否处于封禁期（顺带回收过期记录；未知来源不阻断）
    fn ip_blocked(&self, ip: Option<IpAddr>) -> bool {
        let Some(ip) = ip else { return false };
        let mut map = self.throttle.lock();
        let Some(rec) = map.get_mut(&ip) else {
            return false;
        };
        let Some(until) = rec.blocked_until else {
            // 有连败记录但未达封禁：诚实放行且**不清计数**（首版误在此
            // remove，计数每轮被清零，封禁永不触发——DBG 取证坐实后修正）
            return false;
        };
        if Instant::now() < until {
            return true;
        }
        map.remove(&ip); // 封禁期满：整条回收，计数从零重启
        false
    }

    /// SEC-11：记一次来自该 IP 的码校验失败；连败达上限即临时封禁
    fn note_code_failure(&self, ip: Option<IpAddr>) {
        let Some(ip) = ip else { return };
        let mut map = self.throttle.lock();
        let rec = map.entry(ip).or_default();
        if rec
            .blocked_until
            .is_some_and(|until| Instant::now() >= until)
        {
            *rec = IpRecord::default();
        }
        rec.failures += 1;
        if rec.failures >= MAX_IP_ATTEMPTS {
            rec.blocked_until = Some(Instant::now() + IP_BLOCK);
            tracing::warn!(%ip, failures = rec.failures, "KVM 配对连败达上限，临时封禁该来源 IP");
        }
    }

    /// 配对成功即清零该 IP 连败计数（合法用户的偶发打错不跨码累积）
    fn clear_ip_failures(&self, ip: Option<IpAddr>) {
        if let Some(ip) = ip {
            self.throttle.lock().remove(&ip);
        }
    }

    /// 配对请求接入循环（被配对端）：仅处理 PairRequest 帧
    pub async fn accept_loop(
        self: Arc<Self>,
        listener: TcpListener,
        on_paired: PairedCb,
    ) -> PairingHandle {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut tasks = Vec::new();
        let mut accept_shutdown = shutdown_rx.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    res = listener.accept() => {
                        let Ok((stream, _)) = res else {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        };
                        let svc = self.clone();
                        let cb = on_paired.clone();
                        // 每连接独立任务：读首帧后交给 handle_pair_conn（分流同款）
                        tokio::spawn(async move {
                            let mut stream = stream;
                            let first = match tokio::time::timeout(HANDSHAKE_TIMEOUT, read_frame(&mut stream)).await {
                                Ok(Ok(f)) => f,
                                _ => return,
                            };
                            if let Some(peer) = svc.handle_pair_conn(stream, first).await {
                                if let Some(f) = cb.lock().as_ref() {
                                    f(peer);
                                }
                            }
                        });
                    }
                    _ = accept_shutdown.changed() => break,
                }
            }
        }));
        PairingHandle { shutdown_tx, tasks }
    }

    /// 服务端单连接处理：首帧已由调用方读出并分流（PairRequest）→
    /// 校验 → 持久化 → 应答。pub 供 SessionManager 分流调用。
    pub async fn handle_pair_conn(
        &self,
        mut stream: tokio::net::TcpStream,
        first: Frame,
    ) -> Option<PairedPeer> {
        let handshake = async {
            if first.msg_type != MsgType::PairRequest {
                return None;
            }
            let req: PairRequestPayload = serde_json::from_slice(&first.payload).ok()?;

            // 闭包无法 .await（曾导致 PairReject 永远发不出去），用宏内联 await + return
            macro_rules! reject {
                ($reason:expr) => {{
                    let reply = PairReplyPayload {
                        device_id: self.identity.device_id.clone(),
                        device_name: self.identity.device_name.clone(),
                        pubkey_b64: b64_encode(&self.identity.public_key()),
                        fingerprint: self.identity.pubkey_fingerprint.clone(),
                        reason: Some($reason.into()),
                        locks: None,
                    };
                    let _ = write_frame(
                        &mut stream,
                        &Frame {
                            msg_type: MsgType::PairReject,
                            flags: 0,
                            payload: serde_json::to_vec(&reply).unwrap_or_default(),
                        },
                    )
                    .await;
                    return None;
                }};
            }

            // ① 防自配对
            if req.device_id == self.identity.device_id {
                reject!("self");
            }
            // ② 公钥/指纹绑定校验（防伪造组合）
            let Some(pubkey) = b64_decode(&req.pubkey_b64) else {
                reject!("fingerprint");
            };
            if !verify_binding(&pubkey, &req.fingerprint) {
                reject!("fingerprint");
            }
            // ③ SEC-11（D-37 R-I1）：按 IP 封禁期直接拒绝——不消费码尝试预算
            // （否则单攻击者可用自身失败烧穿共享槽位，把合法用户的 5 次额度用光）
            let peer_ip = stream.peer_addr().ok().map(|a| a.ip());
            if self.ip_blocked(peer_ip) {
                tracing::warn!(from = %req.device_id, ip = ?peer_ip, "KVM 配对请求被 IP 限速拒绝");
                reject!("throttled");
            }
            // ④ 一次性码校验
            if let Err(e) = self.codes.validate(&req.code) {
                self.note_code_failure(peer_ip);
                let reason = match e {
                    CodeError::Mismatch => "code",
                    CodeError::Expired => "code",
                    CodeError::Consumed => "code",
                };
                tracing::warn!(from = %req.device_id, ?e, "KVM 配对码校验失败");
                reject!(reason);
            }
            self.clear_ip_failures(peer_ip);
            // ⑤ 落盘 + 应答
            let peer = PairedPeer {
                device_id: req.device_id.clone(),
                device_name: req.device_name,
                fingerprint: req.fingerprint,
                pubkey_b64: req.pubkey_b64,
                paired_at: unix_ms(),
            };
            if self.store.upsert(peer.clone()).is_err() {
                reject!("busy");
            }
            let reply = PairReplyPayload {
                device_id: self.identity.device_id.clone(),
                device_name: self.identity.device_name.clone(),
                pubkey_b64: b64_encode(&self.identity.public_key()),
                fingerprint: self.identity.pubkey_fingerprint.clone(),
                reason: None,
                locks: self.led_states(),
            };
            write_frame(
                &mut stream,
                &Frame {
                    msg_type: MsgType::PairAccept,
                    flags: 0,
                    payload: serde_json::to_vec(&reply).unwrap_or_default(),
                },
            )
            .await
            .ok()?;
            tracing::info!(peer = %peer.device_id, "KVM 配对完成（被配对端）");
            Some(peer)
        };
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!("KVM 配对握手超时");
                None
            }
        }
    }

    /// 发起配对（控制端）：连对端 SESSION_PORT，送一次性码
    pub async fn pair_with(&self, addr: SocketAddr, code: &str) -> Result<PairedPeer, AppError> {
        let io_err = |e: std::io::Error| AppError::module("KVM_PAIR_003", e.to_string(), None);
        let handshake = async {
            let mut stream = tokio::net::TcpStream::connect(addr).await.map_err(io_err)?;
            let req = PairRequestPayload {
                device_id: self.identity.device_id.clone(),
                device_name: self.identity.device_name.clone(),
                pubkey_b64: b64_encode(&self.identity.public_key()),
                fingerprint: self.identity.pubkey_fingerprint.clone(),
                code: code.to_string(),
            };
            write_frame(
                &mut stream,
                &Frame {
                    msg_type: MsgType::PairRequest,
                    flags: 0,
                    payload: serde_json::to_vec(&req)
                        .map_err(|e| AppError::module("KVM_PAIR_004", e.to_string(), None))?,
                },
            )
            .await?;
            let reply_frame = read_frame(&mut stream).await?;
            let reply: PairReplyPayload = serde_json::from_slice(&reply_frame.payload)
                .map_err(|e| AppError::module("KVM_PAIR_004", e.to_string(), None))?;
            match reply_frame.msg_type {
                MsgType::PairReject => {
                    let reason = reply.reason.unwrap_or_else(|| "unknown".into());
                    return Err(AppError::module(
                        "KVM_PAIR_005",
                        format!("对端拒绝配对: {reason}"),
                        Some("确认一次性码未过期且输入正确"),
                    ));
                }
                MsgType::PairAccept => {}
                other => {
                    return Err(AppError::module(
                        "KVM_PAIR_004",
                        format!("非预期应答帧类型 {:?}", other),
                        None,
                    ));
                }
            }
            // 对端绑定校验：Accept 中回传的公钥必须与指纹自洽
            let Some(peer_pubkey) = b64_decode(&reply.pubkey_b64) else {
                return Err(AppError::module("KVM_PAIR_004", "对端公钥非法", None));
            };
            if !verify_binding(&peer_pubkey, &reply.fingerprint) {
                return Err(AppError::module(
                    "KVM_PAIR_007",
                    "对端公钥与指纹不匹配（疑似伪造）",
                    None,
                ));
            }
            // T-B7-9 首次配对时机：发起端对齐应答端灯态（应答端为准、单向对齐，
            // 防双向各拍对方落回反相；locks=None=旧对端未知⇒诚实 no-op）
            if let (Some(remote), Some(led)) = (reply.locks, self.led.read().clone()) {
                sync_toward(led.as_ref(), &remote);
            }
            let peer = PairedPeer {
                device_id: reply.device_id,
                device_name: reply.device_name,
                fingerprint: reply.fingerprint,
                pubkey_b64: reply.pubkey_b64,
                paired_at: unix_ms(),
            };
            self.store.upsert(peer.clone())?;
            Ok(peer)
        };
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await {
            Ok(v) => v,
            Err(_) => Err(AppError::module("KVM_PAIR_006", "配对握手超时", None)),
        }
    }

    /// 解除配对
    pub fn unpair(&self, device_id: &str) -> Result<bool, AppError> {
        self.store.remove(device_id)
    }

    pub fn paired_peers(&self) -> Vec<PairedPeer> {
        self.store.all()
    }
}

use host_core::util::now_ms_u64 as unix_ms;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kvm-pair-{tag}-{}", uuid::Uuid::now_v7()))
    }

    #[test]
    fn code_issue_and_validate() {
        let mgr = PairCodeManager::new();
        let (code, ttl) = mgr.issue();
        assert_eq!(code.len(), 6);
        assert!(ttl >= 100_000);
        assert!(mgr.validate(&code).is_ok());
        // 单次使用
        assert_eq!(mgr.validate(&code), Err(CodeError::Consumed));
    }

    #[test]
    fn code_attempt_limit() {
        let mgr = PairCodeManager::new();
        let (code, _) = mgr.issue();
        // 前 4 次错误：Mismatch（码仍有效）
        for i in 0..4 {
            assert_eq!(
                mgr.validate("000000"),
                Err(CodeError::Mismatch),
                "第 {} 次",
                i
            );
        }
        // 第 5 次错误：达到上限，作废当前码
        assert_eq!(mgr.validate("000000"), Err(CodeError::Consumed));
        // 作废后正确码也失效
        assert!(mgr.validate(&code).is_err());
    }

    #[test]
    fn pair_store_persist_roundtrip() {
        let dir = temp_dir("store");
        let store = PairStore::load_or_default(&dir).unwrap();
        let peer = PairedPeer {
            device_id: "dev-1".into(),
            device_name: "PC-1".into(),
            fingerprint: "ab".repeat(16),
            pubkey_b64: String::new(),
            paired_at: 1,
        };
        store.upsert(peer).unwrap();
        assert!(store.is_paired("dev-1"));
        assert!(store.verify_fingerprint("dev-1", &"ab".repeat(16)));
        assert!(!store.verify_fingerprint("dev-1", "zz"));

        // 重新加载
        let store2 = PairStore::load_or_default(&dir).unwrap();
        assert_eq!(store2.all().len(), 1);
        assert!(store2.remove("dev-1").unwrap());
        let store3 = PairStore::load_or_default(&dir).unwrap();
        assert!(store3.all().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pair_handshake_roundtrip() {
        let dir_a = temp_dir("a");
        let dir_b = temp_dir("b");
        let id_a = Arc::new(DeviceIdentity::generate("PC-A".into()));
        let id_b = Arc::new(DeviceIdentity::generate("PC-B".into()));
        let codes_b = Arc::new(PairCodeManager::new());
        let store_a = Arc::new(PairStore::load_or_default(&dir_a).unwrap());
        let store_b = Arc::new(PairStore::load_or_default(&dir_b).unwrap());
        let svc_a = PairingService::new(
            id_a.clone(),
            Arc::new(PairCodeManager::new()),
            store_a.clone(),
        );
        let svc_b = PairingService::new(id_b.clone(), codes_b.clone(), store_b.clone());

        let (code, _) = codes_b.issue();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let paired_sink: Arc<Mutex<Vec<PairedPeer>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = paired_sink.clone();
        let _handle = svc_b
            .accept_loop(
                listener,
                Arc::new(Mutex::new(Some(Box::new(move |p| sink.lock().push(p))))),
            )
            .await;

        let peer_from_a = svc_a
            .pair_with(format!("127.0.0.1:{port}").parse().unwrap(), &code)
            .await
            .unwrap();
        assert_eq!(peer_from_a.device_id, id_b.device_id);
        assert_eq!(peer_from_a.fingerprint, id_b.pubkey_fingerprint);

        // 双方都落盘
        assert!(store_a.is_paired(&id_b.device_id));
        assert!(store_b.is_paired(&id_a.device_id));
        // 回调由 accept_loop 的任务侧异步 push，与 pair_with 返回之间无先后保证：
        // 全量 workspace 并发下此处偶发读到 0（首跑即命中）。有界轮询等它，
        // 超时仍断言原值——既不放松判据，也不把时序竞态留给下一次随机红灯。
        let mut ticks = 0;
        while paired_sink.lock().is_empty() && ticks < 200 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            ticks += 1;
        }
        assert_eq!(paired_sink.lock().len(), 1);
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pair_wrong_code_rejected() {
        let dir_a = temp_dir("wa");
        let dir_b = temp_dir("wb");
        let id_a = Arc::new(DeviceIdentity::generate("PC-A".into()));
        let id_b = Arc::new(DeviceIdentity::generate("PC-B".into()));
        let codes_b = Arc::new(PairCodeManager::new());
        let store_a = Arc::new(PairStore::load_or_default(&dir_a).unwrap());
        let store_b = Arc::new(PairStore::load_or_default(&dir_b).unwrap());
        let svc_a = PairingService::new(id_a, Arc::new(PairCodeManager::new()), store_a.clone());
        let svc_b = PairingService::new(id_b, codes_b.clone(), store_b);

        let (_code, _) = codes_b.issue();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let _handle = svc_b
            .accept_loop(listener, Arc::new(Mutex::new(None)))
            .await;

        let result = svc_a
            .pair_with(format!("127.0.0.1:{port}").parse().unwrap(), "999999")
            .await;
        // 回归（D-17 clippy let_underscore_futures 揪出）：reject 曾是闭包内未 await 的
        // future → PairReject 帧永不发出，客户端只能拿到 EOF/超时（KVM_PAIR_003/006）。
        // 必须收到协议层显式拒绝 KVM_PAIR_005（含 reason=code）。
        let err = result.expect_err("错误码必须被拒绝");
        assert!(
            matches!(&err, AppError::Module { code, message, .. }
                if code == "KVM_PAIR_005" && message.contains("code")),
            "应收到 PairReject 帧（KVM_PAIR_005 + reason=code），实际: {err:?}"
        );
        // 拒绝后不落盘
        assert!(store_a.all().is_empty());
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    /// SEC-11（D-37 R-I1）抢占负例：嗅探者抢先发同码 PairRequest——钉住
    /// "先到者胜"现状语义：后到者必被协议层显式拒绝，且被拒方不落盘。
    #[tokio::test(flavor = "multi_thread")]
    async fn pair_concurrent_requests_first_wins() {
        let dir_a = temp_dir("ca");
        let dir_a2 = temp_dir("ca2");
        let dir_b = temp_dir("cb");
        let id_a = Arc::new(DeviceIdentity::generate("PC-A".into()));
        let id_a2 = Arc::new(DeviceIdentity::generate("PC-A2".into()));
        let id_b = Arc::new(DeviceIdentity::generate("PC-B".into()));
        let codes_b = Arc::new(PairCodeManager::new());
        let store_a = Arc::new(PairStore::load_or_default(&dir_a).unwrap());
        let store_a2 = Arc::new(PairStore::load_or_default(&dir_a2).unwrap());
        let store_b = Arc::new(PairStore::load_or_default(&dir_b).unwrap());
        let svc_a = PairingService::new(id_a, Arc::new(PairCodeManager::new()), store_a);
        let svc_a2 = PairingService::new(id_a2, Arc::new(PairCodeManager::new()), store_a2);
        let svc_b = PairingService::new(id_b, codes_b.clone(), store_b.clone());

        let (code, _) = codes_b.issue();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let _handle = svc_b
            .accept_loop(listener, Arc::new(Mutex::new(None)))
            .await;
        let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

        let (r1, r2) = tokio::join!(svc_a.pair_with(addr, &code), svc_a2.pair_with(addr, &code));
        let oks = [r1.is_ok(), r2.is_ok()].iter().filter(|b| **b).count();
        assert_eq!(oks, 1, "单次使用码下恰有一方成功（先到者胜）");
        // 失败方必收到显式 PairReject（不能退化为 EOF/超时——与 D-17 回归同判据）
        let err = [r1, r2]
            .into_iter()
            .find_map(Result::err)
            .expect("必有一方被拒");
        assert!(
            matches!(&err, AppError::Module { code, message, .. }
                if code == "KVM_PAIR_005" && message.contains("code")),
            "后到者应被 PairReject 拒绝，实际: {err:?}"
        );
        // 服务端至多一份落盘（无重复信任记录）
        assert_eq!(store_b.all().len(), 1);
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_a2);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    /// SEC-11（D-37 R-I1）限速负例：①成功清零连败计数（合法用户偶发打错
    /// 不跨码累积）；②同 IP 连续失败达上限后，即使码正确也必被
    /// `throttled` 显式拒绝，且不再消费新码的尝试预算。
    #[tokio::test(flavor = "multi_thread")]
    async fn pair_ip_throttle_after_consecutive_failures() {
        let dir_a = temp_dir("ta");
        let dir_b = temp_dir("tb");
        let id_a = Arc::new(DeviceIdentity::generate("PC-A".into()));
        let id_b = Arc::new(DeviceIdentity::generate("PC-B".into()));
        let codes_b = Arc::new(PairCodeManager::new());
        let store_a = Arc::new(PairStore::load_or_default(&dir_a).unwrap());
        let store_b = Arc::new(PairStore::load_or_default(&dir_b).unwrap());
        let svc_a = PairingService::new(id_a, Arc::new(PairCodeManager::new()), store_a);
        let svc_b = PairingService::new(id_b, codes_b.clone(), store_b);

        let (code, _) = codes_b.issue();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let _handle = svc_b
            .clone()
            .accept_loop(listener, Arc::new(Mutex::new(None)))
            .await;
        let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

        // ① 三次打错 + 一次打对 = 成功（3+1 均低于两侧上限），成功清零计数
        for _ in 0..3 {
            assert!(svc_a.pair_with(addr, "000000").await.is_err());
        }
        assert!(svc_a.pair_with(addr, &code).await.is_ok());

        // ② 连败烧穿上限：码作废后继续空打至 MAX_IP_ATTEMPTS 触发封禁
        let (_code2, _) = codes_b.issue();
        for _ in 0..MAX_IP_ATTEMPTS {
            assert!(svc_a.pair_with(addr, "999999").await.is_err());
        }
        // 封禁期内即使码正确也必拒，且 reason=throttled（非 code——
        // 证明未消费新码的尝试预算，合法用户解封后额度完好）
        let (code3, _) = codes_b.issue();
        let err = svc_a
            .pair_with(addr, &code3)
            .await
            .expect_err("封禁期内必须拒绝");
        assert!(
            matches!(&err, AppError::Module { code, message, .. }
                if code == "KVM_PAIR_005" && message.contains("throttled")),
            "应收到 reason=throttled 的 PairReject，实际: {err:?}"
        );
        // 码本体未被消费：封禁拒绝后正确码仍在（换个"IP"即本机无法模拟，
        // 以 has_active 坐实未被作废即可）
        assert!(codes_b.has_active(), "throttled 拒绝不得消费活跃码");
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }
}
