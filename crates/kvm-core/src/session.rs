//! K3 会话层：TCP 帧协议 + 加密（docs/impl/05 K3）。
//!
//! 帧格式（length-prefixed）：
//! `[u32 len][u8 msg_type][u8 flags][payload]`
//! len = 帧头后字节数（msg_type + flags + payload）。
//! msg_type：0x01 Hello 0x02 InputEvent 0x03 ClipData 0x04 FileChunk
//!           0x05 FileMeta 0x06 Ack 0x07 Ping
//!           （K2 扩展：0x08 PairRequest 0x09 PairAccept 0x0A PairReject）
//!           （K7 扩展：0x0B ControlTake 0x0C ControlRelease）
//!
//! 加密：配对时 X25519 协商共享密钥 → HKDF-SHA256 → ChaCha20-Poly1305
//! 全帧加密（握手完成后的帧 flags 携带单调递增的低位 nonce 序号，完整
//! nonce = 4B 固定前缀(派生) + 8B 计数器）。本文件先落地帧编解码与
//! 密钥派生；Session 连接管理在 K2 配对流程之后接入。

use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};

use host_core::error::AppError;

/// TCP 会话监听端口（随 K1 心跳广播）
pub const SESSION_PORT: u16 = 49801;

/// 帧头固定字节数（msg_type + flags，长度前缀除外）
pub const HEADER_LEN: usize = 2;
/// 单帧 payload 上限（4MB：FileChunk 满载 + 余量）
pub const MAX_PAYLOAD: usize = 4 * 1024 * 1024;

/// 会话消息类型（docs/impl/05 K3；0x08–0x0A 为 K2 配对扩展；0x0B–0x0C 为 K7 控制扩展）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MsgType {
    Hello = 0x01,
    InputEvent = 0x02,
    ClipData = 0x03,
    FileChunk = 0x04,
    FileMeta = 0x05,
    Ack = 0x06,
    Ping = 0x07,
    PairRequest = 0x08,
    PairAccept = 0x09,
    PairReject = 0x0A,
    ControlTake = 0x0B,
    ControlRelease = 0x0C,
}

impl MsgType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Hello),
            0x02 => Some(Self::InputEvent),
            0x03 => Some(Self::ClipData),
            0x04 => Some(Self::FileChunk),
            0x05 => Some(Self::FileMeta),
            0x06 => Some(Self::Ack),
            0x07 => Some(Self::Ping),
            0x08 => Some(Self::PairRequest),
            0x09 => Some(Self::PairAccept),
            0x0A => Some(Self::PairReject),
            0x0B => Some(Self::ControlTake),
            0x0C => Some(Self::ControlRelease),
            _ => None,
        }
    }
}

/// 明文帧（编解码最小单元）
#[derive(Clone, Debug)]
pub struct Frame {
    pub msg_type: MsgType,
    pub flags: u8,
    pub payload: Vec<u8>,
}

/// 从流读取一帧（半帧/粘包安全；调用方负责整体握手超时）。
/// 泛化以支持 TcpStream 与OwnedReadHalf（会话收发分离）。
pub async fn read_frame<S>(stream: &mut S) -> Result<Frame, AppError>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let io_err = |e: std::io::Error| AppError::module("KVM_SESSION_006", e.to_string(), None);
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(io_err)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if !(HEADER_LEN..=HEADER_LEN + MAX_PAYLOAD).contains(&len) {
        return Err(AppError::module("KVM_SESSION_002", "帧载荷超限或长度非法", None));
    }
    let mut rest = vec![0u8; len];
    stream.read_exact(&mut rest).await.map_err(io_err)?;
    let msg_type = MsgType::from_u8(rest[0]).ok_or_else(|| {
        AppError::module("KVM_SESSION_003", format!("未知消息类型 0x{:02x}", rest[0]), None)
    })?;
    Ok(Frame { msg_type, flags: rest[1], payload: rest[2..].to_vec() })
}

/// 向流写一帧
pub async fn write_frame<S>(stream: &mut S, frame: &Frame) -> Result<(), AppError>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let wire = encode_frame(frame);
    stream
        .write_all(&wire)
        .await
        .map_err(|e| AppError::module("KVM_SESSION_006", e.to_string(), None))
}

/// 编码为线格式：`[u32 len][type][flags][payload]`（len = HEADER_LEN + payload）
pub fn encode_frame(frame: &Frame) -> Vec<u8> {
    let len = (HEADER_LEN + frame.payload.len()) as u32;
    let mut out = Vec::with_capacity(4 + len as usize);
    out.extend_from_slice(&len.to_be_bytes());
    out.push(frame.msg_type as u8);
    out.push(frame.flags);
    out.extend_from_slice(&frame.payload);
    out
}

