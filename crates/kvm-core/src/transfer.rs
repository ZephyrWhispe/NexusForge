//! K6 剪贴板/文件通道（docs/impl/05 K6）。
//!
//! 复用 K3 加密帧协议（msg_type 0x03/0x04/0x05/0x06）：
//! - **ClipData**：`host_core::ports::ClipContent` 直接 JSON 序列化（v1 支持
//!   Text/Image；Files 变体无跨机语义，由调用方逐文件走 [`send_file`]）。
//! - **文件**：FileMeta（JSON，transfer_id = 全文件 SHA256 hex）→ FileChunk
//!   （二进制头 `[32B sha256][u64 index BE]` + 原始数据）→ Ack（JSON）。
//!   4MB 块上限受 MAX_PAYLOAD 约束：密文帧 payload = 明文 + 12B nonce + 16B tag，
//!   故 CHUNK_SIZE 预留余量。
//! - **断点续传**：接收端按 (file_hash, chunk_index) 位图跟踪；同一哈希重传时
//!   已收块直接跳过（seek 覆写等价，位图去重避免重复落盘）。
//! - **终验**：全部块集齐后流式重算 SHA256 与 FileMeta比对，不符 Ack 失败。

use std::collections::{HashMap, HashSet};
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use host_core::error::AppError;
use host_core::ports::ClipContent;

use crate::session::{Frame, MsgType, SessionHandle, MAX_PAYLOAD};

/// 单块数据上限：4MB 预算减去加密开销（nonce 12B + tag 16B）与块头（40B）余量
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024 - 4096;
/// ClipData/JSON 载荷单帧预算（同上预留）
pub const CLIP_MAX: usize = MAX_PAYLOAD - 4096;
/// 发送进度回调节流周期
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

// ---------------------------------------------------------------------------
// 线格式载荷
// ---------------------------------------------------------------------------

/// FileMeta 载荷（JSON）。transfer_id = 全文件 SHA256 hex（同内容重传天然续传）
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FileMetaPayload {
    pub transfer_id: String,
    /// 文件名（不含路径；接收端重名自动加 "(n)" 后缀）
    pub name: String,
    pub size: u64,
    /// hex 编码的 SHA256
    pub sha256: String,
    pub chunk_size: u32,
    pub total_chunks: u64,
}

