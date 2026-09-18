//! SYNC1+SYNC4 传输层（docs/impl/07）：复用 K2/K3 信任根的独立加密通道。
//!
//! - 信任根共享：与 KVM 同一 {appData}/kvm/{identity,paired}.json（同机即同身份）
//! - 握手：Hello（明文 JSON）→ 对端白名单校验（PairStore 指纹比对）→
//!   shared = 静态 DH ‖ 临时 DH → HKDF → ChaCha20-Poly1305（双方收/发 cipher 独立）
//! - 消息帧：`[u32 len][12B nonce][ciphertext]`，payload = SyncMsg JSON（不复用 KVM 帧头，
//!   KVM MsgType 封闭枚举不掺自定义类型）
//! - 仅局域网：监听 0.0.0.0 但连接来源必须已配对（文档风险：中继 v1 不做）

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use x25519_dalek::{EphemeralSecret, PublicKey as X25519PublicKey};

use kvm_core::session::{
    derive_session_key, read_frame, write_frame, Frame, FrameCipher, MsgType, HANDSHAKE_TIMEOUT,
};
use kvm_core::pairing::{b64_decode, b64_encode, PairStore, PairedPeer};
use kvm_core::DeviceIdentity;

use crate::error::{Result, SyncError};
use crate::oplog::OpEntry;

/// 握手 Hello 载荷（与 KVM HelloPayload 同构 JSON）
#[derive(Serialize, Deserialize)]
pub struct SyncHello {
    device_id: String,
    device_name: String,
    fingerprint: String,
    eph_pubkey_b64: String,
}

/// 同步协议消息（加密帧内 JSON）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SyncMsg {
    /// 对端推送变更批次（more = 还有后续批；v1 单批上限 BATCH_LIMIT）
    Push { ops: Vec<OpEntry>, more: bool },
    /// 请求对端产出（since_ts = 本端已收到的最大 ts）
    Pull { since_ts: i64 },
    /// 应用完成回执（until_ts = 已处理到的最大 ts，供推送方对账）
    Ack { until_ts: i64, applied: u32, lost: u32 },
    /// 协议错误（中断会话）
    Err { msg: String },
}

/// 单批变更上限（4MB 帧限 / 每条约 1KB）
pub const BATCH_LIMIT: usize = 512;

/// 已握手会话（读写半 + 双向 cipher + 对端信息）
pub struct SyncSession {
    pub rd: OwnedReadHalf,
    pub wr: OwnedWriteHalf,
    pub rx: FrameCipher,
    pub tx: FrameCipher,
    pub peer: PairedPeer,
}

/// 通用白名单校验（device_id 已配对 + 指纹一致）→ (配对记录, 对端静态公钥)
fn verify_peer(store: &PairStore, hello: &SyncHello) -> Result<(PairedPeer, [u8; 32])> {
    let peer = store
        .get(&hello.device_id)
        .ok_or_else(|| SyncError::Peer(format!("设备 {} 未配对", hello.device_id)))?;
    if peer.fingerprint != hello.fingerprint {
        return Err(SyncError::Peer("指纹与配对记录不符".into()));
    }
    let raw = b64_decode(&peer.pubkey_b64).ok_or_else(|| SyncError::Peer("配对公钥 base64 非法".into()))?;
    let pubkey: [u8; 32] = raw.try_into().map_err(|_| SyncError::Peer("配对公钥长度非法".into()))?;
    Ok((peer, pubkey))
}

fn own_hello(identity: &DeviceIdentity, eph_pub: &X25519PublicKey) -> Frame {
    Frame {
        msg_type: MsgType::Hello,
        flags: 0,
        payload: serde_json::to_vec(&SyncHello {
            device_id: identity.device_id.clone(),
            device_name: identity.device_name.clone(),
            fingerprint: identity.pubkey_fingerprint.clone(),
            eph_pubkey_b64: b64_encode(&eph_pub.to_bytes()),
        })
        .unwrap_or_default(),
    }
}

/// shared = 静态 DH（鉴权）‖ 临时 DH（保新鲜）；salt = 双方指纹字典序拼接
/// （与 kvm session_key_from 同式，info 仍为 kvm 常量——信任根同源）
fn session_key_from(
    identity: &DeviceIdentity,
    peer_static: &[u8; 32],
    dh2: [u8; 32],
    peer_fingerprint: &str,
) -> Result<[u8; 32]> {
    let dh1 = identity
        .diffie_hellman(peer_static)
        .ok_or_else(|| SyncError::Peer("静态 DH 共享密钥非法（全零）".into()))?;
    let mut shared = Vec::with_capacity(64);
    shared.extend_from_slice(&dh1);
    shared.extend_from_slice(&dh2);
    let mut salt = [identity.pubkey_fingerprint.as_bytes(), peer_fingerprint.as_bytes()];
    salt.sort();
    Ok(derive_session_key(&shared, &salt.concat()))
}

