//! 进程枚举/终结（T-B7-10）：实现 host-core [`ProcPort`]。
//!
//! 全仓唯一 windows API 层（DESIGN O2）。形制沿 maintenance.rs 的 psapi 用法：
//! EnumProcesses → 逐 pid OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)：
//! - cpu_ms：GetProcessTimes 内核+用户时间合计（累计量，差值由 sys-core 两拍算）
//! - mem_bytes：K32GetProcessMemoryInfo 工作集（读不到记 0——DTO 必填面）
//! - io_bytes：GetProcessIoCounters 读+写合计；**跨权限读不到即 None**（不编 0）
//! - name：QueryFullProcessImageNameW 取文件名；pid 0/4 为无句柄内核假进程，给专名
//!
//! kill：OpenProcess(PROCESS_TERMINATE) + TerminateProcess(1)。调用方（sys-core
//! ProcessTable）必须已越过保护名单 + 复述名双闸才可到达本端口。

use std::path::Path;

use host_core::error::AppError;
use host_core::ports::{ProcPort, ProcSnap};
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::ProcessStatus::{
    EnumProcesses, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows::Win32::System::Threading::{
    GetProcessIoCounters, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
    TerminateProcess, IO_COUNTERS, PROCESS_ACCESS_RIGHTS, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

/// 本进程快照（ProcPort 实现；无状态，每次调用即取）
pub struct WinProc;

/// FILETIME（100ns 刻度）→ 毫秒累计
fn ft_ms(ft: &FILETIME) -> u64 {
    let ticks = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
    ticks / 10_000
}

fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Option<HANDLE> {
    // SAFETY：合法 pid + 最小权限位；句柄由调用方 CloseHandle
    unsafe { OpenProcess(access, false, pid).ok() }
}

/// QueryFullProcessImageNameW → 可执行文件名（OS 原样大小写，归一化交上层）
fn image_name(handle: HANDLE) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY：定长缓冲 + 长度配对传入
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .is_ok();
    if !ok || len == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

impl ProcPort for WinProc {
    fn snapshot(&self) -> Result<Vec<ProcSnap>, AppError> {
        let mut pids = vec![0u32; 4096];
        let mut needed = 0u32;
        // 缓冲不足时按 lpcbneeded 扩容重取一次（远超实际并发即断）
        for _ in 0..2 {
            // SAFETY：固定容量缓冲 + 字节数配对传入
            unsafe { EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut needed) }
                .map_err(|e| {
                    AppError::module(
                        "WIN_PROC_001",
                        "进程枚举失败",
                        Some(&e.message().to_string()),
                    )
                })?;
            let count = (needed as usize).div_ceil(4);
            if count > pids.len() {
                pids.resize(count, 0);
                continue;
            }
            break;
        }
        let count = ((needed as usize) / 4).min(pids.len());
        let mut out = Vec::with_capacity(count);
        for &pid in &pids[..count] {
            if pid == 0 {
                out.push(ProcSnap {
                    pid,
                    name: "System Idle Process".into(),
                    cpu_ms: 0,
                    mem_bytes: 0,
                    io_bytes: None,
                });
                continue;
            }
            if pid == 4 {
                // 内核进程无常规句柄；受保护判定走 pid 不依赖此名
                out.push(ProcSnap {
                    pid,
                    name: "System".into(),
                    cpu_ms: 0,
                    mem_bytes: 0,
                    io_bytes: None,
                });
                continue;
            }
            let Some(h) = open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
                continue; // 受保护/已退出：本轮不可见
            };
            let (mut creation, mut exit, mut kernel, mut user) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            // SAFETY：合法句柄 + 四个定长出参
            let cpu_ms = unsafe {
                GetProcessTimes(h, &mut creation, &mut exit, &mut kernel, &mut user)
                    .map(|()| ft_ms(&kernel) + ft_ms(&user))
                    .unwrap_or(0)
            };
            let mut io = IO_COUNTERS::default();
            // SAFETY：合法句柄 + POD 出参；读不到即 None（不编 0）
            let io_bytes = unsafe { GetProcessIoCounters(h, &mut io) }
                .ok()
                .map(|()| io.ReadTransferCount + io.WriteTransferCount);
            let mut mem = PROCESS_MEMORY_COUNTERS {
                cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
                ..Default::default()
            };
            // SAFETY：cb 已填真实大小；此 API 为 BOOL 形返回（非 Result）以 as_bool 判
            let mem_ok = unsafe { K32GetProcessMemoryInfo(h, &mut mem, mem.cb).as_bool() };
            let mem_bytes = if mem_ok { mem.WorkingSetSize as u64 } else { 0 };
            let name = image_name(h).unwrap_or_else(|| format!("pid-{pid}"));
            // SAFETY：与本函数内 OpenProcess 配对
            unsafe {
                let _ = CloseHandle(h);
            }
            out.push(ProcSnap {
                pid,
                name,
                cpu_ms,
                mem_bytes,
                io_bytes,
            });
        }
        Ok(out)
    }

    fn name_of(&self, pid: u32) -> Result<String, AppError> {
        // pid 0/4 无句柄可开，给内核专名（与 snapshot 同谱）
        match pid {
            0 => return Ok("System Idle Process".into()),
            4 => return Ok("System".into()),
            _ => {}
        }
        let h = open(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok_or_else(|| {
            AppError::module("WIN_PROC_002", format!("进程 {pid} 不存在或不可读名"), None)
        })?;
        let name = image_name(h).ok_or_else(|| {
            AppError::module("WIN_PROC_003", format!("进程 {pid} 镜像名读取失败"), None)
        });
        // SAFETY：与上方 OpenProcess 配对
        unsafe {
            let _ = CloseHandle(h);
        }
        name
    }

    fn kill(&self, pid: u32) -> Result<(), AppError> {
        // 红线闸（保护名单 + 复述名）在 sys-core；到达此处即已授权
        let h = open(pid, PROCESS_TERMINATE).ok_or_else(|| {
            AppError::module(
                "WIN_PROC_004",
                format!("OpenProcess({pid}) 失败（权限不足或进程已退出）"),
                None,
            )
        })?;
        // SAFETY：合法进程句柄；退出码 1 惯用；成对关闭
        let r = unsafe { TerminateProcess(h, 1) };
        unsafe {
            let _ = CloseHandle(h);
        }
        r.map_err(|e| {
            AppError::module(
                "WIN_PROC_005",
                format!("TerminateProcess({pid}) 被拒"),
                Some(&e.message().to_string()),
            )
        })
    }
}
