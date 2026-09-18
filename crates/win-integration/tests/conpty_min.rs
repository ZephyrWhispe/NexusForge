//! ConPTY 最小对照实验（逐行照官方 EchoCon 模式）——定位主实现读路径差异。
//!
//! 分诊诊断探针（DECISIONS D-20）：2026-09-18 用它定位出 conpty.rs 的两处时序修复
//! （句柄须在 CreateProcess 后关闭、watcher 须等读端 EOF）。诊断目的已达成，
//! 默认不参与测试门禁；复现时用一次性终端执行：
//! `cargo test -p win-integration --test conpty_min -- --ignored --nocapture`
//! 注意：用例会调用 FreeConsole() 摘除 cargo test 进程自身的控制台（这正是它的实验目的），
//! 在常规终端里运行会造成该终端会话输出丢失，故必须用可丢弃的终端窗口。

use std::os::windows::ffi::OsStrExt;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Console::{ClosePseudoConsole, COORD, CreatePseudoConsole, HPCON};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, UpdateProcThreadAttribute,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTUPINFOEXW,
};
use windows::Win32::Storage::FileSystem::ReadFile;

fn make_pipe() -> (HANDLE, HANDLE) {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let mut r = HANDLE::default();
    let mut w = HANDLE::default();
    unsafe { CreatePipe(&mut r, &mut w, Some(&sa), 0).expect("CreatePipe") };
    (r, w)
}

#[test]
#[ignore = "分诊诊断探针（D-20）：含 FreeConsole() 宿主副作用，仅在一次性终端中手动复现 ConPTY 渲染问题时运行"]
fn conpty_min_echocon_reference() {
    // 关键对照：本测试进程挂在 PowerShell 控制台上，而生产宿主（GUI 应用 / Windows Terminal /
    // VS Code）都没有控制台。主动摘掉控制台，验证「宿主有控制台导致 ConPTY 不渲染」假设。
    let detached = unsafe { windows::Win32::System::Console::FreeConsole() };
    println!("[min] FreeConsole 结果: {detached:?}（true=已摘除控制台）");

    let (in_read, in_write) = make_pipe();
    let (out_read, out_write) = make_pipe();

    let size = COORD { X: 120, Y: 40 };
    let hpc: HPCON =
        unsafe { CreatePseudoConsole(size, in_read, out_write, 0).expect("CreatePseudoConsole") };

    // 属性列表（照 EchoCon）
    let mut list_size: usize = 0;
    let _ = unsafe {
        InitializeProcThreadAttributeList(LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()), 1, 0, &mut list_size)
    };
    let mut list_buf = vec![0u8; list_size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(list_buf.as_mut_ptr().cast());
    unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut list_size).expect("InitAttrList") };
    unsafe {
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            // A/B 对称实验：lpValue = 指向 HPCON 值的指针
            Some(std::ptr::from_ref(&hpc.0).cast::<core::ffi::c_void>()),
            std::mem::size_of::<usize>(),
            None,
            None,
        )
        .expect("UpdateAttr");
    }

    let mut cmd = "cmd.exe /c pause".encode_utf16().chain([0]).collect::<Vec<u16>>();
    let mut si = STARTUPINFOEXW::default();
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.lpAttributeList = list;
    let mut pi = PROCESS_INFORMATION::default();
    // env=None → 不加 CREATE_UNICODE_ENVIRONMENT（M13 教训：NULL 环境 + UNICODE flag 崩 cmd）
    let flags = EXTENDED_STARTUPINFO_PRESENT;
    unsafe {
        CreateProcessW(
            PCWSTR::null(),
            PWSTR(cmd.as_mut_ptr()),
            None,
            None,
            false,
            flags,
            None,
            PCWSTR::null(),
            &si.StartupInfo,
            &mut pi,
        )
        .expect("CreateProcess");
    }
    // CreateProcess 之后关闭 PTY 侧句柄（EchoCon 顺序）
    unsafe {
        let _ = CloseHandle(in_read);
        let _ = CloseHandle(out_write);
        let _ = CloseHandle(pi.hThread);
    }

    // 主动注入输入：写一条命令进伪终端输入管道（in_write），观察是否触发渲染输出
    // ——若写入后管道有数据回来，说明接线正确、只是"无输入不渲染"
    {
        let raw_in = in_write.0 as isize;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(800));
            let h = HANDLE(raw_in as *mut core::ffi::c_void);
            let data = b"echo NFTEST_INJECT\r\n";
            let mut written: u32 = 0;
            let r = unsafe {
                windows::Win32::Storage::FileSystem::WriteFile(h, Some(data), Some(&mut written), None)
            };
            println!("[min] 注入输入: r={r:?} written={written}");
        });
    }

    // 读线程 + 超时收尾：阻塞 ReadFile 必须放独立线程，主线程用 recv_timeout 兜底
    // （W7 教训：同步 ReadFile 直接读会永久挂死测试——deadline 检查在阻塞之后无效）
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let raw = out_read.0 as isize; // 句柄转整数跨线程（HANDLE 非 Send）
    std::thread::spawn(move || {
        let h = HANDLE(raw as *mut core::ffi::c_void);
        let mut buf = [0u8; 4096];
        loop {
            let mut n: u32 = 0;
            let r = unsafe { ReadFile(h, Some(&mut buf), Some(&mut n), None) };
            if r.is_err() || n == 0 {
                println!("[min] read exit r={r:?} n={n}");
                break;
            }
            println!("[min] chunk {}B", n);
            if tx.send(buf[..n as usize].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut collected = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        let remain = deadline.saturating_duration_since(std::time::Instant::now());
        if remain.is_zero() {
            println!("[min] 8s 超时收尾（读端无数据，无 EOF）");
            break;
        }
        match rx.recv_timeout(remain) {
            Ok(chunk) => collected.extend_from_slice(&chunk),
            Err(_) => break, // 超时或读线程结束（EOF）
        }
    }
    let text = String::from_utf8_lossy(&collected).to_string();
    println!("[min] 共 {} 字节: {:?}", collected.len(), text);

    // 收尾：杀子进程（解除 pause 等待）→ 关 PTY/句柄
    unsafe {
        let _ = windows::Win32::System::Threading::TerminateProcess(pi.hProcess, 1);
        let _ = ClosePseudoConsole(hpc);
        let _ = CloseHandle(pi.hProcess);
    }

    // 判定：pause 提示语（请按任意键继续/Press any key）只在伪终端渲染流中出现
    assert!(
        text.contains("continue") || text.contains("继续"),
        "pause 提示未出现在渲染流（子进程未挂上伪终端，或 conhost 未渲染）——共 {} 字节",
        collected.len()
    );
}