/// Ack 载荷（JSON）：文件或剪贴板传输的最终回执
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AckPayload {
    pub transfer_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// FileChunk 线格式头长：32B sha256 原始摘要 + 8B 索引
const CHUNK_HEADER: usize = 40;

/// 编码 FileChunk：`[32B sha256][u64 index BE][data]`
pub fn encode_chunk(sha: [u8; 32], index: u64, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHUNK_HEADER + data.len());
    out.extend_from_slice(&sha);
    out.extend_from_slice(&index.to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// 解码 FileChunk；返回 (sha256, 块索引, 数据切片)
pub fn decode_chunk(payload: &[u8]) -> Result<([u8; 32], u64, &[u8]), AppError> {
    if payload.len() < CHUNK_HEADER {
        return Err(AppError::module("KVM_TRANSFER_001", "FileChunk 头长度非法", None));
    }
    let mut sha = [0u8; 32];
    sha.copy_from_slice(&payload[..32]);
    let mut idx = [0u8; 8];
    idx.copy_from_slice(&payload[32..40]);
    Ok((sha, u64::from_be_bytes(idx), &payload[CHUNK_HEADER..]))
}

fn clip_frame(payload: Vec<u8>) -> Frame {
    Frame { msg_type: MsgType::ClipData, flags: 0, payload }
}

fn meta_frame(meta: &FileMetaPayload) -> Result<Frame, AppError> {
    Ok(Frame {
        msg_type: MsgType::FileMeta,
        flags: 0,
        payload: serde_json::to_vec(meta)
            .map_err(|e| AppError::module("KVM_TRANSFER_002", format!("FileMeta 序列化失败: {e}"), None))?,
    })
}

fn chunk_frame(sha: [u8; 32], index: u64, data: &[u8]) -> Frame {
    Frame { msg_type: MsgType::FileChunk, flags: 0, payload: encode_chunk(sha, index, data) }
}

pub(crate) fn ack_frame(ack: &AckPayload) -> Result<Frame, AppError> {
    Ok(Frame {
        msg_type: MsgType::Ack,
        flags: 0,
        payload: serde_json::to_vec(ack)
            .map_err(|e| AppError::module("KVM_TRANSFER_003", format!("Ack 序列化失败: {e}"), None))?,
    })
}

// ---------------------------------------------------------------------------
// 发送端
// ---------------------------------------------------------------------------

/// 发送进度快照（节流 200ms + 收尾必调一次）
#[derive(Clone, Debug, Serialize)]
pub struct FileProgress {
    pub transfer_id: String,
    pub sent_chunks: u64,
    pub total_chunks: u64,
}

/// 发送剪贴板内容（单帧；超限报错——超大图片应走文件通道）
pub async fn send_clip(handle: &SessionHandle, content: &ClipContent) -> Result<(), AppError> {
    let payload = serde_json::to_vec(content)
        .map_err(|e| AppError::module("KVM_TRANSFER_005", format!("ClipContent 序列化失败: {e}"), None))?;
    if payload.len() > CLIP_MAX {
        return Err(AppError::module(
            "KVM_TRANSFER_006",
            format!("剪贴板内容 {} 字节超单帧预算 {CLIP_MAX}，请走文件通道", payload.len()),
            None,
        ));
    }
    handle.send(clip_frame(payload)).await
}

/// 发送本地文件：流式计算 SHA256 → FileMeta → 逐块顺序发送（TCP 有序可靠，
/// 不做逐块 Ack；失败由调用方整文件重发，接收端按位图续传）。
/// 返回 transfer_id（= SHA256 hex）。
pub async fn send_file(
    handle: &SessionHandle,
    path: impl AsRef<Path>,
    progress: Option<Box<dyn Fn(FileProgress) + Send + Sync>>,
) -> Result<String, AppError> {
    let path = path.as_ref();
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::module("KVM_TRANSFER_007", "文件路径无有效文件名", None))?
        .to_string();

    // ① 流式计算 SHA256 + 大小
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| AppError::module("KVM_TRANSFER_008", format!("打开文件失败: {e}"), None))?;
    let size = file
        .metadata()
        .await
        .map_err(|e| AppError::module("KVM_TRANSFER_008", format!("读取文件元数据失败: {e}"), None))?
        .len();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = file
            .read(&mut buf)
            .await
            .map_err(|e| AppError::module("KVM_TRANSFER_008", format!("读取文件失败: {e}"), None))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let sha: [u8; 32] = hasher.finalize().into();
    let sha_hex = hex_str(&sha);

    let total_chunks = size.div_ceil(CHUNK_SIZE as u64);
    let meta = FileMetaPayload {
        transfer_id: sha_hex.clone(),
        name: file_name,
        size,
        sha256: sha_hex.clone(),
        chunk_size: CHUNK_SIZE as u32,
        total_chunks,
    };
    handle.send(meta_frame(&meta)?).await?;

    // 首次进度立即上报（调用方由此获得 transfer_id，无需等待首块）
    if let Some(cb) = progress.as_ref() {
        cb(FileProgress { transfer_id: sha_hex.clone(), sent_chunks: 0, total_chunks });
    }

    // ② 逐块顺序发送（复用句柄上的有序队列；每块 await 保证背压）
    if size > 0 {
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| AppError::module("KVM_TRANSFER_008", format!("打开文件失败: {e}"), None))?;
        let mut last_report = Instant::now();
        for index in 0..total_chunks {
            // 非末块必须读满：AsyncReadExt::read 允许部分读取，短块会让接收端
            // 按 index*chunk_size 定位出现零洞，SHA256 终验必然不符（K9 环回实测踩坑）
            let is_last = index + 1 == total_chunks;
            let n = if is_last {
                file.read(&mut buf)
                    .await
                    .map_err(|e| AppError::module("KVM_TRANSFER_008", format!("读取文件失败: {e}"), None))?
            } else {
                file.read_exact(&mut buf)
                    .await
                    .map_err(|e| AppError::module("KVM_TRANSFER_008", format!("读取文件失败: {e}"), None))?;
                buf.len()
            };
            if n == 0 {
                return Err(AppError::module("KVM_TRANSFER_009", "文件在发送中途被截断", None));
            }
            handle.send(chunk_frame(sha, index, &buf[..n])).await?;
            if let Some(cb) = progress.as_ref() {
                let due = index + 1 == total_chunks
                    || last_report.elapsed() >= PROGRESS_INTERVAL;
                if due {
                    last_report = Instant::now();
                    cb(FileProgress { transfer_id: sha_hex.clone(), sent_chunks: index + 1, total_chunks });
                }
            }
        }
    }
    Ok(sha_hex)
}