/// 从缓冲区解码一帧；返回 (帧, 消耗字节数)。
/// 缓冲不足（需更多数据）返回 Ok(None)；非法帧返回 Err。
pub fn decode_frame(buf: &[u8]) -> Result<Option<(Frame, usize)>, AppError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len < HEADER_LEN {
        return Err(AppError::module("KVM_SESSION_001", "帧长度小于帧头", None));
    }
    if len > HEADER_LEN + MAX_PAYLOAD {
        return Err(AppError::module("KVM_SESSION_002", "帧载荷超限", None));
    }
    let total = 4 + len;
    if buf.len() < total {
        return Ok(None); // 半帧，等待更多数据
    }
    let msg_type = MsgType::from_u8(buf[4])
        .ok_or_else(|| AppError::module("KVM_SESSION_003", format!("未知消息类型 0x{:02x}", buf[4]), None))?;
    let flags = buf[5];
    let payload = buf[6..total].to_vec();
    Ok(Some((Frame { msg_type, flags, payload }, total)))
}

/// 由 X25519 共享密钥派生 ChaCha20-Poly1305 会话密钥
/// （salt = 双方指纹拼接的 SHA256，info = b"nexusforge-kvm-v1"）。
/// shared 通常为 64B 双 DH 拼接：静态配对 DH（鉴权）|| 临时 DH（保新鲜）。
pub fn derive_session_key(shared: &[u8], salt_material: &[u8]) -> [u8; 32] {
    let salt: [u8; 32] = Sha256::digest(salt_material).into();
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut okm = [0u8; 32];
    hk.expand(b"nexusforge-kvm-v1", &mut okm).expect("HKDF 扩展长度合法");
    okm
}

/// 会话加密器：单一方向一个实例（收/发各一，nonce 计数器独立）
pub struct FrameCipher {
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl FrameCipher {
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(&key)),
            counter: 0,
        }
    }

    /// 加密一帧 payload；nonce = 8B 计数器（小端）+ 4B 零前缀，随密文返回
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<(chacha20poly1305::Nonce, Vec<u8>), AppError> {
        use chacha20poly1305::aead::Aead;
        let n = self.next_nonce();
        let nonce = chacha20poly1305::Nonce::from_slice(&n);
        let ct = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| AppError::module("KVM_SESSION_004", "帧加密失败", None))?;
        Ok((*nonce, ct))
    }

    pub fn open(&mut self, nonce: &[u8; 12], ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
        use chacha20poly1305::aead::Aead;
        self.cipher
            .decrypt(chacha20poly1305::Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| AppError::module("KVM_SESSION_005", "帧解密失败（密钥或序号不匹配）", None))
    }

    fn next_nonce(&mut self) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&self.counter.to_le_bytes());
        self.counter = self.counter.wrapping_add(1);
        nonce
    }
}

// ---------------------------------------------------------------------------
// K3 会话连接管理：Hello 握手 → 白名单校验 → 双 DH 派生 → 加密帧收发循环
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use x25519_dalek::{EphemeralSecret, PublicKey as X25519PublicKey};

use crate::identity::DeviceIdentity;
use crate::pairing::{b64_decode, b64_encode, PairStore, PairedCb, PairedPeer, PairingService};

/// 握手整体超时（Hello 交换 + 派生）
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// 加密帧 payload 前缀：12B nonce（收发方向计数器独立）
const NONCE_LEN: usize = 12;
/// AEAD Poly1305 tag 长度
const TAG_LEN: usize = 16;

/// 会话握手 Hello 载荷（明文帧；机密性由握手后的加密帧保证）
#[derive(Serialize, Deserialize)]
pub struct HelloPayload {
    pub device_id: String,
    pub device_name: String,
    /// 静态公钥指纹（对端须与配对记录一致）
    pub fingerprint: String,
    /// base64(本会话临时 X25519 公钥)
    pub eph_pubkey_b64: String,
}

/// 会话事件（模块层消费：发布 kvm.session_state / 交付输入帧）
#[derive(Clone, Debug)]
pub enum SessionEvent {
    Established { device_id: String, device_name: String },
    Closed { device_id: String, reason: String },
    Frame { device_id: String, frame: Frame },
}

