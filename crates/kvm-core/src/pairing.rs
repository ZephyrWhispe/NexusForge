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

use parking_lot::Mutex;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::watch;

use host_core::device::DeviceIdentity;
use host_core::error::AppError;
use host_core::wire::{read_frame, write_frame, Frame, MsgType, HANDSHAKE_TIMEOUT};

/// 一次性码有效期
pub const CODE_TTL: Duration = Duration::from_secs(120);
/// 错误尝试上限（达到即作废当前码）
pub const MAX_ATTEMPTS: u8 = 5;

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
    /// Reject 时的原因：code | fingerprint | self | busy
    reason: Option<String>,
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
        })
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
            // ③ 一次性码校验
            if let Err(e) = self.codes.validate(&req.code) {
                let reason = match e {
                    CodeError::Mismatch => "code",
                    CodeError::Expired => "code",
                    CodeError::Consumed => "code",
                };
                tracing::warn!(from = %req.device_id, ?e, "KVM 配对码校验失败");
                reject!(reason);
            }
            // ④ 落盘 + 应答
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

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

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
}
