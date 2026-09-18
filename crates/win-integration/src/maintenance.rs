//! 系统维护动作（docs/impl/08 W4–W6 MaintenancePort 本地实现）。
//!
//! - clean_dir：清空目录内容（保留目录本身）；24h 白名单（mtime 内新文件跳过），
//!   与 sys-core clean 模块同一安全口径。helper 端 `file.clean_dir` 方法复用本实现，
//!   但路径白名单由 helper dispatch 层硬限制（纵深防御——提权进程不删任意目录）
//! - exec：白名单程序执行（powercfg|dism|sfc|netsh|onedrive_uninstall）——spec §4.3
//!   安全约束：args 参数模板拼接禁止透传任意字符串；超时强杀；输出截断 256KB
//! - restore_point：srclient.dll SRSetRestorePointW（BEGIN→END 两段式；系统还原未
//!   开启 → 1058 如实报错）；windows crate 0.58 不覆盖 srclient → 手写 FFI
//! - empty_working_set：EnumProcesses → EmptyWorkingSet（psapi；跳过自身/权限失败）

use std::os::windows::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

use host_core::error::AppError;
use host_core::ports::{MaintenancePort, RepairKind};

/// CREATE_NO_WINDOW（不弹控制台窗）
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 输出截断上限（helper 帧 16MB 内留余量）
const OUTPUT_CAP: usize = 256 * 1024;

/// Exec 白名单（固定程序名精确匹配；onedrive_uninstall 为固定脚本映射非透传）
const EXEC_ALLOWLIST: &[&str] = &["powercfg", "dism", "sfc", "netsh", "onedrive_uninstall"];

/// exec 输出截断（保尾部——错误信息通常在尾部）
fn cap_output(mut s: String) -> String {
    if s.len() > OUTPUT_CAP {
        let cut = s.len() - OUTPUT_CAP;
        // 对齐 UTF-8 边界
        while !s.is_char_boundary(cut.min(s.len())) && cut < s.len() {
            s.remove(0);
        }
        format!("…（输出已截断）\n{}", &s[cut.min(s.len())..])
    } else {
        s
    }
}

pub struct MaintenanceWin;

impl MaintenanceWin {
    pub fn new() -> Self {
        Self
    }

