//! USN/MFT 全盘文件索引（docs/impl/05 F5）：FSCTL_ENUM_USN_DATA 驱动器枚举。
//!
//! - 需要管理员权限；无权限时 CreateFileW 拒绝访问 → AppError::Permission，
//!   file-core 侧自动降级为目录遍历（SearchResult.degraded = true）
//! - 记录布局（FSCTL_ENUM_USN_DATA，记录按 8 字节对齐）：
//!   { u64 FileReferenceNumber, u64 ParentFileReferenceNumber, u32 FileNameLength(字节), WCHAR FileName[] }
//! - v1：构建 FRN→(PFRN,name) 全量映射（5 分钟 TTL 缓存），命中后回溯父链拼绝对路径

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use host_core::error::AppError;
use host_core::ports::{FileHit, UsnIndexPort};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, ERROR_HANDLE_EOF};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{FSCTL_ENUM_USN_DATA, MFT_ENUM_DATA_V0};
use windows::Win32::System::IO::DeviceIoControl;

/// 映射缓存 TTL（实时性与 CPU 占用折中；变更感知交给未来 watcher 增量）
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
/// < 0x10 的根级条目是 $MFT 等元数据文件
const META_FRN_MAX: u64 = 0x10;
/// 输出缓冲 16MiB（单次往返覆盖数万条记录）
const OUT_BUF: usize = 16 * 1024 * 1024;

type FrnMap = HashMap<u64, (u64, String)>;

pub struct UsnIndex {
    cache: Mutex<Option<(Instant, String, Arc<FrnMap>)>>,
}

impl UsnIndex {
    pub fn new() -> Self {
        Self { cache: Mutex::new(None) }
    }
}

impl Default for UsnIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl UsnIndexPort for UsnIndex {
    fn search(&self, query: &str, limit: u32) -> Result<Vec<FileHit>, AppError> {
        // v1 只索引系统盘；多盘并行枚举列入后续里程碑
        let drive = "C:\\";
        let map = self.frn_map(drive)?;
        let q = query.to_lowercase();
        let mut hits: Vec<FileHit> = Vec::new();
        for (&frn, (pfrn, name)) in map.iter() {
            if hits.len() >= limit as usize {
                break;
            }
            let lower = name.to_lowercase();
            let score = if lower.starts_with(&q) {
                0.9
            } else if lower.contains(&q) {
                0.6
            } else {
                continue;
            };
            hits.push(FileHit {
                path: PathBuf::from(resolve_path(&map, frn, *pfrn, drive)),
                score,
            });
        }
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit as usize);
        Ok(hits)
    }
}

impl UsnIndex {
    fn frn_map(&self, drive: &str) -> Result<Arc<FrnMap>, AppError> {
        let mut guard = self.cache.lock().expect("USN 缓存锁");
        if let Some((at, cached_drive, map)) = guard.as_ref() {
            if *at + CACHE_TTL > Instant::now() && cached_drive == drive {
                return Ok(map.clone());
            }
        }
        let map = Arc::new(enumerate_mft(drive)?);
        *guard = Some((Instant::now(), drive.to_owned(), map.clone()));
        Ok(map)
    }
}

/// 打开卷句柄（需管理员；权限不足映射 Permission 错误）
fn open_volume(drive: &str) -> Result<HANDLE, AppError> {
    // "C:\" → "\\.\C:"
    let letter = drive.trim_end_matches(['\\', '/']).chars().next().unwrap_or('C');
    let path = format!(r"\\.\{letter}:");
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
        .map_err(|e| {
            if e.code().0 & 0xFFFF == 5 {
                // HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED)
                AppError::Permission {
                    code: "FILE_SEARCH_002".into(),
                    message: "USN 索引需要管理员权限".into(),
                    hint: "以管理员身份运行，或由系统自动降级为目录遍历".into(),
                }
            } else {
                AppError::Network { code: "FILE_SEARCH_003".into(), message: e.to_string(), retryable: false }
            }
        })
    }
}

/// FSCTL_ENUM_USN_DATA 全量枚举 → FRN 映射
fn enumerate_mft(drive: &str) -> Result<FrnMap, AppError> {
    let handle = open_volume(drive)?;
    let mut map: FrnMap = HashMap::new();
    let mut med = MFT_ENUM_DATA_V0 {
        StartFileReferenceNumber: 0,
        LowUsn: 0,
        HighUsn: i64::MAX,
    };
    let med_ptr = &mut med as *mut MFT_ENUM_DATA_V0 as *const u8;
    let med_len = std::mem::size_of::<MFT_ENUM_DATA_V0>() as u32;
    let med_bytes = unsafe { std::slice::from_raw_parts(med_ptr, med_len as usize) };

    let result = (|| -> Result<(), AppError> {
        let mut buffer = vec![0u8; OUT_BUF];
        loop {
            let mut returned = 0u32;
            if let Err(e) = unsafe {
                DeviceIoControl(
                    handle,
                    FSCTL_ENUM_USN_DATA,
                    Some(med_bytes.as_ptr() as *const _),
                    med_len,
                    Some(buffer.as_mut_ptr() as *mut _),
                    buffer.len() as u32,
                    Some(&mut returned),
                    None,
                )
            } {
                if e.code() == ERROR_HANDLE_EOF.to_hresult() {
                    break; // 枚举完成
                }
                return Err(AppError::module(
                    "FILE_SEARCH_004",
                    format!("FSCTL_ENUM_USN_DATA 失败: {e}"),
                    None,
                ));
            }
            if returned < 8 {
                break;
            }
            // 输出头部 u64 = 下一次枚举的起始 FileReferenceNumber（内核推进游标）
            let next_start =
                u64::from_le_bytes(buffer[..8].try_into().expect("USN 头部长度固定"));
            let chunk = &buffer[..returned as usize];
            let mut offset = 8usize;
            while offset + 20 <= chunk.len() {
                let frn = u64::from_le_bytes(chunk[offset..offset + 8].try_into().unwrap());
                let pfrn = u64::from_le_bytes(chunk[offset + 8..offset + 16].try_into().unwrap());
                let name_len =
                    u32::from_le_bytes(chunk[offset + 16..offset + 20].try_into().unwrap()) as usize;
                if name_len == 0 || offset + 20 + name_len > chunk.len() {
                    break; // 记录被缓冲边界截断（内核保证不会，防御性）
                }
                let name_utf16: Vec<u16> = chunk[offset + 20..offset + 20 + name_len]
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes(b.try_into().unwrap()))
                    .collect();
                let name = String::from_utf16_lossy(&name_utf16);
                offset += (20 + name_len + 7) & !7; // 8 字节对齐
                if frn >= META_FRN_MAX {
                    map.insert(frn, (pfrn, name));
                }
            }
            if next_start == 0 || next_start == med.StartFileReferenceNumber {
                break; // 游标不再前进，防死循环
            }
            med.StartFileReferenceNumber = next_start;
        }
        Ok(())
    })();
    unsafe {
        let _ = CloseHandle(handle);
    }
    result?;
    Ok(map)
}

/// 回溯父链拼绝对路径（最深 64 层防环）
fn resolve_path(map: &FrnMap, _frn: u64, pfrn: u64, drive: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let mut cur = pfrn;
    for _ in 0..64 {
        if cur < META_FRN_MAX {
            break; // 到根
        }
        match map.get(&cur) {
            Some((parent, name)) => {
                parts.push(name);
                cur = *parent;
            }
            None => break,
        }
    }
    parts.reverse();
    let mut path = String::from(drive.trim_end_matches('\\'));
    path.push('\\');
    path.push_str(&parts.join("\\"));
    path
}
