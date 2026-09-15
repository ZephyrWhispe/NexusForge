//! DPAPI 本机数据加密（docs/impl/02 C4；Windows 原生 API，用户偏好系统原生能力）

use windows::Win32::Foundation::HLOCAL;
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
};

use host_core::error::AppError;

fn to_blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    }
}

pub struct Dpapi;

impl host_core::ports::CryptoPort for Dpapi {
    fn protect(&self, plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(
                &to_blob(plaintext),
                None,
                None,
                None,
                None,
                0,
                &mut out,
            )
            .map_err(|e| {
                AppError::module("CLIPBOARD_CRYPTO_001", format!("DPAPI 加密失败: {e}"), None)
            })?;
        }
        // 接管 out.pbData 指针的所有权（LocalFree 由 Vec 回收策略替代：Rust 侧复制后立即释放）
        let slice = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) };
        let v = slice.to_vec();
        unsafe { windows::Win32::Foundation::LocalFree(HLOCAL(out.pbData.cast())) };
        Ok(v)
    }

    fn unprotect(&self, ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(&to_blob(ciphertext), None, None, None, None, 0, &mut out)
                .map_err(|e| {
                    AppError::module("CLIPBOARD_CRYPTO_002", format!("DPAPI 解密失败: {e}"), None)
                })?;
        }
        let slice = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) };
        let v = slice.to_vec();
        unsafe { windows::Win32::Foundation::LocalFree(HLOCAL(out.pbData.cast())) };
        Ok(v)
    }
}

// PWSTR 描述参数预留说明已删除（windows 0.58 用 PCWSTR/None）