    /// 执行白名单程序（超时 try_wait 轮询强杀；stdout+stderr 合并截断）
    fn run_exec(program: &str, args: &[String], timeout_ms: u32) -> Result<String, AppError> {
        if !EXEC_ALLOWLIST.contains(&program) {
            return Err(AppError::module(
                "SYS_MAINT_010",
                format!("程序 {program} 不在执行白名单（powercfg/dism/sfc/netsh/onedrive_uninstall）"),
                None,
            ));
        }
        // 参数模板：白名单程序 + 固定参数集由 catalog 提供；onedrive_uninstall 展开为固定卸载命令
        let (exe, real_args): (String, Vec<String>) = if program == "onedrive_uninstall" {
            // OneDrive 卸载：System32 优先（新版），回落 SysWOW64（旧版）
            let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            let sys32 = format!(r"{sysroot}\System32\OneDriveSetup.exe");
            let wow64 = format!(r"{sysroot}\SysWOW64\OneDriveSetup.exe");
            let path = if std::path::Path::new(&sys32).exists() { sys32 } else { wow64 };
            (path, vec!["/uninstall".to_string()])
        } else {
            (program.to_string(), args.to_vec())
        };
        let mut child = std::process::Command::new(&exe)
            .args(&real_args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| AppError::module("SYS_MAINT_011", format!("{program} 启动失败: {e}"), None))?;
        // 管道读线程（try_wait 收割后 wait_with_output 不可用——标准坑）
        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        let out_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(p) = &mut stdout_pipe {
                use std::io::Read;
                let _ = p.read_to_end(&mut buf);
            }
            buf
        });
        let err_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(p) = &mut stderr_pipe {
                use std::io::Read;
                let _ = p.read_to_end(&mut buf);
            }
            buf
        });
        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) => {
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        // 读线程随管道关闭自然结束；join 防泄漏
                        let _ = out_reader.join();
                        let _ = err_reader.join();
                        return Err(AppError::module(
                            "SYS_MAINT_012",
                            format!("{program} 执行超时（{timeout_ms}ms，已终止）"),
                            None,
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(120));
                }
                Err(e) => return Err(AppError::module("SYS_MAINT_013", format!("{program} 等待失败: {e}"), None)),
            }
        };
        let out_buf = out_reader.join().unwrap_or_default();
        let err_buf = err_reader.join().unwrap_or_default();
        let mut text = String::from_utf8_lossy(&out_buf).into_owned();
        let err_text = String::from_utf8_lossy(&err_buf);
        if !err_text.trim().is_empty() {
            text.push_str("\n[stderr] ");
            text.push_str(&err_text);
        }
        if !status.success() {
            return Err(AppError::module(
                "SYS_MAINT_014",
                format!("{program} 退出码 {}：{}", status.code().unwrap_or(-1), cap_output(text)),
                None,
            ));
        }
        Ok(cap_output(text))
    }

    /// 系统还原点（srclient FFI：BEGIN_SYSTEM_CHANGE → 记录 seq → END_SYSTEM_CHANGE）
    fn create_restore_point(description: &str) -> Result<(), AppError> {
        #[repr(C)]
        struct RestorePointInfoW {
            dw_event_type: u32,
            dw_restore_pt_type: u32,
            ll_sequence_number: i64,
            sz_description: [u16; 256],
        }
        #[repr(C)]
        struct SmgrStatus {
            n_status: u32,
            ll_sequence_number: i64,
        }
        #[link(name = "srclient")]
        extern "system" {
            fn SRSetRestorePointW(p_restore_pt_spec: *mut RestorePointInfoW, p_smgr_status: *mut SmgrStatus) -> i32;
        }
        const BEGIN_SYSTEM_CHANGE: u32 = 100;
        const END_SYSTEM_CHANGE: u32 = 101;
        const MODIFY_SETTINGS: u32 = 12;
        // 描述截断 255 WCHAR（结构体要求含 NUL）
        let mut desc = [0u16; 256];
        for (i, c) in description.encode_utf16().take(255).enumerate() {
            desc[i] = c;
        }
        let mut begin = RestorePointInfoW {
            dw_event_type: BEGIN_SYSTEM_CHANGE,
            dw_restore_pt_type: MODIFY_SETTINGS,
            ll_sequence_number: 0,
            sz_description: desc,
        };
        let mut status = SmgrStatus { n_status: 0, ll_sequence_number: 0 };
        // SAFETY：两个结构体均为合法栈上 FFI 参数；srclient 仅写 status/begin
        let ok = unsafe { SRSetRestorePointW(&mut begin, &mut status) };
        if ok == 0 {
            let code = status.n_status;
            let hint = match code {
                1058 => Some("系统还原未开启（控制面板 → 恢复 → 配置系统还原）"),
                1055 => Some("还原点创建被策略禁用"),
                _ => None,
            };
            return Err(AppError::module("SYS_MAINT_020", format!("创建还原点失败（错误码 {code}）"), hint));
        }
        let seq = status.ll_sequence_number;
        let mut end = RestorePointInfoW {
            dw_event_type: END_SYSTEM_CHANGE,
            dw_restore_pt_type: MODIFY_SETTINGS,
            ll_sequence_number: seq,
            sz_description: desc,
        };
        let mut status2 = SmgrStatus { n_status: 0, ll_sequence_number: 0 };
        // SAFETY：同上
        let ok2 = unsafe { SRSetRestorePointW(&mut end, &mut status2) };
        if ok2 == 0 {
            return Err(AppError::module(
                "SYS_MAINT_021",
                format!("还原点收尾失败（错误码 {}）", status2.n_status),
                None,
            ));
        }
        Ok(())
    }

    /// 系统修复（RepairKind → 白名单固定参数模板）
    fn run_repair(kind: RepairKind) -> Result<String, AppError> {
        // DISM/SFC 常规 10–30 分钟；超时 30 分钟
        const REPAIR_TIMEOUT_MS: u32 = 30 * 60 * 1000;
        match kind {
            RepairKind::DismScanHealth => {
                Self::run_exec("dism", &["/Online".into(), "/Cleanup-Image".into(), "/ScanHealth".into()], REPAIR_TIMEOUT_MS)
            }
            RepairKind::DismRestoreHealth => {
                Self::run_exec("dism", &["/Online".into(), "/Cleanup-Image".into(), "/RestoreHealth".into()], REPAIR_TIMEOUT_MS)
            }
            RepairKind::DismComponentCleanup => {
                Self::run_exec("dism", &["/Online".into(), "/Cleanup-Image".into(), "/StartComponentCleanup".into()], REPAIR_TIMEOUT_MS)
            }
            RepairKind::SfcScanNow => Self::run_exec("sfc", &["/scannow".into()], REPAIR_TIMEOUT_MS),
        }
    }

    /// Defender 实时保护开关（固定 PowerShell 模板——整个命令二选一，无任何用户输入拼接）
    fn set_defender_realtime(disable: bool) -> Result<(), AppError> {
        // $true/$false 由 bool 决定；命令本身固定（不是通用 powershell exec，无注入面）
        let flag = if disable { "$true" } else { "$false" };
        let script = format!("Set-MpPreference -DisableRealtimeMonitoring {flag}");
        let out = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| AppError::module("SYS_MAINT_030", format!("PowerShell 启动失败: {e}"), None))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            // 篡改保护拦截 / 非管理员 / Defender 服务被第三方接管 → 统一如实报错 + 官方指引
            let hint = "若被篡改保护拦截：Windows 安全中心 → 病毒和威胁防护 → 管理设置 → 关闭篡改保护；需管理员权限";
            return Err(AppError::module(
                "SYS_MAINT_031",
                format!("Defender 实时保护切换失败: {stderr}"),
                Some(hint),
            ));
        }
        Ok(())
    }
}

