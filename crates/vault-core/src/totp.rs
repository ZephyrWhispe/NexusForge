//! V7 TOTP 计算核心（docs/impl/05 V7）：RFC 6238，HMAC-SHA1，30s 步长。
//!
//! 进度环形条由前端 rAF 用剩余秒渲染，core 只负责算码与剩余时间。

use hmac::{Hmac, Mac};
use sha1::Sha1;

use host_core::error::AppError;

/// 步长（RFC 6238 默认）
pub const STEP_SECS: u64 = 30;

fn err(msg: impl Into<String>) -> AppError {
    AppError::module("VAULT_TOTP_001", msg, None)
}

/// RFC 4648 base32 解码（大小写不敏感，忽略空格与 `=` 填充）
fn b32_decode(s: &str) -> Result<Vec<u8>, AppError> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let mut bits: u32 = 0;
    let mut acc: u32 = 0;
    for ch in s.bytes() {
        let c = ch.to_ascii_uppercase();
        if c == b'=' || c == b' ' || c == b'-' {
            continue;
        }
        let v = ALPHABET
            .iter()
            .position(|&a| a == c)
            .ok_or_else(|| err(format!("base32 非法字符: {}", ch as char)))? as u32;
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if out.is_empty() {
        return Err(err("base32 密钥为空"));
    }
    Ok(out)
}

/// 计算指定时刻的 TOTP：返回 (6 位码, 距下一次滚动的剩余秒)
pub fn totp_at(secret_b32: &str, unix_secs: u64) -> Result<(String, u64), AppError> {
    let key = b32_decode(secret_b32)?;
    let counter = unix_secs / STEP_SECS;
    let mut mac = Hmac::<Sha1>::new_from_slice(&key).map_err(|e| err(e.to_string()))?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    // RFC 4226 动态截断
    let offset = (digest[19] & 0x0f) as usize;
    let code = u32::from_be_bytes([digest[offset], digest[offset + 1], digest[offset + 2], digest[offset + 3]])
        & 0x7fff_ffff;
    let otp = format!("{:06}", code % 1_000_000);
    let remaining = STEP_SECS - unix_secs % STEP_SECS;
    Ok((otp, remaining))
}

/// 当前时刻的 TOTP
pub fn totp_now(secret_b32: &str) -> Result<(String, u64), AppError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| err(e.to_string()))?
        .as_secs();
    totp_at(secret_b32, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 测试密钥 "12345678901234567890"（ASCII）的 base32
    const RFC_KEY: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    /// RFC 6238 附录 B（SHA-1）8 位向量取低 6 位
    #[test]
    fn rfc6238_vectors() {
        for (t, expect) in [
            (59u64, "287082"),
            (1111111109, "081804"),
            (1111111111, &"14050471"[2..]), // 8 位向量取末 6 位
            (1234567890, &"89005924"[2..]),
            (2000000000, &"69279037"[2..]),
        ] {
            let (otp, remaining) = totp_at(RFC_KEY, t).unwrap();
            let expect6 = &expect[expect.len() - 6..];
            assert_eq!(otp, expect6, "t={t}");
            assert_eq!(remaining, STEP_SECS - t % STEP_SECS);
        }
    }

    #[test]
    fn rejects_invalid_base32() {
        assert!(totp_at("!!!", 0).is_err());
        assert!(totp_at("", 0).is_err());
    }
}
