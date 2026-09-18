//! 提权 Helper（docs/impl/08 §4 W3）：ShellExecuteExW runas 拉起 + 命名管道帧原语。
//!
//! 帧格式 `[u32 BE len][payload]`（payload = 单条 JSON-RPC 消息）；v1 同步实现
//! （引擎全同步 + docker.rs 管道先例；相对文档 tokio named pipe 的偏离记录于项目记忆）。
//!
//! 安全模型（规格 §4.3 全部强制）：
//! - spawn：token 经 `NF_HELPER_TOKEN` 环境变量注入（提权进程继承调用方环境块），spawn 后立即清除
//! - helper 侧：`GetNamedPipeClientProcessId` → 镜像路径必须与自身同目录 + 首帧 token 匹配
//! - 主进程侧：spawn 后校验回传句柄的镜像路径 == 请求的 helper_exe（防 TOCTOU 替换）
//! - 管道：`FILE_FLAG_FIRST_PIPE_INSTANCE` 单实例互斥；默认 DACL 限当前用户

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use host_core::error::AppError;
use host_core::ports::{HelperSpawnPort, HelperSpec};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ,
    GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    WaitNamedPipeW, NAMED_PIPE_MODE,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, GetProcessId, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};

/// helper 可执行文件名（与主 exe 同目录交付）
pub const HELPER_EXE_NAME: &str = "nexusforge-sys-helper.exe";
/// token 注入环境变量（ShellExecuteExW 提权进程继承调用方环境）
pub const TOKEN_ENV: &str = "NF_HELPER_TOKEN";
/// 帧载荷上限（16MB——防异常/恶意帧撑爆内存）
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// 管道句柄类型重导出（helper bin 不直依 windows crate——DESIGN O2 纪律）
pub type PipeHandle = HANDLE;

/// 关闭管道句柄
pub fn close_handle(h: PipeHandle) {
    unsafe {
        let _ = CloseHandle(h);
    }
}

/// helper 管道名（按主进程 PID 派生，同应用多实例隔离）
pub fn pipe_name_for(parent_pid: u32) -> String {
    format!(r"\\.\pipe\nexusforge-sys-helper-v1-{parent_pid}")
}

/// UTF-16 宽字符串（NUL 结尾）
pub(crate) fn to_wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// 查询进程镜像路径（OpenProcess + QueryFullProcessImageNameW）
pub fn query_image_path(pid: u32) -> Result<PathBuf, AppError> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).map_err(|e| {
            AppError::module("SYS_HELPER_001", format!("OpenProcess({pid}) 失败: {e}"), None)
        })?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut size);
        let _ = CloseHandle(h);
        r.map_err(|e| {
            AppError::module("SYS_HELPER_001", format!("QueryFullProcessImageNameW({pid}) 失败: {e}"), None)
        })?;
        Ok(PathBuf::from(String::from_utf16_lossy(&buf[..size as usize])))
    }
}