/// 单条会话句柄：send 排队给 writer 任务；close() 或句柄 drop 即关断会话
/// （reader/writer 收到 close 信号退出 → 写半释放 → 对端 EOF）。
/// 注册表仅存元数据，不持有句柄，故"最后一个外部句柄丢弃"即关闭。
#[derive(Clone)]
pub struct SessionHandle {
    pub device_id: String,
    pub device_name: String,
    tx: mpsc::Sender<Frame>,
    close_tx: Arc<watch::Sender<bool>>,
}

impl SessionHandle {
    pub async fn send(&self, frame: Frame) -> Result<(), AppError> {
        self.tx
            .send(frame)
            .await
            .map_err(|_| AppError::module("KVM_SESSION_009", "会话已关闭", None))
    }

    /// 主动关断（幂等）
    pub fn close(&self) {
        let _ = self.close_tx.send(true);
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        // 仅当这是最后一个句柄副本时才关断会话。中间副本（SessionManager::
        // get_handle 的临时克隆、worker 每帧的 lookup clone 等）用完即弃，
        // 若每次 drop 都发 close 会误杀活会话（K7 转发链路实测踩坑）。
        if Arc::strong_count(&self.close_tx) == 1 {
            let _ = self.close_tx.send(true);
        }
    }
}

type SessionRegistry = Arc<Mutex<HashMap<String, SessionHandle>>>;
/// 注册表语义：**仅服务端会话**（manager 持有句柄保活；reader 退出时移除）。
/// 客户端会话句柄唯一归调用方，不入注册表——drop 即关断。

/// 加密帧写出：payload = nonce || ciphertext
pub async fn write_encrypted_frame<S>(stream: &mut S, cipher: &mut FrameCipher, frame: &Frame) -> Result<(), AppError>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let (nonce, ct) = cipher.seal(&frame.payload)?;
    let mut payload = Vec::with_capacity(NONCE_LEN + ct.len());
    payload.extend_from_slice(nonce.as_slice());
    payload.extend_from_slice(&ct);
    write_frame(stream, &Frame { msg_type: frame.msg_type, flags: frame.flags, payload }).await
}

/// 加密帧读取：解析 nonce 前缀并解密
pub async fn read_encrypted_frame<S>(stream: &mut S, cipher: &mut FrameCipher) -> Result<Frame, AppError>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let frame = read_frame(stream).await?;
    if frame.payload.len() < NONCE_LEN + TAG_LEN {
        return Err(AppError::module("KVM_SESSION_005", "密文帧长度非法", None));
    }
    let (nonce, ct) = frame.payload.split_at(NONCE_LEN);
    let mut n = [0u8; NONCE_LEN];
    n.copy_from_slice(nonce);
    let payload = cipher.open(&n, ct)?;
    Ok(Frame { msg_type: frame.msg_type, flags: frame.flags, payload })
}

fn parse_hello(frame: Frame) -> Result<HelloPayload, AppError> {
    if frame.msg_type != MsgType::Hello {
        return Err(AppError::module("KVM_SESSION_010", format!("非 Hello 帧: {:?}", frame.msg_type), None));
    }
    serde_json::from_slice(&frame.payload)
        .map_err(|e| AppError::module("KVM_SESSION_010", format!("Hello 载荷非法: {e}"), None))
}

/// 白名单准入：device_id 必须已配对且指纹一致；返回配对记录与对端静态公钥
fn verify_peer(store: &PairStore, hello: &HelloPayload) -> Result<(PairedPeer, [u8; 32]), AppError> {
    let peer = store
        .get(&hello.device_id)
        .ok_or_else(|| AppError::module("KVM_SESSION_007", format!("设备 {} 未配对", hello.device_id), None))?;
    if peer.fingerprint != hello.fingerprint {
        return Err(AppError::module("KVM_SESSION_008", "指纹与配对记录不符", None));
    }
    let raw = b64_decode(&peer.pubkey_b64)
        .ok_or_else(|| AppError::module("KVM_SESSION_011", "配对记录公钥 base64 非法", None))?;
    let pubkey: [u8; 32] = raw
        .try_into()
        .map_err(|_| AppError::module("KVM_SESSION_011", "配对记录公钥长度非法", None))?;
    Ok((peer, pubkey))
}

fn peer_eph_pubkey(hello: &HelloPayload) -> Result<[u8; 32], AppError> {
    let raw = b64_decode(&hello.eph_pubkey_b64)
        .ok_or_else(|| AppError::module("KVM_SESSION_011", "临时公钥 base64 非法", None))?;
    raw.try_into()
        .map_err(|_| AppError::module("KVM_SESSION_011", "临时公钥长度非法", None))
}