async fn ephemeral_dh2(eph: EphemeralSecret, peer_eph: [u8; 32]) -> Result<[u8; 32]> {
    let shared = eph.diffie_hellman(&X25519PublicKey::from(peer_eph));
    if !shared.was_contributory() {
        return Err(SyncError::Peer("临时 DH 共享密钥非法（全零）".into()));
    }
    Ok(shared.to_bytes())
}

pub(crate) async fn read_hello_frame<S>(stream: &mut S) -> Result<SyncHello>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let frame = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_frame(stream))
        .await
        .map_err(|_| SyncError::Net("握手超时".into()))?
        .map_err(|e| SyncError::Net(e.to_string()))?;
    if frame.msg_type != MsgType::Hello {
        return Err(SyncError::Proto(format!("非 Hello 帧: {:?}", frame.msg_type)));
    }
    serde_json::from_slice(&frame.payload).map_err(|e| SyncError::Proto(format!("Hello 载荷非法: {e}")))
}

async fn write_hello_frame<S>(stream: &mut S, hello: &Frame) -> Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(HANDSHAKE_TIMEOUT, write_frame(stream, hello))
        .await
        .map_err(|_| SyncError::Net("握手写超时".into()))?
        .map_err(|e| SyncError::Net(e.to_string()))
}

/// 客户端握手：发 Hello → 收 Hello → 校验 → 派生
pub async fn handshake_client(
    mut stream: TcpStream,
    identity: &Arc<DeviceIdentity>,
    store: &PairStore,
) -> Result<SyncSession> {
    let eph = EphemeralSecret::random_from_rng(rand::rngs::OsRng);
    let eph_pub = X25519PublicKey::from(&eph);
    write_hello_frame(&mut stream, &own_hello(identity, &eph_pub)).await?;

    let hello = read_hello_frame(&mut stream).await?;
    let (peer, peer_static) = verify_peer(store, &hello)?;
    let peer_eph_raw = b64_decode(&hello.eph_pubkey_b64)
        .ok_or_else(|| SyncError::Proto("对端临时公钥 base64 非法".into()))?;
    let peer_eph: [u8; 32] = peer_eph_raw
        .try_into()
        .map_err(|_| SyncError::Proto("对端临时公钥长度非法".into()))?;
    let dh2 = ephemeral_dh2(eph, peer_eph).await?;
    let key = session_key_from(identity, &peer_static, dh2, &peer.fingerprint)?;
    let (rd, wr) = stream.into_split();
    Ok(SyncSession { rd, wr, rx: FrameCipher::new(key), tx: FrameCipher::new(key), peer })
}

/// 服务端握手：Hello 已由 accept 分流读出
pub async fn handshake_server(
    mut stream: TcpStream,
    identity: &Arc<DeviceIdentity>,
    store: &PairStore,
    hello: SyncHello,
) -> Result<SyncSession> {
    let (peer, peer_static) = verify_peer(store, &hello)?;
    let peer_eph_raw = b64_decode(&hello.eph_pubkey_b64)
        .ok_or_else(|| SyncError::Proto("对端临时公钥 base64 非法".into()))?;
    let peer_eph: [u8; 32] = peer_eph_raw
        .try_into()
        .map_err(|_| SyncError::Proto("对端临时公钥长度非法".into()))?;

    let eph = EphemeralSecret::random_from_rng(rand::rngs::OsRng);
    let eph_pub = X25519PublicKey::from(&eph);
    write_hello_frame(&mut stream, &own_hello(identity, &eph_pub)).await?;

    let dh2 = ephemeral_dh2(eph, peer_eph).await?;
    let key = session_key_from(identity, &peer_static, dh2, &peer.fingerprint)?;
    let (rd, wr) = stream.into_split();
    Ok(SyncSession { rd, wr, rx: FrameCipher::new(key), tx: FrameCipher::new(key), peer })
}

/// 读取一条加密消息（`[u32 len][12B nonce][ct]`，ct 尾部含 Poly1305 tag）
pub async fn read_msg(session: &mut SyncSession) -> Result<SyncMsg> {
    let mut len_buf = [0u8; 4];
    session
        .rd
        .read_exact(&mut len_buf)
        .await
        .map_err(|e| SyncError::Net(e.to_string()))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len < 12 + 16 || len > 4 * 1024 * 1024 {
        return Err(SyncError::Proto(format!("消息长度非法: {len}")));
    }
    let mut body = vec![0u8; len];
    session
        .rd
        .read_exact(&mut body)
        .await
        .map_err(|e| SyncError::Net(e.to_string()))?;
    let (nonce, ct) = body.split_at(12);
    let mut n = [0u8; 12];
    n.copy_from_slice(nonce);
    let plain = session.rx.open(&n, ct).map_err(|e| SyncError::Net(e.to_string()))?;
    serde_json::from_slice(&plain).map_err(|e| SyncError::Proto(format!("消息载荷非法: {e}")))
}