/// 两个路径是否同目录（canonicalize 失败时按字面 parent 比较）
pub fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.parent(), b.parent()) {
        (Some(pa), Some(pb)) => match (pa.canonicalize(), pb.canonicalize()) {
            (Ok(x), Ok(y)) => x == y,
            _ => pa == pb,
        },
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// HelperSpawnPort：ShellExecuteExW runas（UAC 弹窗）
// ---------------------------------------------------------------------------

pub struct HelperSpawnWin;

impl HelperSpawnWin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HelperSpawnWin {
    fn default() -> Self {
        Self::new()
    }
}

impl HelperSpawnPort for HelperSpawnWin {
    fn spawn(&self, spec: &HelperSpec) -> Result<u32, AppError> {
        let verb = to_wide("runas");
        let file = to_wide(spec.helper_exe.to_string_lossy().as_ref());
        let params = format!("--parent-pid {} --idle-exit {}", spec.parent_pid, spec.idle_exit_secs);
        let params_w = to_wide(&params);
        // token 注入：提权进程继承调用方环境块；spawn 调用结束立即清除
        std::env::set_var(TOKEN_ENV, &spec.token);
        let result: Result<u32, AppError> = unsafe {
            let mut sei = SHELLEXECUTEINFOW {
                cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                fMask: SEE_MASK_NOCLOSEPROCESS,
                lpVerb: PCWSTR::from_raw(verb.as_ptr()),
                lpFile: PCWSTR::from_raw(file.as_ptr()),
                lpParameters: PCWSTR::from_raw(params_w.as_ptr()),
                nShow: 0, // SW_HIDE（helper 为 windows_subsystem=windows，无窗口）
                ..Default::default()
            };
            let r = ShellExecuteExW(&mut sei);
            std::env::remove_var(TOKEN_ENV);
            r.map_err(|e| {
                AppError::module("SYS_HELPER_002", format!("提权拉起 Helper 失败（UAC 取消或被策略拦截）: {e}"), None)
            })?;
            if sei.hProcess.is_invalid() || sei.hProcess == HANDLE::default() {
                return Ok(0); // NOCLOSEPROCESS 未回传句柄——握手重试兜底
            }
            // 主进程侧校验：spawn 出来的镜像必须是请求的 helper exe（防 TOCTOU 替换）
            let mut buf = [0u16; 1024];
            let mut size = buf.len() as u32;
            let image_ok = QueryFullProcessImageNameW(
                sei.hProcess,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
            .map(|_| {
                PathBuf::from(String::from_utf16_lossy(&buf[..size as usize]))
                    == spec.helper_exe.canonicalize().unwrap_or_else(|_| spec.helper_exe.clone())
            })
            .unwrap_or(false);
            let pid = GetProcessId(sei.hProcess);
            let _ = CloseHandle(sei.hProcess);
            if !image_ok {
                return Err(AppError::module(
                    "SYS_HELPER_002",
                    "Helper 镜像路径校验失败（可执行文件被替换？）",
                    None,
                ));
            }
            Ok(pid)
        };
        result.map_err(|e| {
            AppError::module(
                "SYS_HELPER_002",
                format!("提权拉起 Helper 失败（UAC 取消或被策略拦截）: {e}"),
                None,
            )
        })
    }
}

// ---------------------------------------------------------------------------
// 管道客户端（主进程侧）
// ---------------------------------------------------------------------------

/// 连接 helper 管道（helper 启动需要时间：deadline 内重试）
pub fn pipe_connect(pipe_name: &str, timeout_ms: u32) -> Result<HANDLE, AppError> {
    let wide = to_wide(pipe_name);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    loop {
        unsafe {
            match CreateFileW(
                PCWSTR::from_raw(wide.as_ptr()),
                (GENERIC_READ.0 | GENERIC_WRITE.0) as u32,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            ) {
                Ok(h) => return Ok(h),
                Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() => {
                    let _ = WaitNamedPipeW(PCWSTR::from_raw(wide.as_ptr()), 500);
                }
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(80)),
                Err(e) => {
                    return Err(AppError::module(
                        "SYS_HELPER_003",
                        format!("Helper 管道不可用（Helper 未运行？）: {e}"),
                        None,
                    ))
                }
            }
        }
    }
}

fn write_all(h: HANDLE, mut data: &[u8]) -> Result<(), AppError> {
    unsafe {
        while !data.is_empty() {
            let mut n: u32 = 0;
            WriteFile(h, Some(data), Some(&mut n), None)
                .map_err(|e| AppError::module("SYS_HELPER_004", format!("写管道失败: {e}"), None))?;
            if n == 0 {
                return Err(AppError::module("SYS_HELPER_004", "写管道零字节", None));
            }
            data = &data[n as usize..];
        }
    }
    Ok(())
}

fn read_exact(h: HANDLE, mut buf: &mut [u8]) -> Result<(), AppError> {
    unsafe {
        while !buf.is_empty() {
            let mut n: u32 = 0;
            ReadFile(h, Some(buf), Some(&mut n), None)
                .map_err(|e| AppError::module("SYS_HELPER_004", format!("读管道失败: {e}"), None))?;
            if n == 0 {
                return Err(AppError::module("SYS_HELPER_004", "连接已关闭（EOF）", None));
            }
            let written = n as usize;
            buf = &mut buf[written..];
        }
    }
    Ok(())
}

/// 写一帧（[u32 BE len][payload]）
pub fn write_frame(h: HANDLE, payload: &[u8]) -> Result<(), AppError> {
    let mut pkt = Vec::with_capacity(4 + payload.len());
    pkt.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    pkt.extend_from_slice(payload);
    write_all(h, &pkt)
}

/// 读一帧；对端断开返回错误（broken pipe / EOF）
pub fn read_frame(h: HANDLE) -> Result<Vec<u8>, AppError> {
    let mut head = [0u8; 4];
    read_exact(h, &mut head)?;
    let len = u32::from_be_bytes(head) as usize;
    if len > MAX_FRAME {
        return Err(AppError::module("SYS_HELPER_004", format!("帧超上限: {len}"), None));
    }
    let mut out = vec![0u8; len];
    read_exact(h, &mut out)?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// 管道服务端（helper bin 侧；同步阻塞 + 看门狗线程空闲自退）
// ---------------------------------------------------------------------------

/// 创建单实例管道服务端（已存在同名管道 → ACCESS_DENIED 互斥退出）
pub fn pipe_server_create(pipe_name: &str) -> Result<HANDLE, AppError> {
    let wide = to_wide(pipe_name);
    let open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE;
    // PIPE_TYPE_BYTE / PIPE_READMODE_BYTE / PIPE_WAIT 全部为 0（字节模式默认值）
    unsafe {
        let h = CreateNamedPipeW(
            PCWSTR::from_raw(wide.as_ptr()),
            open_mode,
            NAMED_PIPE_MODE(0),
            1, // 单实例（helper 一次服务一个客户端）
            64 * 1024,
            64 * 1024,
            0,
            None,
        );
        if h == INVALID_HANDLE_VALUE {
            let err = windows::Win32::Foundation::GetLastError();
            return Err(AppError::module(
                "SYS_HELPER_005",
                if err == ERROR_ACCESS_DENIED {
                    "同名管道已存在（单实例互斥）".to_string()
                } else {
                    format!("CreateNamedPipeW 失败: {err:?}")
                },
                None,
            ));
        }
        Ok(h)
    }
}

/// 等待客户端连接（阻塞；客户端在调用前已打开 → ERROR_PIPE_CONNECTED 亦视为成功）
pub fn pipe_accept(server: HANDLE) -> Result<(), AppError> {
    unsafe {
        match ConnectNamedPipe(server, None) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
            Err(e) => Err(AppError::module("SYS_HELPER_005", format!("ConnectNamedPipe 失败: {e}"), None)),
        }
    }
}

/// 断开当前客户端（回到 accept 循环前调用）
pub fn pipe_disconnect(server: HANDLE) {
    unsafe {
        let _ = DisconnectNamedPipe(server);
    }
}

/// 连接方的客户端 PID（握手镜像校验用）
pub fn client_pid(h: HANDLE) -> Result<u32, AppError> {
    unsafe {
        let mut pid = 0u32;
        GetNamedPipeClientProcessId(h, &mut pid)
            .map_err(|e| AppError::module("SYS_HELPER_005", format!("GetNamedPipeClientProcessId 失败: {e}"), None))?;
        Ok(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机：当前进程镜像路径查询回环
    #[test]
    fn query_image_path_self() {
        let cur = std::env::current_exe().unwrap();
        let got = query_image_path(std::process::id()).unwrap();
        assert_eq!(got.canonicalize().unwrap(), cur.canonicalize().unwrap());
    }

    /// 同目录判定（大小写不敏感路径经由 canonicalize）
    #[test]
    fn same_dir_check() {
        let cur = std::env::current_exe().unwrap();
        assert!(same_dir(&cur, &cur));
        assert!(!same_dir(&cur, Path::new(r"C:\Windows\explorer.exe")));
    }

    /// 管道名派生格式（规格 §4.1）
    #[test]
    fn pipe_name_format() {
        assert_eq!(pipe_name_for(1234), r"\\.\pipe\nexusforge-sys-helper-v1-1234");
    }
}
