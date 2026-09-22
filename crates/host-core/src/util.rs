//! 全工作区公共工具收敛（D-16）：时间戳 / hex / base64 / 统一错误工厂。
//!
//! 收敛前实测：`now_ms` 同名定义 14 份 + 内联拷贝、`(code, msg)` 错误工厂
//! 10+ 份逐字复制、STANDARD base64 编解码 4 处手写——本模块是唯一出处。

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;

use crate::error::AppError;

/// UTC 毫秒时间戳（全仓唯一时间源；时钟回拨等异常归零而非 panic）
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// u64 毫秒时间戳（协议/排序字段用；负值饱和为 0）
pub fn now_ms_u64() -> u64 {
    now_ms().max(0) as u64
}

/// 小写十六进制编码（SHA 摘要展示、指纹等）
pub fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 标准 base64（`+`/`/` 字母表、`=` 填充）——跨设备传输字段的统一编码
pub fn b64_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// 标准 base64 解码，失败返回 None（不区分非法字母表与长度错误）
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// 宽松 base64 解码：先按 URL_SAFE_NO_PAD（订阅链接段），再按 STANDARD。
/// 仅用于解码外部输入（编码端必须显式选定字母表，不留歧义）。
pub fn b64_decode_lenient(s: &str) -> Option<Vec<u8>> {
    let url = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok();
    url.or_else(|| b64_decode(s))
}

/// 模块类 AppError 统一构造（错误码保留，hint 省略）
pub fn app_err(code: &str, msg: impl std::fmt::Display) -> AppError {
    AppError::module(code, msg.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_ms_monotonic_and_epoch_sane() {
        let a = now_ms();
        let b = now_ms_u64();
        // 2024-01-01 之后的毫秒时间戳（本项目启动于 2026）
        assert!(a > 1_700_000_000_000, "now_ms 应为 epoch 毫秒: {a}");
        // 判据是"同一条时间线"而非"同一毫秒"：now_ms_u64 内部再调一次 now_ms()，
        // 两次独立读墙钟恰好跨毫秒边界就会让严格相等判红（本仓首轮 workspace 跑实测相差 1ms）。
        assert!(
            b.abs_diff(a.max(0) as u64) <= 5,
            "两枚读时钟入口须相近（相差 {}ms）",
            b.abs_diff(a.max(0) as u64)
        );
        assert!(now_ms() >= a, "毫秒时间戳不应回退");
    }

    #[test]
    fn hex_lower_matches_known_vector() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(hex_lower(&[]), "");
    }

    #[test]
    fn b64_roundtrip_and_lenient() {
        let data = [0xde, 0xad, 0xbe, 0xef];
        let std_b64 = b64_encode(&data);
        assert_eq!(b64_decode(&std_b64).as_deref(), Some(&data[..]));
        // lenient 必须覆盖 STANDARD（含 padding）
        assert_eq!(b64_decode_lenient(&std_b64).as_deref(), Some(&data[..]));
        // lenient 接受 URL_SAFE_NO_PAD 段（`?`/`~` 字节使两种字母表可区分）
        let url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0xfb, 0xff, 0xfe_u8]);
        assert_eq!(b64_decode_lenient(&url), Some(vec![0xfb, 0xff, 0xfe]));
        // 垃圾输入统一 None
        assert!(b64_decode("!!not base64!!").is_none());
        assert!(b64_decode_lenient("!!not base64!!").is_none());
    }

    #[test]
    fn app_err_keeps_code_and_stringifies_msg() {
        let e = app_err("DEMO_001", format!("第 {} 次失败", 3));
        assert_eq!(e.code(), "DEMO_001");
        assert_eq!(e.to_string(), "第 3 次失败");
    }
}
