//! SY4 资源监控（docs/impl/06 SY4）：1s 采样，环形缓冲 300 点，事件节流 1s 推 UI。
//!
//! 采样值来自 PerfPort（win-integration PDH 实现，WMI 太慢已规避）；
//! 清理扫描/包管理均不在此处（见 clean.rs / pkg.rs）。

use std::collections::VecDeque;
use std::sync::Mutex;

use host_core::ports::PerfPort;
use serde::{Deserialize, Serialize};

/// 环形缓冲容量（docs/impl/06 SY4：300 点）
pub const BUFFER_CAP: usize = 300;

/// 单个采样点（IPC DTO）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricsPoint {
    pub ts_ms: i64,
    /// CPU 使用率 0-100
    pub cpu: f64,
    pub mem_used: u64,
    pub mem_total: u64,
    /// 网络总吞吐字节/秒
    pub net_bps: f64,
    /// (mount, used, total)
    pub disks: Vec<DiskPoint>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiskPoint {
    pub mount: String,
    pub used: u64,
    pub total: u64,
}

/// 采样环形缓冲
pub struct MetricsBuffer {
    buf: Mutex<VecDeque<MetricsPoint>>,
}

impl MetricsBuffer {
    pub fn new() -> Self {
        Self { buf: Mutex::new(VecDeque::with_capacity(BUFFER_CAP)) }
    }

    /// 一次采样（失败静默跳过——PDH 个别计数器不可用时不应打断监控）
    pub fn sample(&self, perf: &dyn PerfPort, now_ms: i64) -> Option<MetricsPoint> {
        let cpu = perf.cpu_percent().ok()?;
        let (mem_used, mem_total) = perf.mem_bytes().unwrap_or((0, 0));
        let net_bps = perf.net_bps().unwrap_or(0.0);
        let disks = perf
            .disk_spaces()
            .unwrap_or_default()
            .into_iter()
            .map(|d| DiskPoint {
                mount: d.mount,
                used: d.total.saturating_sub(d.free),
                total: d.total,
            })
            .collect();
        let point = MetricsPoint { ts_ms: now_ms, cpu, mem_used, mem_total, net_bps, disks };
        let mut buf = self.buf.lock().expect("metrics 锁污染");
        if buf.len() >= BUFFER_CAP {
            buf.pop_front();
        }
        buf.push_back(point.clone());
        Some(point)
    }

    /// 全部历史（旧 → 新）
    pub fn history(&self) -> Vec<MetricsPoint> {
        self.buf.lock().expect("metrics 锁污染").iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.buf.lock().expect("metrics 锁污染").len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for MetricsBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::error::AppError;
    use host_core::ports::DiskSpace;
    type R<T> = std::result::Result<T, AppError>;

    /// 假 PerfPort：cpu 每次调用递增 10
    struct FakePerf {
        calls: std::sync::atomic::AtomicU32,
    }
    impl FakePerf {
        fn new() -> Self {
            Self { calls: std::sync::atomic::AtomicU32::new(0) }
        }
    }
    impl PerfPort for FakePerf {
        fn cpu_percent(&self) -> R<f64> {
            let c = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(f64::from((c * 10) % 110) % 100.0)
        }
        fn mem_bytes(&self) -> R<(u64, u64)> {
            Ok((4_000_000_000, 16_000_000_000))
        }
        fn disk_spaces(&self) -> R<Vec<DiskSpace>> {
            Ok(vec![DiskSpace { mount: "C:\\".into(), free: 10, total: 100 }])
        }
        fn net_bps(&self) -> R<f64> {
            Ok(1024.0)
        }
    }

    #[test]
    fn ring_buffer_capacity_300() {
        let buf = MetricsBuffer::new();
        let perf = FakePerf::new();
        for i in 0..305 {
            buf.sample(&perf, i as i64);
        }
        assert_eq!(buf.len(), BUFFER_CAP);
        let h = buf.history();
        assert_eq!(h.first().unwrap().ts_ms, 5); // 前 5 个被挤出
        assert_eq!(h.last().unwrap().ts_ms, 304);
        assert_eq!(h.last().unwrap().net_bps, 1024.0);
        assert_eq!(h.last().unwrap().disks[0].used, 90);
    }
}
