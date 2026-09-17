//! ConPTY 伪终端封装（docs/impl/06 T1）：CreatePseudoConsole + 两条匿名管道。
//!
//! 关闭顺序（docs/impl/06 风险标注）：ClosePseudoConsole → 等子进程（5s）→ Terminate 兜底 → 关句柄；
//! 写与 resize 单 worker select 串行化（并发会损坏转义序列）；
//! TermCfg.shell 为完整命令行（可含参数，如 `wsl.exe -d Ubuntu`）。
//!
//! windows 0.58 归置：PseudoConsole API 在 `Win32::System::Console`，CreatePipe 在
//! `Win32::System::Pipes`，SetHandleInformation 在 `Win32::Foundation`。

use std::collections::HashMap;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use host_core::error::AppError;
use host_core::ports::{ConptyPort, PtyHandle, TermCfg};
use tokio::sync::mpsc;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Console::{
    ClosePseudoConsole, COORD, CreatePseudoConsole, ResizePseudoConsole, HPCON,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTUPINFOEXW,
};
/// 单会话资源（Arc 共享；kill 闭包与最后一次 Drop 幂等触发关闭）
struct SessionInner {
    hpc: HPCON,
    h_process: HANDLE,
    h_thread: HANDLE,
    in_write: HANDLE,
    closed: AtomicBool,
}

unsafe impl Send for SessionInner {}
unsafe impl Sync for SessionInner {}

impl SessionInner {
    /// 关闭序列（幂等）：ClosePseudoConsole → 等子进程 → Terminate 兜底 → 关句柄
    fn shutdown(&self) {
        if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        unsafe {
            // 1. 关 ConPTY：output 管道收到 EOF，读线程自然退出
            ClosePseudoConsole(self.hpc);
            // 2. 等子进程（ConPTY 关闭通常带动子进程退出）
            if WaitForSingleObject(self.h_process, 5000) != WAIT_OBJECT_0 {
                let _ = TerminateProcess(self.h_process, 1);
                let _ = WaitForSingleObject(self.h_process, 2000);
            }
            // 3. 关句柄（in_write 由 worker 关闭——worker 因 WriteFile 失败退出前不关）
            let _ = CloseHandle(self.h_process);
            let _ = CloseHandle(self.h_thread);
            let _ = CloseHandle(self.in_write);
        }
    }

    /// 退出码（None = 仍在运行）
    fn exit_code(&self) -> Option<u32> {
        let mut code: u32 = 0;
        unsafe {
            if GetExitCodeProcess(self.h_process, &mut code).is_ok() && code != 259 {
                return Some(code);
            }
        }
        None
    }
}

impl Drop for SessionInner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// HANDLE 跨线程传递包装（裸指针默认 !Send；句柄本身可跨线程使用）
#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}

/// ConPTY 会话（实现 [`ConptyPort`]，state.rs 注册）
pub struct ConptyWin;

impl ConptyWin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConptyWin {
    fn default() -> Self {
        Self::new()
    }
}

/// 创建一条两端可继承的匿名管道（EchoCon 模式；bInheritHandles=FALSE 时
/// 只有 PSEUDOCONSOLE 属性机制传递句柄，无多余句柄泄漏）
fn make_pipe() -> Result<(HANDLE, HANDLE), AppError> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let mut r = HANDLE::default();
    let mut w = HANDLE::default();
    unsafe {
        CreatePipe(&mut r, &mut w, Some(&sa), 0)
            .map_err(|e| AppError::module("TERM_PTY_001", format!("CreatePipe 失败: {e}"), None))?;
    }
    Ok((r, w))
}