/// 字节 → 小写 hex（transfer_id 场景）
pub fn hex_str(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ---------------------------------------------------------------------------
// 接收端：TransferManager
// ---------------------------------------------------------------------------

/// FileMeta 处理结果
#[derive(Clone, Debug)]
pub enum MetaOutcome {
    /// 已就绪待收块（位图可能非空：同哈希重传即续传）
    Accepted { received: u64, total_chunks: u64 },
    /// 空文件直接完成
    Completed { final_path: PathBuf },
}

/// FileChunk 处理结果
#[derive(Clone, Debug)]
pub enum ChunkOutcome {
    InProgress { received: u64, total_chunks: u64 },
    Completed { final_path: PathBuf },
}

struct Incoming {
    meta: FileMetaPayload,
    file: std::fs::File,
    received: HashSet<u64>,
}

/// 接收端状态机：FileMeta 建档（预分配 + 位图）→ FileChunk seek 写入 →
/// 集齐后 SHA256 终验 → 改名到最终路径。全部同步 fs：调用方须置于
/// spawn_blocking（模块层在事件任务内已做拆分）。
pub struct TransferManager {
    /// 接收根目录（part 与成品文件都在此）
    root: PathBuf,
    transfers: Mutex<HashMap<String, Incoming>>,
}

impl TransferManager {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), transfers: Mutex::new(HashMap::new()) }
    }

    pub fn incoming_root(&self) -> &Path {
        &self.root
    }

    fn part_path(&self, transfer_id: &str) -> PathBuf {
        self.root.join(format!("{transfer_id}.part"))
    }

    /// FileMeta：建档 / 续传重入 / 空文件直通
    pub fn on_meta(&self, meta: FileMetaPayload) -> Result<MetaOutcome, AppError> {
        if meta.sha256 != meta.transfer_id {
            return Err(AppError::module("KVM_TRANSFER_010", "transfer_id 与 sha256 不一致", None));
        }
        std::fs::create_dir_all(&self.root)
            .map_err(|e| AppError::module("KVM_TRANSFER_011", format!("创建接收目录失败: {e}"), None))?;

        if meta.size == 0 {
            let final_path = self.claim_final_path(&meta.name)?;
            std::fs::File::create(&final_path)
                .map_err(|e| AppError::module("KVM_TRANSFER_011", format!("创建空文件失败: {e}"), None))?;
            return Ok(MetaOutcome::Completed { final_path });
        }

        let mut transfers = self.transfers.lock().expect("传输表锁");
        if let Some(incoming) = transfers.get(&meta.transfer_id) {
            // 重入：同哈希续传，仅更新位图快照返回
            let received = incoming.received.len() as u64;
            return Ok(MetaOutcome::Accepted { received, total_chunks: incoming.meta.total_chunks });
        }
        // 预分配：create 不截断（保留既有部分数据），set_len 对齐到声明大小
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(self.part_path(&meta.transfer_id))
            .map_err(|e| AppError::module("KVM_TRANSFER_011", format!("创建 part 文件失败: {e}"), None))?;
        file.set_len(meta.size)
            .map_err(|e| AppError::module("KVM_TRANSFER_011", format!("预分配文件失败: {e}"), None))?;
        let total_chunks = meta.total_chunks;
        transfers.insert(meta.transfer_id.clone(), Incoming { meta, file, received: HashSet::new() });
        Ok(MetaOutcome::Accepted { received: 0, total_chunks })
    }

    /// FileChunk：seek 写入 + 位图；集齐即终验改名。
    /// 锁内仅做位图与落盘，SHA256 终验（慢）在锁外执行。
    pub fn on_chunk(&self, payload: &[u8]) -> Result<ChunkOutcome, AppError> {
        let (sha, index, data) = decode_chunk(payload)?;
        let transfer_id = hex_str(&sha);
        let done = {
            let mut transfers = self.transfers.lock().expect("传输表锁");
            let incoming = transfers
                .get_mut(&transfer_id)
                .ok_or_else(|| AppError::module("KVM_TRANSFER_012", "收到未建档的 FileChunk", None))?;
            if !incoming.received.insert(index) {
                // 重复块（续传重发）：已落盘，跳过写入
            } else {
                if data.len() > incoming.meta.chunk_size as usize {
                    return Err(AppError::module("KVM_TRANSFER_013", "块数据超过声明 chunk_size", None));
                }
                let offset = index * incoming.meta.chunk_size as u64;
                use std::io::{Seek, Write};
                let f = &mut incoming.file;
                f.seek(SeekFrom::Start(offset))
                    .and_then(|_| f.write_all(data))
                    .and_then(|_| f.flush())
                    .map_err(|e| AppError::module("KVM_TRANSFER_014", format!("写入块失败: {e}"), None))?;
            }
            incoming.received.len() as u64 == incoming.meta.total_chunks
        };
        if !done {
            let (received, total) = {
                let transfers = self.transfers.lock().expect("传输表锁");
                match transfers.get(&transfer_id) {
                    Some(i) => (i.received.len() as u64, i.meta.total_chunks),
                    None => return self.finalize(&transfer_id).map(|final_path| ChunkOutcome::Completed { final_path }),
                }
            };
            return Ok(ChunkOutcome::InProgress { received, total_chunks: total });
        }
        self.finalize(&transfer_id).map(|final_path| ChunkOutcome::Completed { final_path })
    }

    /// 终验 + 出表 + 改名（锁外慢操作；part 清理在终验失败时同步执行）
    fn finalize(&self, transfer_id: &str) -> Result<PathBuf, AppError> {
        let incoming = self
            .transfers
            .lock()
            .expect("传输表锁")
            .remove(transfer_id)
            .ok_or_else(|| AppError::module("KVM_TRANSFER_012", "终验时传输条目缺失", None))?;
        let part = self.part_path(transfer_id);
        let mut file = std::fs::File::open(&part)
            .map_err(|e| AppError::module("KVM_TRANSFER_015", format!("打开 part 失败: {e}"), None))?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher)
            .map_err(|e| AppError::module("KVM_TRANSFER_015", format!("终验读取失败: {e}"), None))?;
        let actual = hex_str(&hasher.finalize());
        if actual != transfer_id {
            let _ = std::fs::remove_file(&part);
            return Err(AppError::module(
                "KVM_TRANSFER_016",
                format!("SHA256 终验不符：期望 {transfer_id}，实际 {actual}"),
                None,
            ));
        }
        let final_path = self.claim_final_path(&incoming.meta.name)?;
        std::fs::rename(&part, &final_path)
            .map_err(|e| AppError::module("KVM_TRANSFER_017", format!("改名失败: {e}"), None))?;
        tracing::info!(transfer_id, path = %final_path.display(), "文件接收完成（SHA256 校验通过）");
        Ok(final_path)
    }

    /// 成品文件命名：重名自动加 " (n)" 后缀（name (1).txt）
    fn claim_final_path(&self, name: &str) -> Result<PathBuf, AppError> {
        // 文件名防路径穿越：仅取最后一段
        let safe_name = Path::new(name)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| AppError::module("KVM_TRANSFER_018", "文件名非法", None))?;
        let candidate = self.root.join(safe_name);
        if !candidate.exists() {
            return Ok(candidate);
        }
        let stem = Path::new(safe_name).file_stem().and_then(|s| s.to_str()).unwrap_or(safe_name);
        let ext = Path::new(safe_name).extension().and_then(|e| e.to_str());
        for n in 1..1000u32 {
            let dotted = match ext {
                Some(e) => format!("{stem} ({n}).{e}"),
                None => format!("{stem} ({n})"),
            };
            let p = self.root.join(dotted);
            if !p.exists() {
                return Ok(p);
            }
        }
        Err(AppError::module("KVM_TRANSFER_018", "重名文件过多，无法命名", None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kvm-transfer-{tag}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sha256_hex(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        hex_str(&h.finalize())
    }

    #[test]
    fn chunk_codec_roundtrip() {
        let sha = [7u8; 32];
        let data = vec![1u8; 100];
        let wire = encode_chunk(sha, 42, &data);
        assert_eq!(wire.len(), CHUNK_HEADER + 100);
        let (sha2, idx, payload) = decode_chunk(&wire).unwrap();
        assert_eq!(sha2, sha);
        assert_eq!(idx, 42);
        assert_eq!(payload, &data[..]);
        assert!(decode_chunk(&[0u8; 10]).is_err());
    }

    #[test]
    fn receive_file_completes_with_sha256_verification() {
        let root = temp_root("ok");
        let mgr = TransferManager::new(&root);
        // 数据量跨 3 块（CHUNK_SIZE 过大，此处用小 chunk_size 的 meta 手工构造）
        let body: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let sha_hex = sha256_hex(&body);
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&hex_to_bytes(&sha_hex));

        let total_chunks = body.len().div_ceil(300) as u64;
        let meta = FileMetaPayload {
            transfer_id: sha_hex.clone(),
            name: "hello.bin".into(),
            size: body.len() as u64,
            sha256: sha_hex.clone(),
            chunk_size: 300,
            total_chunks,
        };
        assert!(matches!(mgr.on_meta(meta.clone()).unwrap(), MetaOutcome::Accepted { received: 0, .. }));

        for (i, chunk) in body.chunks(300).enumerate() {
            let out = mgr.on_chunk(&encode_chunk(sha, i as u64, chunk)).unwrap();
            let done = i + 1 == total_chunks as usize;
            assert_eq!(done, matches!(out, ChunkOutcome::Completed { .. }), "块 {i} 结果: {out:?}");
        }
        let final_path = root.join("hello.bin");
        assert!(final_path.exists());
        assert_eq!(std::fs::read(&final_path).unwrap(), body);
        // part 文件已改名消失
        assert!(!mgr.part_path(&sha_hex).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupted_file_fails_verification() {
        let root = temp_root("bad");
        let mgr = TransferManager::new(&root);
        let body = vec![9u8; 700];
        let sha_hex = sha256_hex(&body);
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&hex_to_bytes(&sha_hex));
        let meta = FileMetaPayload {
            transfer_id: sha_hex.clone(),
            name: "bad.bin".into(),
            size: 700,
            sha256: sha_hex.clone(),
            chunk_size: 300,
            total_chunks: 3,
        };
        mgr.on_meta(meta).unwrap();
        // 块数据被篡改（内容与 sha 不符）但全部送达
        for (i, chunk) in body.chunks(300).enumerate() {
            let mut c = chunk.to_vec();
            if i == 1 {
                c[0] ^= 0xFF;
            }
            let r = mgr.on_chunk(&encode_chunk(sha, i as u64, &c));
            if i + 1 == 3 {
                assert!(r.is_err(), "终验必须失败");
            }
        }
        // part 已清理
        assert!(!mgr.part_path(&sha_hex).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_chunks_resume_and_empty_file() {
        let root = temp_root("resume");
        let mgr = TransferManager::new(&root);
        let body = vec![5u8; 500];
        let sha_hex = sha256_hex(&body);
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&hex_to_bytes(&sha_hex));
        let meta = FileMetaPayload {
            transfer_id: sha_hex.clone(),
            name: "resume.bin".into(),
            size: 500,
            sha256: sha_hex,
            chunk_size: 200,
            total_chunks: 3,
        };
        mgr.on_meta(meta.clone()).unwrap();
        // 收 2 块后中断
        mgr.on_chunk(&encode_chunk(sha, 0, &body[..200])).unwrap();
        mgr.on_chunk(&encode_chunk(sha, 1, &body[200..400])).unwrap();
        // 重传 FileMeta（同哈希）→ 位图保留
        match mgr.on_meta(meta).unwrap() {
            MetaOutcome::Accepted { received, total_chunks } => {
                assert_eq!((received, total_chunks), (2, 3));
            }
            other => panic!("期望 Accepted，实际 {other:?}"),
        }
        // 重发块 0（应被位图跳过）+ 补最后一块 → 完成
        mgr.on_chunk(&encode_chunk(sha, 0, &body[..200])).unwrap();
        let out = mgr.on_chunk(&encode_chunk(sha, 2, &body[400..])).unwrap();
        assert!(matches!(out, ChunkOutcome::Completed { .. }));
        assert_eq!(std::fs::read(root.join("resume.bin")).unwrap(), body);

        // 空文件直通
        let empty_sha = sha256_hex(&[]);
        match mgr
            .on_meta(FileMetaPayload {
                transfer_id: empty_sha.clone(),
                name: "empty.txt".into(),
                size: 0,
                sha256: empty_sha,
                chunk_size: 200,
                total_chunks: 0,
            })
            .unwrap()
        {
            MetaOutcome::Completed { final_path } => {
                assert!(final_path.ends_with("empty.txt"));
            }
            other => panic!("空文件应直通完成，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn final_path_collision_gets_numeric_suffix() {
        let root = temp_root("collide");
        std::fs::write(root.join("doc.txt"), b"old").unwrap();
        let mgr = TransferManager::new(&root);
        let p1 = mgr.claim_final_path("doc.txt").unwrap();
        assert!(p1.to_str().unwrap().ends_with("doc (1).txt"), "实际 {p1:?}");
        std::fs::write(&p1, b"").unwrap();
        let p2 = mgr.claim_final_path("doc.txt").unwrap();
        assert!(p2.to_str().unwrap().ends_with("doc (2).txt"));
        // 路径穿越防护
        let p3 = mgr.claim_final_path("..\\evil.txt").unwrap();
        assert!(p3.parent().unwrap() == root, "路径穿越被钳制: {p3:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn hex_to_bytes(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    // ---- K6 端到端：真实加密会话上的文件传输环回 ----

    use crate::pairing::{PairCodeManager, PairStore, PairingService};
    use crate::session::{MsgType, SessionEvent, SessionManager};
    use crate::identity::DeviceIdentity;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::mpsc;

    fn temp_dir_tag(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kvm-e2e-{tag}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A（发送端，客户端角色）→ B（接收端，服务端角色）
    #[tokio::test(flavor = "multi_thread")]
    async fn file_transfer_roundtrip_over_session() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let dir_a = temp_dir_tag("a");
        let dir_b = temp_dir_tag("b");
        let id_a = Arc::new(DeviceIdentity::generate("PC-A".into()));
        let id_b = Arc::new(DeviceIdentity::generate("PC-B".into()));
        let store_a = Arc::new(PairStore::load_or_default(&dir_a).unwrap());
        let store_b = Arc::new(PairStore::load_or_default(&dir_b).unwrap());
        // 双向 seed 白名单（配对流程已由 pairing 测试覆盖）
        store_a
            .upsert(crate::pairing::PairedPeer {
                device_id: id_b.device_id.clone(),
                device_name: id_b.device_name.clone(),
                fingerprint: id_b.pubkey_fingerprint.clone(),
                pubkey_b64: crate::pairing::b64_encode(&id_b.public_key()),
                paired_at: 0,
            })
            .unwrap();
        store_b
            .upsert(crate::pairing::PairedPeer {
                device_id: id_a.device_id.clone(),
                device_name: id_a.device_name.clone(),
                fingerprint: id_a.pubkey_fingerprint.clone(),
                pubkey_b64: crate::pairing::b64_encode(&id_a.public_key()),
                paired_at: 0,
            })
            .unwrap();

        let mgr_b = SessionManager::new(
            id_b.clone(),
            PairingService::new(id_b.clone(), Arc::new(PairCodeManager::new()), store_b.clone()),
            store_b.clone(),
        );
        let (ev_tx_b, mut ev_rx_b) = mpsc::unbounded_channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _serve = mgr_b.clone().serve(listener, ev_tx_b, Arc::new(StdMutex::new(None))).await;

        let mgr_a = SessionManager::new(
            id_a.clone(),
            PairingService::new(id_a.clone(), Arc::new(PairCodeManager::new()), store_a.clone()),
            store_a.clone(),
        );
        let (ev_tx_a, mut ev_rx_a) = mpsc::unbounded_channel();
        let handle_a = mgr_a.connect(addr, ev_tx_a).await.unwrap();

        // 接收端（B 侧）模拟模块事件循环：Frame → TransferManager → 完成后回 Ack
        let recv_root = dir_b.join("incoming");
        let recv_mgr = Arc::new(TransferManager::new(&recv_root));
        let peer_id = id_a.device_id.clone();
        let mgr_b_for_ack = mgr_b.clone();
        let recv_task = tokio::spawn(async move {
            while let Some(ev) = ev_rx_b.recv().await {
                match ev {
                    SessionEvent::Frame { frame, .. } => match frame.msg_type {
                        MsgType::FileMeta => {
                            let meta: FileMetaPayload = serde_json::from_slice(&frame.payload).unwrap();
                            match recv_mgr.on_meta(meta).unwrap() {
                                MetaOutcome::Accepted { .. } => {}
                                MetaOutcome::Completed { final_path } => return Ok::<_, String>(final_path),
                            }
                        }
                        MsgType::FileChunk => match recv_mgr.on_chunk(&frame.payload).unwrap() {
                            ChunkOutcome::InProgress { .. } => {}
                            ChunkOutcome::Completed { final_path } => {
                                // 回 Ack（B 侧句柄从服务端注册表取）；transfer_id 重算 = 最终文件 sha256
                                if let Some(h) = mgr_b_for_ack.get_handle(&peer_id) {
                                    let data = std::fs::read(&final_path).unwrap();
                                    let mut h2 = Sha256::new();
                                    h2.update(&data);
                                    let ack = AckPayload {
                                        transfer_id: hex_str(h2.finalize().as_slice()),
                                        ok: true,
                                        error: None,
                                    };
                                    let _ = h.send(ack_frame(&ack).unwrap()).await;
                                }
                                return Ok(final_path);
                            }
                        },
                        _ => {}
                    },
                    SessionEvent::Established { .. } => {}
                    SessionEvent::Closed { .. } => return Err("closed".into()),
                }
            }
            Err("event stream ended".into())
        });

        // 发送端写源文件并发送
        let src = dir_a.join("payload.bin");
        let body: Vec<u8> = (0..700_000u32).map(|i| (i * 7 % 256) as u8).collect();
        std::fs::write(&src, &body).unwrap();

        let transfer_id = send_file(&handle_a, &src, Some(Box::new(|_p| {}))).await.unwrap();

        // 接收完成 + 内容逐字节一致
        let final_path = tokio::time::timeout(std::time::Duration::from_secs(15), recv_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read(&final_path).unwrap(), body);
        assert_eq!(transfer_id, sha256_hex(&body));

        // B 侧回 Ack 到达 A 侧事件流（排空 Established 等前置事件）。
        // 单次 recv 超时继续等总 deadline（全量并发测试负载下事件可能延迟到达）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut ack: Option<AckPayload> = None;
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_secs(1), ev_rx_a.recv()).await {
                Ok(Some(ev)) => match &ev {
                    SessionEvent::Frame { frame, .. } if frame.msg_type == MsgType::Ack => {
                        ack = Some(serde_json::from_slice(&frame.payload).unwrap());
                        break;
                    }
                    other => eprintln!("[diag-ack] other event: {other:?}"),
                },
                Ok(None) => {
                    eprintln!("[diag-ack] event stream closed (all senders dropped)");
                    break;
                }
                Err(_) => continue, // 单次超时：继续等
            }
        }
        let ack = ack.expect("Ack 应到达发送端");
        assert!(ack.ok);
        assert_eq!(ack.transfer_id, transfer_id);

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }
}