fn own_hello(identity: &DeviceIdentity, eph_pub: &X25519PublicKey) -> Frame {
    Frame {
        msg_type: MsgType::Hello,
        flags: 0,
        payload: serde_json::to_vec(&HelloPayload {
            device_id: identity.device_id.clone(),
            device_name: identity.device_name.clone(),
            fingerprint: identity.pubkey_fingerprint.clone(),
            eph_pubkey_b64: b64_encode(&eph_pub.to_bytes()),
        })
        .unwrap_or_default(),
    }
}

/// 会话密钥：shared = 静态 DH（鉴权，配对即知对端静态公钥）|| 临时 DH（保新鲜）；
/// salt = 双方指纹按字典序拼接（双方独立计算结果一致）。
fn session_key_from(
    identity: &DeviceIdentity,
    peer_static: &[u8; 32],
    dh2: [u8; 32],
    peer_fingerprint: &str,
) -> Result<[u8; 32], AppError> {
    let dh1 = identity
        .diffie_hellman(peer_static)
        .ok_or_else(|| AppError::module("KVM_SESSION_011", "静态 DH 共享密钥非法（全零）", None))?;
    let mut shared = Vec::with_capacity(64);
    shared.extend_from_slice(&dh1);
    shared.extend_from_slice(&dh2);
    let mut salt = [identity.pubkey_fingerprint.as_bytes(), peer_fingerprint.as_bytes()];
    salt.sort();
    Ok(derive_session_key(&shared, &salt.concat()))
}

async fn ephemeral_dh2(eph: EphemeralSecret, peer_eph: [u8; 32]) -> Result<[u8; 32], AppError> {
    let shared = eph.diffie_hellman(&X25519PublicKey::from(peer_eph));
    if !shared.was_contributory() {
        return Err(AppError::module("KVM_SESSION_011", "临时 DH 共享密钥非法（全零）", None));
    }
    Ok(shared.to_bytes())
}

/// 服务端握手：Hello 已由分流读出 → 校验 → 回 Hello → 派生 → 拆分读写半
async fn server_handshake(
    mut stream: TcpStream,
    identity: &Arc<DeviceIdentity>,
    store: &PairStore,
    first: Frame,
) -> Result<(OwnedReadHalf, OwnedWriteHalf, FrameCipher, FrameCipher, PairedPeer), AppError> {
    let hello = parse_hello(first)?;
    let (peer, peer_static) = verify_peer(store, &hello)?;
    let peer_eph = peer_eph_pubkey(&hello)?;

    let eph = EphemeralSecret::random_from_rng(rand::rngs::OsRng);
    let eph_pub = X25519PublicKey::from(&eph);
    write_frame(&mut stream, &own_hello(identity, &eph_pub)).await?;

    let dh2 = ephemeral_dh2(eph, peer_eph).await?;
    let key = session_key_from(identity, &peer_static, dh2, &peer.fingerprint)?;
    let (rd, wr) = stream.into_split();
    Ok((rd, wr, FrameCipher::new(key), FrameCipher::new(key), peer))
}

/// 客户端握手：发 Hello → 收 Hello → 校验 → 派生 → 拆分读写半
async fn client_handshake(
    mut stream: TcpStream,
    identity: &Arc<DeviceIdentity>,
    store: &PairStore,
) -> Result<(OwnedReadHalf, OwnedWriteHalf, FrameCipher, FrameCipher, PairedPeer), AppError> {
    let eph = EphemeralSecret::random_from_rng(rand::rngs::OsRng);
    let eph_pub = X25519PublicKey::from(&eph);
    write_frame(&mut stream, &own_hello(identity, &eph_pub)).await?;

    let reply = read_frame(&mut stream).await?;
    let hello = parse_hello(reply)?;
    let (peer, peer_static) = verify_peer(store, &hello)?;
    let peer_eph = peer_eph_pubkey(&hello)?;

    let dh2 = ephemeral_dh2(eph, peer_eph).await?;
    let key = session_key_from(identity, &peer_static, dh2, &peer.fingerprint)?;
    let (rd, wr) = stream.into_split();
    Ok((rd, wr, FrameCipher::new(key), FrameCipher::new(key), peer))
}

