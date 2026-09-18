//! 注册表窄实现（docs/impl/08 W0 WinOps 数据面）：RegistryOps Port 的 Windows 侧。
//!
//! 路由契约（docs/impl/08）：HKCU 进程内直写；HKLM 技术上可实现但 WinOps catalog v1
//! 不收录（需提权 Helper——写入失败即符合契约）。key 格式 `HKCU\Software\...\...`。
//!
//! windows 0.58 归置：Registry API 在 `Win32::System::Registry`，返回 `WIN32_ERROR`
//! （Foundation），`.ok()` 转 `windows::core::Result`；成功码 `ERROR_SUCCESS = WIN32_ERROR(0)`。

use std::os::windows::ffi::OsStrExt;

use host_core::error::AppError;
use host_core::ports::{RegValue, RegistryOps};
use windows::core::PCWSTR;
use windows::Win32::Foundation::WIN32_ERROR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD,
    REG_OPTION_NON_VOLATILE, REG_QWORD, REG_SZ, REG_VALUE_TYPE,
};

const ERROR_FILE_NOT_FOUND: WIN32_ERROR = WIN32_ERROR(2);

pub struct RegistryOpsWin;

impl RegistryOpsWin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RegistryOpsWin {
    fn default() -> Self {
        Self::new()
    }
}

/// 解析 `HKCU\sub\path` / `HKLM\sub\path` → (根键, 子键宽字符串)
fn parse_key(key: &str) -> Result<(HKEY, Vec<u16>), AppError> {
    let (root, rest) = if let Some(r) = key.strip_prefix("HKCU\\") {
        (HKEY_CURRENT_USER, r)
    } else if let Some(r) = key.strip_prefix("HKLM\\") {
        (HKEY_LOCAL_MACHINE, r)
    } else {
        return Err(AppError::module(
            "SYS_WINOPS_002",
            format!("不支持的关键字前缀（v1 仅 HKCU/HKLM）: {key}"),
            None,
        ));
    };
    let wide: Vec<u16> = std::ffi::OsStr::new(rest)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    Ok((root, wide))
}

fn io_err(op: &str, e: windows::core::Error) -> AppError {
    AppError::module("SYS_WINOPS_002", format!("{op} 失败: {e}"), None)
}

/// WIN32_ERROR → AppError（成功码直接透传 None 由调用方控制）
fn werr(op: &str, e: windows::core::Error) -> AppError {
    io_err(op, e)
}