/// 写出一条加密消息
pub async fn write_msg(session: &mut SyncSession, msg: &SyncMsg) -> Result<()> {
    let plain = serde_json::to_vec(msg).map_err(|e| SyncError::Proto(e.to_string()))?;
    let (nonce, ct) = session.tx.seal(&plain).map_err(|e| SyncError::Net(e.to_string()))?;
    let mut body = Vec::with_capacity(12 + ct.len());
    body.extend_from_slice(nonce.as_slice());
    body.extend_from_slice(&ct);
    let mut wire = Vec::with_capacity(4 + body.len());
    wire.extend_from_slice(&(body.len() as u32).to_be_bytes());
    wire.extend_from_slice(&body);
    session
        .wr
        .write_all(&wire)
        .await
        .map_err(|e| SyncError::Net(e.to_string()))?;
    session.wr.flush().await.map_err(|e| SyncError::Net(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvm_core::pairing::PairedPeer;

    /// 构造互信的两台设备（临时目录共享 identity.json 目录布局）
    fn paired_devices(tag: &str) -> (Arc<DeviceIdentity>, PairStore, Arc<DeviceIdentity>, PairStore) {
        let dir_a = std::env::temp_dir().join(format!("nf_sync_tr_{tag}_a_{}", std::process::id()));
        let dir_b = std::env::temp_dir().join(format!("nf_sync_tr_{tag}_b_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
        let id_a = Arc::new(DeviceIdentity::load_or_create(&dir_a, None).unwrap());
        let id_b = Arc::new(DeviceIdentity::load_or_create(&dir_b, None).unwrap());
        let store_a = PairStore::load_or_default(&dir_a).unwrap();
        let store_b = PairStore::load_or_default(&dir_b).unwrap();
        // 互配：A 记录 B，B 记录 A
        let pb = PairedPeer {
            device_id: id_b.device_id.clone(),
            device_name: id_b.device_name.clone(),
            fingerprint: id_b.pubkey_fingerprint.clone(),
            pubkey_b64: b64_encode(&id_b.public_key()),
            paired_at: 0,
        };
        let pa = PairedPeer {
            device_id: id_a.device_id.clone(),
            device_name: id_a.device_name.clone(),
            fingerprint: id_a.pubkey_fingerprint.clone(),
            pubkey_b64: b64_encode(&id_a.public_key()),
            paired_at: 0,
        };
        store_a.upsert(pb).unwrap();
        store_b.upsert(pa).unwrap();
        (id_a, store_a, id_b, store_b)
    }

    #[tokio::test]
    async fn handshake_and_encrypted_roundtrip() {
        let (id_a, store_a, id_b, store_b) = paired_devices("hs");
        let b_id = id_b.device_id.clone();
        let a_id = id_a.device_id.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // 服务端任务：accept → 读 Hello → 握手（独立身份/存储 move 进闭包）
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = stream;
            let hello = read_hello_frame(&mut stream).await.unwrap();
            handshake_server(stream, &id_b, &store_b, hello).await.unwrap()
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut client = handshake_client(stream, &id_a, &store_a).await.unwrap();
        let mut server = server.await.unwrap();

        assert_eq!(client.peer.device_id, b_id);
        assert_eq!(server.peer.device_id, a_id);

        // 加密消息往返（双向独立 cipher 计数器；多轮验证 nonce 单调）
        for i in 0..3 {
            write_msg(&mut client, &SyncMsg::Pull { since_ts: i }).await.unwrap();
            match read_msg(&mut server).await.unwrap() {
                SyncMsg::Pull { since_ts } => assert_eq!(since_ts, i),
                m => panic!("非 Pull: {m:?}"),
            }
            write_msg(&mut server, &SyncMsg::Ack { until_ts: i + 1, applied: 1, lost: 0 }).await.unwrap();
            match read_msg(&mut client).await.unwrap() {
                SyncMsg::Ack { applied, .. } => assert_eq!(applied, 1),
                m => panic!("非 Ack: {m:?}"),
            }
        }
    }

    /// 未配对设备（指纹不符/不在白名单）握手被拒
    #[test]
    fn unpaired_device_rejected() {
        let (id_a, store_a, _id_b, _store_b) = paired_devices("up");
        // 未配对的第三方设备 X（不在 A 白名单）
        let dir_x = std::env::temp_dir().join(format!("nf_sync_tr_up_x_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir_x);
        let id_x = DeviceIdentity::load_or_create(&dir_x, None).unwrap();
        let hello = SyncHello {
            device_id: id_x.device_id.clone(),
            device_name: id_x.device_name.clone(),
            fingerprint: id_x.pubkey_fingerprint.clone(),
            eph_pubkey_b64: b64_encode(&id_x.public_key()),
        };
        let err = verify_peer(&store_a, &hello).unwrap_err();
        assert!(err.to_string().contains("未配对"));
        // 在白名单但指纹不符（中间人）
        store_a
            .upsert(PairedPeer {
                device_id: id_x.device_id.clone(),
                device_name: id_x.device_name.clone(),
                fingerprint: "0".repeat(32),
                pubkey_b64: b64_encode(&id_x.public_key()),
                paired_at: 0,
            })
            .unwrap();
        let err = verify_peer(&store_a, &hello).unwrap_err();
        assert!(err.to_string().contains("指纹"));
        let _ = std::fs::remove_dir_all(&dir_x);
        let _ = (id_a, store_a);
    }
}
