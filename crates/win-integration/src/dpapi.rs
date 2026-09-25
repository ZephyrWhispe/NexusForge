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

/// CRYPT_INTEGER_BLOB 出参统一收口（STD-09）：cbData=0 或 pbData 空指针按
/// FFI 异常如实报错（旧实现直接 from_raw_parts，空输出会构造 UB 切片）；
/// 复制后 LocalFree 归还 DPAPI 分配的缓冲，杜绝泄漏。
fn take_blob_output(out: CRYPT_INTEGER_BLOB, code: &str) -> Result<Vec<u8>, AppError> {
    if out.cbData == 0 || out.pbData.is_null() {
        return Err(AppError::module(
            code,
            "DPAPI 返回空输出（cbData/pbData 非法）",
            None,
        ));
    }
    // SAFETY：cbData > 0 且 pbData 非空，DPAPI 契约保证该缓冲在本进程存活至 LocalFree
    let slice = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) };
    let v = slice.to_vec();
    // SAFETY：pbData 来自 Crypt*Data 的 LocalAlloc，复制完成后必须归还
    unsafe { windows::Win32::Foundation::LocalFree(HLOCAL(out.pbData.cast())) };
    Ok(v)
}

pub struct Dpapi;

impl host_core::ports::CryptoPort for Dpapi {
    fn protect(&self, plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY：to_blob 构造的输入仅在本调用内存活且被 DPAPI 只读；out 为合法
        // 可写结构体指针；三个可选描述参数按 FFI 契约传 None/0。成功后 out.pbData
        // 指向 DPAPI 分配的 LocalAlloc 缓冲（复制后 LocalFree 归还）。
        unsafe {
            CryptProtectData(&to_blob(plaintext), None, None, None, None, 0, &mut out).map_err(
                |e| AppError::module("CLIPBOARD_CRYPTO_001", format!("DPAPI 加密失败: {e}"), None),
            )?;
        }
        take_blob_output(out, "CLIPBOARD_CRYPTO_001")
    }

    fn unprotect(&self, ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY：同 protect——输入只读、out 合法可写；成功后接管 out.pbData。
        unsafe {
            CryptUnprotectData(&to_blob(ciphertext), None, None, None, None, 0, &mut out).map_err(
                |e| AppError::module("CLIPBOARD_CRYPTO_002", format!("DPAPI 解密失败: {e}"), None),
            )?;
        }
        take_blob_output(out, "CLIPBOARD_CRYPTO_002")
    }
}

// PWSTR 描述参数预留说明已删除（windows 0.58 用 PCWSTR/None）