/// 启动读写循环并登记会话。writer 持有写半与 tx cipher；reader 持有读半与
/// rx cipher，Ping 空载荷探测自动回 pong（载荷 b"1"，对端收到后不再回复）。
/// close 信号（watch）任一端触发即双向退出：writer 释放写半 → 对端 EOF。
#[allow(clippy::too_many_arguments)]
fn spawn_session(
    device_id: String,
    device_name: String,
    mut rd: OwnedReadHalf,
    mut wr: OwnedWriteHalf,
    rx_cipher: FrameCipher,
    tx_cipher: FrameCipher,
    registry: SessionRegistry,
    events: mpsc::UnboundedSender<SessionEvent>,
) -> SessionHandle {
    let (tx, mut rxq) = mpsc::channel::<Frame>(64);
    let (close_tx, close_rx) = watch::channel(false);

    // 写循环：close 信号或队列关闭（句柄全弃）→ writer 退出 → 对端读到 EOF
    {
        let mut tx_cipher = tx_cipher;
        let mut close_rx_writer = close_rx.clone();
        let wid = device_id.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = close_rx_writer.changed() => {
                        tracing::info!(device_id = %wid, "KVM writer 退出：close 信号");
                        break
                    }
                    frame = rxq.recv() => match frame {
                        Some(frame) => {
                            if write_encrypted_frame(&mut wr, &mut tx_cipher, &frame).await.is_err() {
                                tracing::warn!(device_id = %wid, "KVM writer 退出：写帧失败");
                                break;
                            }
                        }
                        None => {
                            tracing::info!(device_id = %wid, "KVM writer 退出：发送队列关闭（句柄全弃）");
                            break
                        }
                    },
                }
            }
        });
    }

    // 读循环：解密 → 事件上抛；退出时移除注册表并广播 Closed
    {
        let device_id_reader = device_id.clone();
        let mut rx_cipher = rx_cipher;
        let ping_tx = tx.clone();
        let registry_reader = registry.clone();
        let events_reader = events.clone();
        let mut close_rx_reader = close_rx;
        tokio::spawn(async move {
            let reason = loop {
                tokio::select! {
                    _ = close_rx_reader.changed() => break "closed_by_local",
                    res = read_encrypted_frame(&mut rd, &mut rx_cipher) => match res {
                        Ok(frame) => match frame.msg_type {
                            MsgType::Ping if frame.payload.is_empty() => {
                                let _ = ping_tx
                                    .try_send(Frame { msg_type: MsgType::Ping, flags: 0, payload: vec![1] });
                            }
                            _ => {
                                let _ = events_reader.send(SessionEvent::Frame {
                                    device_id: device_id_reader.clone(),
                                    frame,
                                });
                            }
                        },
                        Err(e) => {
                            tracing::warn!(device_id = %device_id_reader, error = %e, "KVM 会话读循环退出");
                            break "closed"
                        }
                    },
                }
            };
            registry_reader.lock().expect("会话注册表锁").remove(&device_id_reader);
            let _ = events_reader.send(SessionEvent::Closed { device_id: device_id_reader, reason: reason.into() });
        });
    }

    SessionHandle { device_id, device_name, tx, close_tx: Arc::new(close_tx) }
}

/// 服务端总入口：SESSION_PORT 单监听按首帧分流（PairRequest→K2 配对；Hello→K3 会话）
pub struct SessionManager {
    identity: Arc<DeviceIdentity>,
    pairing: Arc<PairingService>,
    store: Arc<PairStore>,
    registry: SessionRegistry,
}

pub struct SessionServeHandle {
    shutdown_tx: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl SessionServeHandle {
    pub async fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        for t in self.tasks.drain(..) {
            let _ = t.await;
        }
    }
}

impl Drop for SessionServeHandle {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        for t in self.tasks.drain(..) {
            t.abort();
        }
    }
}

impl SessionManager {
    pub fn new(identity: Arc<DeviceIdentity>, pairing: Arc<PairingService>, store: Arc<PairStore>) -> Arc<Self> {
        Arc::new(Self { identity, pairing, store, registry: Arc::new(Mutex::new(HashMap::new())) })
    }

    /// 活跃会话快照（仅服务端接入的会话；device_id, device_name）
    pub fn active_sessions(&self) -> Vec<(String, String)> {
        let mut list: Vec<(String, String)> = self
            .registry
            .lock()
            .expect("会话注册表锁")
            .values()
            .map(|h| (h.device_id.clone(), h.device_name.clone()))
            .collect();
        list.sort();
        list
    }

