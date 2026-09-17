//! 系统代理设置（docs/impl/05 PR4）：HKCU `...\Internet Settings` 读写 + WinINET 广播。
//!
//! 高危语义（docs/impl/05 风险标注）：本实现写入系统代理后，任何异常退出都必须还原
//! —— host-core crash hook / ProxyModule::stop / `--restore-proxy` 启动参数三处调用方负责。

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Networking::WinInet::{
    InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use host_core::error::AppError;
use host_core::ports::{SysProxyPort, SysProxyState};

const KEY_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";

pub struct WindowsSysProxy;

fn err(op: &str, code: u32) -> AppError {
    AppError::module(
        "PROXY_SYS_001",
        format!("系统代理{op}失败（Win32 {code}）"),
        Some("若为拒绝访问，请检查安全软件或以管理员运行"),
    )
}

/// 读 REG_DWORD（缺省 0）
unsafe fn read_dword(key: HKEY, name: &str) -> Result<u32, AppError> {
    let name_h = HSTRING::from(name);
    let mut ty = REG_VALUE_TYPE::default();
    let mut buf = [0u8; 4];
    let mut len = buf.len() as u32;
    let code = RegQueryValueExW(
        key,
        &name_h,
        None,
        Some(&mut ty),
        Some(buf.as_mut_ptr()),
        Some(&mut len),
    );
    if code.is_err() {
        return Ok(0); // 值不存在 = 未启用，视为默认
    }
    Ok(u32::from_ne_bytes(buf))
}

/// 读 REG_SZ（缺省空串）
unsafe fn read_sz(key: HKEY, name: &str) -> Result<String, AppError> {
    let name_h = HSTRING::from(name);
    let mut ty = REG_VALUE_TYPE::default();
    let mut buf = [0u8; 2048];
    let mut len = buf.len() as u32;
    let code = RegQueryValueExW(
        key,
        &name_h,
        None,
        Some(&mut ty),
        Some(buf.as_mut_ptr()),
        Some(&mut len),
    );
    if code.is_err() {
        return Ok(String::new());
    }
    let n = (len as usize / 2).min(1024);
    let wide = &buf[..n * 2];
    let u16s: Vec<u16> = wide
        .chunks_exact(2)
        .map(|c| u16::from_ne_bytes([c[0], c[1]]))
        .collect();
    Ok(String::from_utf16_lossy(
        &u16s.iter().copied().take_while(|&c| c != 0).collect::<Vec<_>>(),
    ))
}

impl SysProxyPort for WindowsSysProxy {
    fn read(&self) -> Result<SysProxyState, AppError> {
        unsafe {
            let path_h = HSTRING::from(KEY_PATH);
            let mut key = HKEY::default();
            let open = RegOpenKeyExW(HKEY_CURRENT_USER, &path_h, 0, KEY_QUERY_VALUE, &mut key);
            if open.is_err() {
                return Err(err("注册表读取", open.0));
            }
            let res = (|| {
                Ok(SysProxyState {
                    enable: read_dword(key, "ProxyEnable")? != 0,
                    server: read_sz(key, "ProxyServer")?,
                    bypass: read_sz(key, "ProxyOverride")?,
                })
            })();
            let _ = RegCloseKey(key);
            res
        }
    }

    fn write(&self, state: &SysProxyState) -> Result<(), AppError> {
        unsafe {
            let path_h = HSTRING::from(KEY_PATH);
            let mut key = HKEY::default();
            let open = RegOpenKeyExW(HKEY_CURRENT_USER, &path_h, 0, KEY_SET_VALUE, &mut key);
            if open.is_err() {
                return Err(err("注册表写入", open.0));
            }
            let set_dword = RegSetValueExW(
                key,
                w!("ProxyEnable"),
                0,
                REG_DWORD,
                Some(&(state.enable as u32).to_ne_bytes()),
            );
            let server_utf16: Vec<u16> = state.server.encode_utf16().chain([0]).collect();
            let mut server_bytes = Vec::with_capacity(server_utf16.len() * 2);
            for v in &server_utf16 {
                server_bytes.extend_from_slice(&v.to_ne_bytes());
            }
            let set_server = RegSetValueExW(key, w!("ProxyServer"), 0, REG_SZ, Some(&server_bytes));
            let bypass_utf16: Vec<u16> = state.bypass.encode_utf16().chain([0]).collect();
            let mut bypass_bytes = Vec::with_capacity(bypass_utf16.len() * 2);
            for v in &bypass_utf16 {
                bypass_bytes.extend_from_slice(&v.to_ne_bytes());
            }
            let set_bypass = RegSetValueExW(key, w!("ProxyOverride"), 0, REG_SZ, Some(&bypass_bytes));
            let _ = RegCloseKey(key);
            if set_dword.is_err() {
                return Err(err("ProxyEnable 写入", set_dword.0));
            }
            if set_server.is_err() {
                return Err(err("ProxyServer 写入", set_server.0));
            }
            if set_bypass.is_err() {
                return Err(err("ProxyOverride 写入", set_bypass.0));
            }
            Ok(())
        }
    }

    fn refresh(&self) -> Result<(), AppError> {
        // 广播让 WinINET/应用感知变更（无需句柄，全局生效）
        unsafe {
            InternetSetOptionW(None, INTERNET_OPTION_SETTINGS_CHANGED, None, 0)
                .map_err(|e| AppError::module("PROXY_SYS_002", format!("SETTINGS_CHANGED 广播失败: {e}"), None))?;
            InternetSetOptionW(None, INTERNET_OPTION_REFRESH, None, 0)
                .map_err(|e| AppError::module("PROXY_SYS_002", format!("REFRESH 广播失败: {e}"), None))?;
        }
        Ok(())
    }

    fn is_admin(&self) -> bool {
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut elev = TOKEN_ELEVATION::default();
            let mut ret = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut elev as *mut _ as *mut _),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret,
            )
            .is_ok()
                && elev.TokenIsElevated != 0;
            let _ = CloseHandle(token);
            ok
        }
    }
}