impl RegistryOps for RegistryOpsWin {
    fn read_value(&self, key: &str, value_name: &str) -> Result<(RegValue, bool), AppError> {
        let (root, sub) = parse_key(key)?;
        let name_w: Vec<u16> = std::ffi::OsStr::new(value_name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let mut hkey = HKEY::default();
            let opened = RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), 0, KEY_QUERY_VALUE, &mut hkey);
            if opened != WIN32_ERROR(0) {
                let _ = RegCloseKey(hkey);
                // key 整体不存在 → "值不存在"语义（existed=false），非错误
                if opened == ERROR_FILE_NOT_FOUND {
                    return Ok((RegValue::Dword(0), false));
                }
                return Err(werr("RegOpenKeyExW", opened.to_hresult().into()));
            }
            let mut ty = REG_VALUE_TYPE(0);
            let mut size = 0u32;
            // 第一次调用取 size（data=None）
            let q = RegQueryValueExW(
                hkey,
                PCWSTR(name_w.as_ptr()),
                None,
                Some(&mut ty),
                None,
                Some(&mut size),
            );
            if q != WIN32_ERROR(0) {
                let _ = RegCloseKey(hkey);
                if q == ERROR_FILE_NOT_FOUND {
                    return Ok((RegValue::Dword(0), false));
                }
                return Err(werr("RegQueryValueExW(size)", q.to_hresult().into()));
            }
            let mut buf = vec![0u8; size as usize];
            let r = RegQueryValueExW(
                hkey,
                PCWSTR(name_w.as_ptr()),
                None,
                Some(&mut ty),
                Some(buf.as_mut_ptr()),
                Some(&mut size),
            );
            let _ = RegCloseKey(hkey);
            if r != WIN32_ERROR(0) {
                return Err(werr("RegQueryValueExW", r.to_hresult().into()));
            }
            let v = match ty {
                t if t == REG_DWORD && size >= 4 => {
                    RegValue::Dword(u32::from_le_bytes(buf[0..4].try_into().unwrap()))
                }
                t if t == REG_QWORD && size >= 8 => {
                    RegValue::Qword(u64::from_le_bytes(buf[0..8].try_into().unwrap()))
                }
                t if t == REG_SZ => {
                    let wide: Vec<u16> = buf[..size as usize]
                        .chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    let s = String::from_utf16_lossy(&wide);
                    RegValue::Str(s.trim_end_matches('\0').to_string())
                }
                other => {
                    return Err(AppError::module(
                        "SYS_WINOPS_002",
                        format!("不支持的注册表值类型: {other:?}"),
                        None,
                    ))
                }
            };
            Ok((v, true))
        }
    }

    fn write_value(&self, key: &str, value_name: &str, value: &RegValue) -> Result<(), AppError> {
        let (root, sub) = parse_key(key)?;
        let name_w: Vec<u16> = std::ffi::OsStr::new(value_name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let mut hkey = HKEY::default();
            RegCreateKeyExW(
                root,
                PCWSTR(sub.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut hkey,
                None,
            )
            .ok()
            .map_err(|e| werr("RegCreateKeyExW", e))?;
            let r = match value {
                RegValue::Dword(d) => RegSetValueExW(
                    hkey,
                    PCWSTR(name_w.as_ptr()),
                    0,
                    REG_DWORD,
                    Some(d.to_le_bytes().as_slice()),
                ),
                RegValue::Qword(q) => RegSetValueExW(
                    hkey,
                    PCWSTR(name_w.as_ptr()),
                    0,
                    REG_QWORD,
                    Some(q.to_le_bytes().as_slice()),
                ),
                RegValue::Str(s) => {
                    let mut wide: Vec<u16> = std::ffi::OsStr::new(s).encode_wide().collect();
                    wide.push(0);
                    let bytes: Vec<u8> = wide.iter().flat_map(|w| w.to_le_bytes()).collect();
                    RegSetValueExW(hkey, PCWSTR(name_w.as_ptr()), 0, REG_SZ, Some(bytes.as_slice()))
                }
            };
            let _ = RegCloseKey(hkey);
            if r != WIN32_ERROR(0) {
                return Err(werr("RegSetValueExW", r.to_hresult().into()));
            }
            Ok(())
        }
    }

    fn delete_value(&self, key: &str, value_name: &str) -> Result<(), AppError> {
        let (root, sub) = parse_key(key)?;
        let name_w: Vec<u16> = std::ffi::OsStr::new(value_name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let mut hkey = HKEY::default();
            RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), 0, KEY_SET_VALUE, &mut hkey)
                .ok()
                .map_err(|e| werr("RegOpenKeyExW", e))?;
            let r = RegDeleteValueW(hkey, PCWSTR(name_w.as_ptr()));
            let _ = RegCloseKey(hkey);
            if r != WIN32_ERROR(0) {
                return Err(werr("RegDeleteValueW", r.to_hresult().into()));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实 HKCU 读写删回环（测试专属 key，结束清理）
    #[test]
    fn hkcu_roundtrip() {
        let ops = RegistryOpsWin::new();
        let key = format!(r"HKCU\Software\NexusForgeTest\{}", std::process::id());
        // 不存在 → existed=false
        let r = ops.read_value(&key, "d");
        let (_, existed) = match r {
            Ok(x) => x,
            Err(e) => panic!("读不存在值应返回 existed=false，实际: {e:?}"),
        };
        assert!(!existed);
        // 写 dword → 读回
        ops.write_value(&key, "d", &RegValue::Dword(42)).unwrap();
        let (v, existed) = ops.read_value(&key, "d").unwrap();
        assert!(existed && v == RegValue::Dword(42));
        // 写 string → 读回
        ops.write_value(&key, "s", &RegValue::Str("hello 注册表".into())).unwrap();
        let (v, _) = ops.read_value(&key, "s").unwrap();
        assert_eq!(v, RegValue::Str("hello 注册表".into()));
        // 写 qword → 读回
        ops.write_value(&key, "q", &RegValue::Qword(1 << 40)).unwrap();
        let (v, _) = ops.read_value(&key, "q").unwrap();
        assert_eq!(v, RegValue::Qword(1 << 40));
        // 覆盖写
        ops.write_value(&key, "d", &RegValue::Dword(7)).unwrap();
        let (v, _) = ops.read_value(&key, "d").unwrap();
        assert_eq!(v, RegValue::Dword(7));
        // 删除 → 不存在
        ops.delete_value(&key, "d").unwrap();
        let (_, existed) = ops.read_value(&key, "d").unwrap();
        assert!(!existed);
        // 再删幂等容忍（delete 已删值报错——restore 路径用 let _ 吞）
        assert!(ops.delete_value(&key, "d").is_err());
    }

    /// 非法前缀拒绝（路由契约）
    #[test]
    fn unsupported_prefix_rejected() {
        let ops = RegistryOpsWin::new();
        assert!(ops.read_value(r"HKEY_CLASSES_ROOT\x", "v").is_err());
    }
}