impl ConptyPort for ConptyWin {
    fn spawn(&self, cfg: TermCfg) -> Result<PtyHandle, AppError> {
        // 1. 两条匿名管道：input（我们写 w → PTY 读 r）、output（PTY 写 w → 我们读 r）
        let (in_read, in_write) = make_pipe()?;
        let (out_read, out_write) = make_pipe()?;
        // 我们持有的两端摘除继承位（防子进程持有导致 EOF 永不到来）
        unsafe {
            SetHandleInformation(in_write, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT)
                .map_err(|e| AppError::module("TERM_PTY_001", format!("SetHandleInformation(in_write): {e}"), None))?;
            SetHandleInformation(out_read, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT)
                .map_err(|e| AppError::module("TERM_PTY_001", format!("SetHandleInformation(out_read): {e}"), None))?;
        }

        // 2. CreatePseudoConsole（PTY 取走 in_read / out_write）
        let size = COORD { X: cfg.cols.max(1) as i16, Y: cfg.rows.max(1) as i16 };
        let hpc = unsafe {
            CreatePseudoConsole(size, in_read, out_write, 0)
                .map_err(|e| AppError::module("TERM_PTY_002", format!("CreatePseudoConsole 失败: {e}"), None))?
        };
        // 子端句柄已被 ConPTY 复制，关闭我们的副本
        unsafe {
            let _ = CloseHandle(in_read);
            let _ = CloseHandle(out_write);
        }

        // 3. 子进程（shell 为完整命令行）
        let env_block = build_env_block(&cfg.env);
        let (h_process, h_thread) = unsafe { spawn_child(&cfg, hpc, env_block.as_ptr()) }?;

        let inner = Arc::new(SessionInner {
            hpc,
            h_process,
            h_thread,
            in_write,
            closed: AtomicBool::new(false),
        });

        // 4. 输出线程：同步 ReadFile → blocking_send（EOF = ClosePseudoConsole 触发）
        let (out_tx, output_rx) = mpsc::channel::<Vec<u8>>(512);
        let out_read = SendHandle(out_read);
        std::thread::Builder::new()
            .name("nf-pty-read".into())
            .spawn(move || {
                // 整体捕获 SendHandle（edition 2021 字段级捕获会抓裸 HANDLE 破坏 Send）
                let out = out_read;
                let mut buf = [0u8; 8192];
                loop {
                    let mut n: u32 = 0;
                    let ok = unsafe {
                        windows::Win32::Storage::FileSystem::ReadFile(
                            out.0,
                            Some(&mut buf),
                            Some(&mut n),
                            None,
                        )
                        .is_ok()
                    };
                    if !ok || n == 0 {
                        break;
                    }
                    if out_tx.blocking_send(buf[..n as usize].to_vec()).is_err() {
                        break;
                    }
                }
                unsafe {
                    let _ = CloseHandle(out.0);
                }
            })
            .map_err(|e| AppError::module("TERM_PTY_003", format!("读线程创建失败: {e}"), None))?;

        // 5. 写 + resize 串行 worker（单任务 select，每个操作 await 完再收下一个；
        //    并发会损坏转义序列——docs/impl/06 T1 风险标注）
        let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(256);
        let (resize_tx, mut resize_rx) = mpsc::channel::<(u16, u16)>(16);
        let worker_inner = inner.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    w = input_rx.recv() => match w {
                        Some(data) if !data.is_empty() => {
                            // 捕获整个 Arc（字段级捕获会抓裸 HANDLE 破坏 Send）
                            let inner2 = worker_inner.clone();
                            let ok = tokio::task::spawn_blocking(move || unsafe {
                                windows::Win32::Storage::FileSystem::WriteFile(inner2.in_write, Some(&data), None, None).is_ok()
                            })
                            .await
                            .unwrap_or(false);
                            if !ok {
                                break; // 管道断开（会话关闭）
                            }
                        }
                        Some(_) => {}
                        None => break,
                    },
                    r = resize_rx.recv() => match r {
                        Some((c, rows)) => {
                            let inner3 = worker_inner.clone();
                            let _ = tokio::task::spawn_blocking(move || unsafe {
                                ResizePseudoConsole(
                                    inner3.hpc,
                                    COORD { X: c.max(1) as i16, Y: rows.max(1) as i16 },
                                )
                            })
                            .await;
                        }
                        None => {}
                    },
                }
            }
        });

        Ok(PtyHandle::new(
            input_tx,
            output_rx,
            resize_tx,
            {
                let inner = inner.clone();
                Box::new(move || inner.shutdown())
            },
        ))
    }
}