    /// 取服务端会话句柄（仅注册表内的服务端接入会话；客户端会话句柄归调用方）
    pub fn get_handle(&self, device_id: &str) -> Option<SessionHandle> {
        self.registry
            .lock()
            .expect("会话注册表锁")
            .get(device_id)
            .cloned()
    }

    /// 接入循环：accept → 读首帧 → 分流。on_paired 转发 K2 配对成功回调。
    pub async fn serve(
        self: Arc<Self>,
        listener: TcpListener,
        events: mpsc::UnboundedSender<SessionEvent>,
        on_paired: PairedCb,
    ) -> SessionServeHandle {
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
                        let mgr = self.clone();
                        let events = events.clone();
                        let on_paired = on_paired.clone();
                        tokio::spawn(async move { mgr.handle_incoming(stream, events, on_paired).await });
                    }
                    _ = accept_shutdown.changed() => break,
                }
            }
        }));
        SessionServeHandle { shutdown_tx, tasks }
    }

    async fn handle_incoming(
        self: Arc<Self>,
        mut stream: TcpStream,
        events: mpsc::UnboundedSender<SessionEvent>,
        on_paired: PairedCb,
    ) {
        let first = match tokio::time::timeout(HANDSHAKE_TIMEOUT, read_frame(&mut stream)).await {
            Ok(Ok(f)) => f,
            _ => return,
        };
        match first.msg_type {
            MsgType::PairRequest => {
                if let Some(peer) = self.pairing.handle_pair_conn(stream, first).await {
                    if let Some(f) = on_paired.lock().expect("paired cb 锁").as_ref() {
                        f(peer);
                    }
                }
            }
            MsgType::Hello => {
                let mgr = self.clone();
                let hs = async move {
                    let (rd, wr, rx_cipher, tx_cipher, peer) =
                        server_handshake(stream, &mgr.identity, &mgr.store, first).await?;
                    let handle = spawn_session(
                        peer.device_id.clone(),
                        peer.device_name.clone(),
                        rd,
                        wr,
                        rx_cipher,
                        tx_cipher,
                        mgr.registry.clone(),
                        events.clone(),
                    );
                    // 服务端持有句柄保活；reader 退出时自动移除
                    mgr.registry.lock().expect("会话注册表锁").insert(peer.device_id.clone(), handle);
                    let _ = events.send(SessionEvent::Established {
                        device_id: peer.device_id.clone(),
                        device_name: peer.device_name.clone(),
                    });
                    tracing::info!(peer = %peer.device_id, "KVM 会话已建立（服务端）");
                    Ok::<(), AppError>(())
                };
                match tokio::time::timeout(HANDSHAKE_TIMEOUT, hs).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(error = %e, "KVM 会话握手失败（服务端）"),
                    Err(_) => tracing::warn!("KVM 会话握手超时（服务端）"),
                }
            }
            _ => {} // 未知首帧：静默丢弃（防端口探测）
        }
    }

    /// 客户端发起会话（须已配对）。返回的句柄是唯一所有权：drop 即关断。
    pub async fn connect(
        &self,
        addr: SocketAddr,
        events: mpsc::UnboundedSender<SessionEvent>,
    ) -> Result<SessionHandle, AppError> {
        let identity = self.identity.clone();
        let store = self.store.clone();
        // 注册表仅服务端会话插入；客户端传入仅为复用 spawn_session（reader 移除时 no-op）
        let registry = self.registry.clone();
        let hs = async move {
            let stream = TcpStream::connect(addr)
                .await
                .map_err(|e| AppError::module("KVM_SESSION_006", e.to_string(), None))?;
            let (rd, wr, rx_cipher, tx_cipher, peer) = client_handshake(stream, &identity, &store).await?;
            let handle = spawn_session(
                peer.device_id.clone(),
                peer.device_name.clone(),
                rd,
                wr,
                rx_cipher,
                tx_cipher,
                registry,
                events.clone(),
            );
            let _ = events.send(SessionEvent::Established {
                device_id: peer.device_id.clone(),
                device_name: peer.device_name.clone(),
            });
            tracing::info!(peer = %peer.device_id, "KVM 会话已建立（客户端）");
            Ok(handle)
        };
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, hs).await {
            Ok(v) => v,
            Err(_) => Err(AppError::module("KVM_SESSION_001", "会话握手超时", None)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::PairCodeManager;

    #[test]
    fn frame_roundtrip() {
        let frame = Frame {
            msg_type: MsgType::InputEvent,
            flags: 0x80,
            payload: vec![1, 2, 3, 250],
        };
        let wire = encode_frame(&frame);
        assert_eq!(wire.len(), 4 + HEADER_LEN + 4);
        let (decoded, consumed) = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(consumed, wire.len());
        assert_eq!(decoded.msg_type, MsgType::InputEvent);
        assert_eq!(decoded.flags, 0x80);
        assert_eq!(decoded.payload, vec![1, 2, 3, 250]);
    }

    #[test]
    fn partial_frame_returns_none() {
        let frame = Frame { msg_type: MsgType::Ping, flags: 0, payload: vec![9; 100] };
        let wire = encode_frame(&frame);
        let half = decode_frame(&wire[..50]).unwrap();
        assert!(half.is_none());
        let (done, consumed) = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(consumed, wire.len());
        assert_eq!(done.msg_type, MsgType::Ping);
    }

    #[test]
    fn unknown_type_rejected() {
        let mut wire = encode_frame(&Frame { msg_type: MsgType::Ping, flags: 0, payload: vec![] });
        wire[4] = 0xEE;
        assert!(decode_frame(&wire).is_err());
    }

    #[test]
    fn cipher_roundtrip_and_tamper() {
        let mut tx = FrameCipher::new([7u8; 32]);
        let mut rx = FrameCipher::new([7u8; 32]);
        let (nonce, ct) = tx.seal(b"hello kvm").unwrap();
        let pt = rx.open(&nonce.into(), &ct).unwrap();
        assert_eq!(pt, b"hello kvm");
        // 篡改必失败
        let mut bad = ct.clone();
        bad[0] ^= 0xFF;
        assert!(rx.open(&nonce.into(), &bad).is_err());
    }

    #[test]
    fn derived_keys_match_for_same_material() {
        let k1 = derive_session_key(&[1u8; 32], b"fpA fpB");
        let k2 = derive_session_key(&[1u8; 32], b"fpA fpB");
        let k3 = derive_session_key(&[1u8; 32], b"fpB fpA");
        assert_eq!(k1, k2);
        assert_ne!(k1, k3, "salt 顺序不同（角色互换）应派生不同密钥");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn encrypted_stream_roundtrip_via_duplex() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let key = [9u8; 32];
        let mut tx = FrameCipher::new(key);
        let mut rx = FrameCipher::new(key);
        let frame = Frame { msg_type: MsgType::InputEvent, flags: 0, payload: vec![5; 40] };
        let writer = tokio::spawn(async move { write_encrypted_frame(&mut a, &mut tx, &frame).await });
        let reader = tokio::spawn(async move { read_encrypted_frame(&mut b, &mut rx).await });
        writer.await.unwrap().unwrap();
        let got = reader.await.unwrap().unwrap();
        assert_eq!(got.msg_type, MsgType::InputEvent);
        assert_eq!(got.payload, vec![5; 40]);
    }

    // ---- K3 会话握手与加密收发 ----

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("kvm-session-{tag}-{}", uuid::Uuid::now_v7()))
    }

    /// 把 peer 写入 store 白名单（配对流程已由 pairing 测试覆盖，此处直接 seed）
    fn seed_peer(store: &PairStore, peer_id: &Arc<DeviceIdentity>) {
        store
            .upsert(PairedPeer {
                device_id: peer_id.device_id.clone(),
                device_name: peer_id.device_name.clone(),
                fingerprint: peer_id.pubkey_fingerprint.clone(),
                pubkey_b64: b64_encode(&peer_id.public_key()),
                paired_at: 0,
            })
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_handshake_and_encrypted_exchange() {
        let dir_a = temp_dir("a");
        let dir_b = temp_dir("b");
        let id_a = Arc::new(DeviceIdentity::generate("PC-A".into()));
        let id_b = Arc::new(DeviceIdentity::generate("PC-B".into()));
        let store_a = Arc::new(PairStore::load_or_default(&dir_a).unwrap());
        let store_b = Arc::new(PairStore::load_or_default(&dir_b).unwrap());
        seed_peer(&store_a, &id_b);
        seed_peer(&store_b, &id_a);

        let pairing_a = PairingService::new(id_a.clone(), Arc::new(PairCodeManager::new()), store_a.clone());
        let pairing_b = PairingService::new(id_b.clone(), Arc::new(PairCodeManager::new()), store_b.clone());
        let mgr_b = SessionManager::new(id_b.clone(), pairing_b, store_b.clone());
        let (ev_tx_b, mut ev_rx_b) = mpsc::unbounded_channel();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _serve = mgr_b.clone().serve(listener, ev_tx_b, Arc::new(Mutex::new(None))).await;

        let mgr_a = SessionManager::new(id_a.clone(), pairing_a, store_a.clone());
        let (ev_tx_a, mut ev_rx_a) = mpsc::unbounded_channel();
        let handle = tokio::time::timeout(HANDSHAKE_TIMEOUT, mgr_a.connect(addr, ev_tx_a))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(handle.device_id, id_b.device_id);

        // 服务端收到 Established（对端 = A）
        let ev = tokio::time::timeout(HANDSHAKE_TIMEOUT, ev_rx_b.recv()).await.unwrap().unwrap();
        assert!(
            matches!(ev, SessionEvent::Established { ref device_id, .. } if *device_id == id_a.device_id),
            "服务端应收到 Established: {ev:?}"
        );
        // 客户端同样收到 Established（对端 = B）
        let ev = tokio::time::timeout(HANDSHAKE_TIMEOUT, ev_rx_a.recv()).await.unwrap().unwrap();
        assert!(matches!(ev, SessionEvent::Established { .. }), "客户端应收到 Established: {ev:?}");

        // InputEvent 加密送达服务端
        handle.send(Frame { msg_type: MsgType::InputEvent, flags: 0, payload: vec![1, 2, 3] }).await.unwrap();
        let ev = tokio::time::timeout(HANDSHAKE_TIMEOUT, ev_rx_b.recv()).await.unwrap().unwrap();
        match ev {
            SessionEvent::Frame { device_id, frame } => {
                assert_eq!(device_id, id_a.device_id);
                assert_eq!(frame.msg_type, MsgType::InputEvent);
                assert_eq!(frame.payload, vec![1, 2, 3]);
            }
            other => panic!("期望 Frame 事件，实际: {other:?}"),
        }

        // Ping 空载荷探测 → 服务端自动回 pong（载荷 [1]，送达客户端）
        handle.send(Frame { msg_type: MsgType::Ping, flags: 0, payload: vec![] }).await.unwrap();
        let ev = tokio::time::timeout(HANDSHAKE_TIMEOUT, ev_rx_a.recv()).await.unwrap().unwrap();
        match ev {
            SessionEvent::Frame { frame, .. } => {
                assert_eq!(frame.msg_type, MsgType::Ping);
                assert_eq!(frame.payload, vec![1]);
            }
            other => panic!("期望 Ping pong 帧，实际: {other:?}"),
        }

        // 关闭客户端 → 服务端 Closed + 注册表清空
        drop(handle);
        let ev = tokio::time::timeout(HANDSHAKE_TIMEOUT, ev_rx_b.recv()).await.unwrap().unwrap();
        assert!(matches!(ev, SessionEvent::Closed { ref device_id, .. } if *device_id == id_a.device_id));
        assert!(mgr_b.active_sessions().is_empty());
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unpaired_device_rejected() {
        let dir_b = temp_dir("ub");
        let id_b = Arc::new(DeviceIdentity::generate("PC-B".into()));
        let store_b = Arc::new(PairStore::load_or_default(&dir_b).unwrap()); // 空白名单
        let pairing_b = PairingService::new(id_b.clone(), Arc::new(PairCodeManager::new()), store_b.clone());
        let mgr_b = SessionManager::new(id_b, pairing_b, store_b);
        let (ev_tx_b, _ev_rx_b) = mpsc::unbounded_channel();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _serve = mgr_b.clone().serve(listener, ev_tx_b, Arc::new(Mutex::new(None))).await;

        // 未配对设备 X：本端白名单无 B → 客户端握手也过不了
        let dir_x = temp_dir("ux");
        let id_x = Arc::new(DeviceIdentity::generate("PC-X".into()));
        let store_x = Arc::new(PairStore::load_or_default(&dir_x).unwrap());
        let pairing_x = PairingService::new(id_x.clone(), Arc::new(PairCodeManager::new()), store_x.clone());
        let mgr_x = SessionManager::new(id_x.clone(), pairing_x, store_x.clone());
        let (ev_tx_x, _ev_rx_x) = mpsc::unbounded_channel();
        let res = tokio::time::timeout(HANDSHAKE_TIMEOUT, mgr_x.connect(addr, ev_tx_x)).await.unwrap();
        assert!(res.is_err(), "未配对设备必须被拒绝");

        // 服务端未建立任何会话
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(mgr_b.active_sessions().is_empty());
        let _ = std::fs::remove_dir_all(&dir_b);
        let _ = std::fs::remove_dir_all(&dir_x);
    }
}