impl Default for MaintenanceWin {
    fn default() -> Self {
        Self::new()
    }
}

impl MaintenancePort for MaintenanceWin {
    fn clean_dir(&self, path: &str, recursive: bool, skip_recent_hours: u32) -> Result<u32, AppError> {
        let dir = std::path::Path::new(path);
        if !dir.is_dir() {
            // 目录不存在 → 视为已清理（幂等）
            return Ok(0);
        }
        let skip = Duration::from_secs(skip_recent_hours as u64 * 3600);
        let now = std::time::SystemTime::now();
        let mut removed = 0u32;
        let entries =
            std::fs::read_dir(dir).map_err(|e| AppError::module("SYS_MAINT_001", format!("读取 {path} 失败: {e}"), None))?;
        for e in entries.flatten() {
            let p = e.path();
            // 占用/权限失败逐条容忍（部分清理优于全盘报错）；24h 内新文件跳过（在用缓存保护）
            let fresh = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .map(|age| age < skip)
                .unwrap_or(false);
            if fresh {
                continue;
            }
            let ok = if p.is_dir() {
                if recursive {
                    std::fs::remove_dir_all(&p).is_ok()
                } else {
                    std::fs::remove_dir(&p).is_ok()
                }
            } else {
                std::fs::remove_file(&p).is_ok()
            };
            if ok {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn exec(&self, program: &str, args: &[String], timeout_ms: u32) -> Result<String, AppError> {
        Self::run_exec(program, args, timeout_ms)
    }

    fn restore_point(&self, description: &str) -> Result<(), AppError> {
        Self::create_restore_point(description)
    }

    fn empty_working_set(&self) -> Result<u32, AppError> {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::ProcessStatus::{EmptyWorkingSet, EnumProcesses};
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_INFORMATION, PROCESS_SET_QUOTA};
        const MAX_PIDS: usize = 4096;
        let mut pids = [0u32; MAX_PIDS];
        let mut bytes_returned = 0u32;
        // SAFETY：固定容量缓冲区 + 长度传入；EnumProcesses 只写 bytes_returned 内
        unsafe {
            EnumProcesses(pids.as_mut_ptr(), (MAX_PIDS * 4) as u32, &mut bytes_returned);
        }
        let count = (bytes_returned as usize / 4).min(MAX_PIDS);
        let self_pid = std::process::id();
        let mut ok_count = 0u32;
        for &pid in &pids[..count] {
            if pid == 0 || pid == self_pid {
                continue; // 系统空闲进程 / 自身
            }
            // SAFETY：OpenProcess 句柄用后即关；EmptyWorkingSet 仅接受合法进程句柄
            unsafe {
                let Ok(h) = OpenProcess(
                    PROCESS_ACCESS_RIGHTS(PROCESS_SET_QUOTA.0 | PROCESS_QUERY_INFORMATION.0),
                    false,
                    pid,
                ) else { continue };
                if !h.is_invalid() && EmptyWorkingSet(h).is_ok() {
                    ok_count += 1;
                }
                let _ = CloseHandle(HANDLE(h.0));
            }
        }
        Ok(ok_count)
    }

    fn repair(&self, kind: RepairKind) -> Result<String, AppError> {
        Self::run_repair(kind)
    }

    fn defender_realtime(&self, disable: bool) -> Result<(), AppError> {
        Self::set_defender_realtime(disable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exec 白名单：非白名单程序拒绝（不启动进程）
    #[test]
    fn exec_allowlist_rejects_unknown_program() {
        let m = MaintenanceWin::new();
        let err = m.exec("powershell", &["-Command".into(), "whoami".into()], 1000).unwrap_err();
        assert!(err.to_string().contains("白名单"), "任意程序必须拒绝: {err}");
        let err2 = m.exec("cmd", &["/c".into(), "echo hi".into()], 1000).unwrap_err();
        assert!(err2.to_string().contains("白名单"));
    }

    /// 超时强杀：非提权下 sfc 立即失败（无法构造白名单长任务）→ #[ignore] 提权环境手动跑：
    /// `cargo test -p win-integration exec_timeout -- --ignored`（sfc /scannow 30s 超时触发 SYS_MAINT_012）
    #[test]
    #[ignore]
    fn exec_timeout_kills_long_task() {
        let m = MaintenanceWin::new();
        let err = m.exec("sfc", &["/scannow".into()], 30_000).unwrap_err();
        assert!(err.to_string().contains("超时"), "长任务超时应报错: {err}");
    }

    /// 真机：白名单程序正常执行（powercfg /? 极快且无副作用）
    #[test]
    fn exec_powercfg_help() {
        let m = MaintenanceWin::new();
        let out = m.exec("powercfg", &["/?".to_string()], 10_000).unwrap();
        assert!(!out.is_empty(), "powercfg /? 应有输出");
    }

    /// 输出截断保尾部
    #[test]
    fn cap_output_keeps_tail() {
        let big = "x".repeat(300 * 1024) + "TAIL";
        let capped = cap_output(big);
        assert!(capped.len() <= OUTPUT_CAP + 64);
        assert!(capped.ends_with("TAIL"), "尾部保留");
        assert!(capped.contains("截断"));
    }

    /// 清理语义：旧文件删除、24h 白名单跳过、目录本身保留、返回计数
    #[test]
    fn clean_dir_whitelist_and_count() {
        use std::time::{Duration, SystemTime};
        let base = std::env::temp_dir().join(format!("nf_maint_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let old = base.join("old.txt");
        std::fs::write(&old, b"x").unwrap();
        // 旧化 old.txt（mtime → 3 天前）：File::set_times（Rust 1.75+）
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(3 * 86400)))
            .unwrap();
        // "新"文件：mtime 为当前时刻 → 24h 白名单命中
        let fresh = base.join("fresh.txt");
        std::fs::write(&fresh, b"x").unwrap();
        let m = MaintenanceWin::new();
        let n = m.clean_dir(base.to_str().unwrap(), true, 24).unwrap();
        // old.txt（旧）删除；fresh.txt 与刚创建的 sub 目录（mtime 24h 内）→ 白名单跳过
        assert_eq!(n, 1, "仅 old.txt 删除；fresh.txt 与刚创建的 sub 目录被白名单跳过");
        assert!(base.is_dir(), "目录本身保留");
        assert!(fresh.exists(), "24h 内新文件不删");
        assert!(sub.exists(), "24h 内新建目录不删");
        // skip=0：全部删除（clear_cache 口径），含递归子目录与首轮跳过的 sub
        let sub2 = base.join("sub2");
        std::fs::create_dir_all(sub2.join("deep")).unwrap();
        std::fs::write(sub2.join("deep").join("n.txt"), b"x").unwrap();
        let n2 = m.clean_dir(base.to_str().unwrap(), true, 0).unwrap();
        assert_eq!(n2, 3, "fresh.txt + sub（首轮跳过）+ sub2 递归删除");
        assert!(!fresh.exists());
        assert!(!sub.exists());
        assert!(!sub2.exists());
        // 幂等：目录不存在返回 0
        let n3 = m.clean_dir(base.join("no_such").to_str().unwrap(), true, 24).unwrap();
        assert_eq!(n3, 0);
        let _ = std::fs::remove_dir_all(&base);
    }
}
