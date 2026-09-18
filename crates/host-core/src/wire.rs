//! 设备间线协议原语（D-02 上移自 kvm-core::session）：帧编解码 + HKDF 会话密钥
//! 派生 + ChaCha20-Poly1305 帧加密器。
//!
//! KVM 会话（K3）与 SYNC 传输（impl/07 SYNC1）共用同一信任根派生公式与帧格式，
//! 属宿主级协议契约；模块 crate 一律经此消费，杜绝模块间直接依赖（DESIGN O1）。
//!
//! 帧格式（length-prefixed）：
//! `[u32 len][u8 msg_type][u8 flags][payload]`
//! len = 帧头后字节数（msg_type + flags + payload）。

use std::time::Duration;

use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit};
use hkdf::Hkdf;
use sha2::Sha256;

use crate::error::AppError;

/// 帧头固定字节数（msg_type + flags，长度前缀除外）
pub const HEADER_LEN: usize = 2;
/// 单帧 payload 上限（4MB：FileChunk 满载 + 余量）
pub const MAX_PAYLOAD: usize = 4 * 1024 * 1024;
/// 握手整体超时（Hello 交换 + 派生）
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

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
        return Err(AppError::module(
            "KVM_SESSION_002",
            "帧载荷超限或长度非法",
            None,
        ));
    }
    let mut rest = vec![0u8; len];
    stream.read_exact(&mut rest).await.map_err(io_err)?;
    let msg_type = MsgType::from_u8(rest[0]).ok_or_else(|| {
        AppError::module(
            "KVM_SESSION_003",
            format!("未知消息类型 0x{:02x}", rest[0]),
            None,
        )
    })?;
    Ok(Frame {
        msg_type,
        flags: rest[1],
        payload: rest[2..].to_vec(),
    })
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
    let msg_type = MsgType::from_u8(buf[4]).ok_or_else(|| {
        AppError::module(
            "KVM_SESSION_003",
            format!("未知消息类型 0x{:02x}", buf[4]),
            None,
        )
    })?;
    let flags = buf[5];
    let payload = buf[6..total].to_vec();
    Ok(Some((
        Frame {
            msg_type,
            flags,
            payload,
        },
        total,
    )))
}

/// 由 X25519 共享密钥派生 ChaCha20-Poly1305 会话密钥
/// （salt = 双方指纹拼接的 SHA256，info = b"nexusforge-kvm-v1"）。
/// shared 通常为 64B 双 DH 拼接：静态配对 DH（鉴权）|| 临时 DH（保新鲜）。
pub fn derive_session_key(shared: &[u8], salt_material: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let salt: [u8; 32] = Sha256::digest(salt_material).into();
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut okm = [0u8; 32];
    hk.expand(b"nexusforge-kvm-v1", &mut okm)
        .expect("HKDF 扩展长度合法");
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
    pub fn seal(
        &mut self,
        plaintext: &[u8],
    ) -> Result<(chacha20poly1305::Nonce, Vec<u8>), AppError> {
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
            .map_err(|_| {
                AppError::module("KVM_SESSION_005", "帧解密失败（密钥或序号不匹配）", None)
            })
    }

    fn next_nonce(&mut self) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&self.counter.to_le_bytes());
        self.counter = self.counter.wrapping_add(1);
        nonce
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_codec_roundtrip_and_guards() {
        let f = Frame {
            msg_type: MsgType::Hello,
            flags: 3,
            payload: b"hi".to_vec(),
        };
        let wire = encode_frame(&f);
        let (d, consumed) = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(consumed, wire.len());
        assert_eq!(d.msg_type, MsgType::Hello);
        assert_eq!(d.flags, 3);
        assert_eq!(d.payload, b"hi");
        // 半帧 → Ok(None)
        assert!(decode_frame(&wire[..wire.len() - 1]).unwrap().is_none());
        // 未知消息类型 → Err（负例：0x0D 未登记）
        let mut bad = wire.clone();
        bad[4] = 0x0D;
        assert!(decode_frame(&bad).is_err());
        // 长度谎报（小于帧头）→ Err
        let mut lying = wire.clone();
        lying[..4].copy_from_slice(&1u32.to_be_bytes());
        assert!(decode_frame(&lying).is_err());
    }

    #[tokio::test]
    async fn read_write_frame_over_duplex() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let write = tokio::spawn(async move {
            write_frame(
                &mut b,
                &Frame {
                    msg_type: MsgType::Ping,
                    flags: 0,
                    payload: vec![42],
                },
            )
            .await
            .unwrap();
        });
        let f = read_frame(&mut a).await.unwrap();
        assert_eq!(f.msg_type, MsgType::Ping);
        assert_eq!(f.payload, vec![42]);
        write.await.unwrap();
    }

    fn nonce_arr(n: chacha20poly1305::Nonce) -> [u8; 12] {
        let mut a = [0u8; 12];
        a.copy_from_slice(&n);
        a
    }

    #[test]
    fn cipher_seal_open_roundtrip_and_counter_nonce() {
        let key = derive_session_key(&[7u8; 64], b"fpAfpB");
        let mut tx = FrameCipher::new(key);
        let mut rx = FrameCipher::new(key);
        let (n1, c1) = tx.seal(b"first").unwrap();
        let (n2, c2) = tx.seal(b"second").unwrap();
        let (n1, n2) = (nonce_arr(n1), nonce_arr(n2));
        assert_ne!(n1, n2, "nonce 必随计数器推进");
        // 负例：密文与 nonce 不匹配（重放/乱序）→ AEAD 认证失败
        assert!(rx.open(&n1, &c2).is_err());
        assert_eq!(rx.open(&n1, &c1).unwrap(), b"first");
        assert_eq!(rx.open(&n2, &c2).unwrap(), b"second");
        // 篡改密文 → 认证失败（负例）
        let (n3, mut c3) = tx.seal(b"third").unwrap();
        let n3 = nonce_arr(n3);
        let l = c3.len() - 1;
        c3[l] ^= 0xFF;
        assert!(rx.open(&n3, &c3).is_err());
    }

    #[test]
    fn derive_session_key_binds_salt_and_shared() {
        let k1 = derive_session_key(&[1u8; 64], b"AA||BB");
        let k2 = derive_session_key(&[1u8; 64], b"AA||CC");
        let k3 = derive_session_key(&[2u8; 64], b"AA||BB");
        assert_ne!(k1, k2, "salt 不同必异key");
        assert_ne!(k1, k3, "shared 不同必异key");
    }
}