/// 退出码查询入口（term-core 经 Port 拿不到；v1 退出码随 EOF 置 null，此函数留作迭代）
#[allow(dead_code)]
pub(crate) fn conpty_exit_code(inner: &Arc<SessionInner>) -> Option<u32> {
    inner.exit_code()
}

/// 环境块：继承当前进程 + cfg.env 覆盖（终端必须保留 PATH 等）；UTF-16 排序块
fn build_env_block(env: &HashMap<String, String>) -> Vec<u16> {
    let mut merged: HashMap<String, String> = std::env::vars().collect();
    for (k, v) in env {
        merged.insert(k.clone(), v.clone());
    }
    let mut pairs: Vec<(String, String)> = merged.into_iter().collect();
    pairs.sort_by(|a, b| a.0.to_uppercase().cmp(&b.0.to_uppercase()));
    let mut block = Vec::new();
    for (k, v) in pairs {
        block.extend(k.encode_utf16());
        block.push(0);
        block.extend(v.encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

/// STARTUPINFOEXW + PSEUDOCONSOLE 属性创建子进程；返回 (hProcess, hThread)
///
/// # Safety
/// `env_block` 必须是以双 NUL 结尾的 UTF-16 块指针；hpc 必须有效
unsafe fn spawn_child(cfg: &TermCfg, hpc: HPCON, env_block: *const u16) -> Result<(HANDLE, HANDLE), AppError> {
    // 属性列表（1 项：PSEUDOCONSOLE）；先取尺寸（null 调用返回错误但写回 size）
    let mut list_size: usize = 0;
    let _ = InitializeProcThreadAttributeList(LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()), 1, 0, &mut list_size);
    let mut list_buf = vec![0u8; list_size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(list_buf.as_mut_ptr().cast());
    InitializeProcThreadAttributeList(list, 1, 0, &mut list_size)
        .map_err(|e| AppError::module("TERM_PTY_004", format!("初始化属性列表失败: {e}"), None))?;
    let update = UpdateProcThreadAttribute(
        list,
        0,
        PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
        Some(hpc.0 as *mut core::ffi::c_void),
        std::mem::size_of::<usize>(),
        None,
        None,
    );
    if let Err(e) = update {
        DeleteProcThreadAttributeList(list);
        return Err(AppError::module("TERM_PTY_004", format!("设置 PSEUDOCONSOLE 属性失败: {e}"), None));
    }

    // 命令行（可变 UTF-16 缓冲）
    let mut cmd: Vec<u16> = cfg.shell.encode_utf16().collect();
    cmd.push(0);
    // cwd
    let cwd_wide: Option<Vec<u16>> = cfg.cwd.as_ref().map(|p| {
        let mut v: Vec<u16> = p.as_os_str().encode_wide().collect();
        v.push(0);
        v
    });

    let mut si = STARTUPINFOEXW::default();
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.lpAttributeList = list;
    let mut pi = PROCESS_INFORMATION::default();

    let ok = CreateProcessW(
        PCWSTR::null(),
        PWSTR(cmd.as_mut_ptr()),
        None,
        None,
        false,
        EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
        Some(env_block.cast()),
        cwd_wide
            .as_ref()
            .map(|v| PCWSTR::from_raw(v.as_ptr()))
            .unwrap_or_else(|| PCWSTR::null()),
        &si.StartupInfo,
        &mut pi,
    );
    DeleteProcThreadAttributeList(list);
    if let Err(e) = ok {
        return Err(AppError::module(
            "TERM_PTY_005",
            format!("CreateProcess 失败（{}）: {e}", cfg.shell),
            None,
        ));
    }
    Ok((pi.hProcess, pi.hThread))
}
