//! 性能采样（docs/impl/06 SY4）：PDH CPU/网络 + GlobalMemoryStatusEx 内存 + 各盘空间。
//!
//! 实现内部持有 PDH query 与 1s 采样线程，缓存最新快照——get_* 直接读值
//! （文档要求：WMI 太慢，用 PDH 1s 采样）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use host_core::error::AppError;
use host_core::ports::{DiskSpace, PerfPort};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetLogicalDrives};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

/// 最新快照缓存
#[derive(Clone, Debug, Default)]
struct Snapshot {
    cpu: f64,
    mem_used: u64,
    mem_total: u64,
    net_bps: f64,
    disks: Vec<DiskSpace>,
}

/// PDH 句柄为裸 isize（windows 0.58）
type PdhQuery = isize;
type PdhCounter = isize;

/// PDH 性能采样（实现 [`PerfPort`]；Drop 停线程 + 关 query）
pub struct PdhWin {
    snapshot: Arc<Mutex<Snapshot>>,
    cancel: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl PdhWin {
    /// 创建 query + 计数器并启动 1s 采样线程
    pub fn new() -> Result<Self, AppError> {
        let mut query: PdhQuery = 0;
        // PDH 函数返回 u32 错误码（ERROR_SUCCESS = 0）
        let r = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) };
        pdh(r, "PdhOpenQueryW")?;

        let cpu = add_english(query, r"\Processor(_Total)\% Processor Time")?;
        let net = add_english(query, r"\Network Interface(*)\Bytes Total/sec")?;

        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let snap_clone = snapshot.clone();
        let cancel_clone = cancel.clone();

        let handle = std::thread::Builder::new()
            .name("nf-perf-sample".into())
            .spawn(move || unsafe {
                // 预热一次 collect（CPU 计数器首次值为 0）
                PdhCollectQueryData(query);
                loop {
                    if cancel_clone.load(Ordering::SeqCst) {
                        PdhCloseQuery(query);
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                    if PdhCollectQueryData(query) != 0 {
                        continue;
                    }
                    let cpu = read_double(cpu).unwrap_or(0.0);
                    let net = read_array_sum(net).unwrap_or(0.0);
                    let (mem_used, mem_total) = read_memory();
                    let disks = read_disks();
                    if let Ok(mut s) = snap_clone.lock() {
                        s.cpu = cpu.clamp(0.0, 100.0);
                        s.net_bps = net.max(0.0);
                        s.mem_used = mem_used;
                        s.mem_total = mem_total;
                        s.disks = disks;
                    }
                }
            })
            .map_err(|e| AppError::module("SYS_PERF_001", format!("采样线程创建失败: {e}"), None))?;

        Ok(Self {
            snapshot,
            cancel,
            thread: Mutex::new(Some(handle)),
        })
    }
}

impl Drop for PdhWin {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Ok(mut t) = self.thread.lock() {
            if let Some(h) = t.take() {
                let _ = h.join();
            }
        }
    }
}

impl PerfPort for PdhWin {
    fn cpu_percent(&self) -> Result<f64, AppError> {
        self.snapshot
            .lock()
            .map(|s| s.cpu)
            .map_err(|_| AppError::module("SYS_PERF_003", "采样快照锁污染".to_string(), None))
    }

    fn mem_bytes(&self) -> Result<(u64, u64), AppError> {
        self.snapshot
            .lock()
            .map(|s| (s.mem_used, s.mem_total))
            .map_err(|_| AppError::module("SYS_PERF_003", "采样快照锁污染".to_string(), None))
    }

    fn disk_spaces(&self) -> Result<Vec<DiskSpace>, AppError> {
        self.snapshot
            .lock()
            .map(|s| s.disks.clone())
            .map_err(|_| AppError::module("SYS_PERF_003", "采样快照锁污染".to_string(), None))
    }

    fn net_bps(&self) -> Result<f64, AppError> {
        self.snapshot
            .lock()
            .map(|s| s.net_bps)
            .map_err(|_| AppError::module("SYS_PERF_003", "采样快照锁污染".to_string(), None))
    }
}

fn pdh(code: u32, what: &str) -> Result<(), AppError> {
    if code == 0 {
        Ok(())
    } else {
        Err(AppError::module("SYS_PERF_002", format!("{what} 失败（PDH 0x{code:08X}）"), None))
    }
}

fn add_english(query: PdhQuery, path: &str) -> Result<PdhCounter, AppError> {
    let mut wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut counter: PdhCounter = 0;
    let r = unsafe { PdhAddEnglishCounterW(query, PCWSTR::from_raw(wide.as_mut_ptr()), 0, &mut counter) };
    pdh(r, &format!("PdhAddEnglishCounterW({path})"))?;
    Ok(counter)
}

/// 读取单值 double 计数器
unsafe fn read_double(counter: PdhCounter) -> Option<f64> {
    let values = formatted_values(counter)?;
    values.into_iter().next()
}

/// 通配计数器：全部实例求和（网络总吞吐）
unsafe fn read_array_sum(counter: PdhCounter) -> Option<f64> {
    let values = formatted_values(counter)?;
    Some(values.into_iter().sum())
}

/// 两次调用取 PDH 格式化 double 值（PDH_FMT_COUNTERVALUE_ITEM_W 数组）
unsafe fn formatted_values(counter: PdhCounter) -> Option<Vec<f64>> {
    let mut size = 0u32;
    let mut count = 0u32;
    // 第一次调用探测所需大小（返回 PDH_MORE_DATA）
    let _ = PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let r = PdhGetFormattedCounterArrayW(
        counter,
        PDH_FMT_DOUBLE,
        &mut size,
        &mut count,
        Some(buf.as_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W),
    );
    if r != 0 {
        return None;
    }
    let items = std::slice::from_raw_parts(
        buf.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W,
        count as usize,
    );
    Some(items.iter().map(|item| item.FmtValue.Anonymous.doubleValue).collect())
}

/// 内存（GlobalMemoryStatusEx）
fn read_memory() -> (u64, u64) {
    let mut mem = MEMORYSTATUSEX::default();
    mem.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    unsafe {
        if GlobalMemoryStatusEx(&mut mem).is_ok() {
            (mem.ullTotalPhys - mem.ullAvailPhys, mem.ullTotalPhys)
        } else {
            (0, 0)
        }
    }
}

/// 各盘空间（GetLogicalDrives 位图 + GetDiskFreeSpaceExW）
fn read_disks() -> Vec<DiskSpace> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26 {
        if mask & (1u32 << i) != 0 {
            let letter = (b'A' + i) as char;
            let mount = format!("{letter}:\\");
            let wide: Vec<u16> = mount.encode_utf16().chain(std::iter::once(0)).collect();
            let mut free: u64 = 0;
            let mut total: u64 = 0;
            let mut _free_total: u64 = 0;
            let ok = unsafe {
                GetDiskFreeSpaceExW(
                    PCWSTR::from_raw(wide.as_ptr()),
                    Some(&mut _free_total),
                    Some(&mut total),
                    Some(&mut free),
                )
            };
            if ok.is_ok() && total > 0 {
                out.push(DiskSpace { mount, free, total });
            }
        }
    }
    out
}
