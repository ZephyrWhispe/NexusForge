//! V6 密码生成器（docs/impl/05 V6）：charclass 组合 + OsRng。
//!
//! 策略：每选中类保证至少 1 个字符，其余从合并池随机；Fisher-Yates 洗牌；
//! 「避免易混淆字符」开关剔除 0/O/1/l/I 与符号中的 | ' ` "。

use rand::rngs::OsRng;
use rand::seq::SliceRandom;
use rand::Rng;

use host_core::error::AppError;

const UPPER: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const DIGITS: &[u8] = b"0123456789";
const SYMBOLS: &[u8] = b"!@#$%^&*()-_=+[]{};:,.<>?/";
/// 易混淆字符（含符号中的竖线/引号/反引号）
const AMBIGUOUS: &[u8] = b"0O1lI|'`\"";

fn err(msg: impl Into<String>) -> AppError {
    AppError::module("VAULT_GEN_001", msg, None)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PasswordPolicy {
    pub length: u8,
    pub upper: bool,
    pub lower: bool,
    pub digits: bool,
    pub symbols: bool,
    pub avoid_ambiguous: bool,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self { length: 16, upper: true, lower: true, digits: true, symbols: true, avoid_ambiguous: false }
    }
}

impl PasswordPolicy {
    fn pools(&self) -> Vec<Vec<u8>> {
        let strip = |pool: &'static [u8]| -> Vec<u8> {
            if self.avoid_ambiguous {
                pool.iter().copied().filter(|c| !AMBIGUOUS.contains(c)).collect()
            } else {
                pool.to_vec()
            }
        };
        let mut pools = Vec::new();
        for (on, base) in [
            (self.upper, UPPER),
            (self.lower, LOWER),
            (self.digits, DIGITS),
            (self.symbols, SYMBOLS),
        ] {
            if on {
                let p = strip(base);
                if !p.is_empty() {
                    pools.push(p);
                }
            }
        }
        pools
    }
}

/// 生成密码：每选中类先各取 1 个保证覆盖，剩余从合并池取，最后整体洗牌
pub fn generate_password(policy: &PasswordPolicy) -> Result<String, AppError> {
    let pools = policy.pools();
    if pools.is_empty() {
        return Err(err("至少启用一个字符类"));
    }
    if policy.length < pools.len() as u8 {
        return Err(err(format!("长度 {} 小于字符类数 {}", policy.length, pools.len())));
    }
    let mut rng = OsRng;
    let mut chars: Vec<u8> = Vec::with_capacity(policy.length as usize);
    for pool in &pools {
        chars.push(pool[rng.gen_range(0..pool.len())]);
    }
    let merged: Vec<u8> = pools.iter().flat_map(|p| p.iter().copied()).collect();
    while chars.len() < policy.length as usize {
        chars.push(merged[rng.gen_range(0..merged.len())]);
    }
    chars.shuffle(&mut rng);
    Ok(String::from_utf8(chars).expect("字符池均为 ASCII"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_of(c: u8) -> &'static str {
        if c.is_ascii_uppercase() {
            "upper"
        } else if c.is_ascii_lowercase() {
            "lower"
        } else if c.is_ascii_digit() {
            "digit"
        } else {
            "symbol"
        }
    }

    #[test]
    fn covers_all_classes_and_respects_length() {
        let policy = PasswordPolicy { length: 64, ..Default::default() };
        let pw = generate_password(&policy).unwrap();
        assert_eq!(pw.len(), 64);
        for want in ["upper", "lower", "digit", "symbol"] {
            assert!(
                pw.bytes().any(|c| class_of(c) == want),
                "必须覆盖 {want}"
            );
        }
    }

    #[test]
    fn avoid_ambiguous_excludes_confusables() {
        let policy = PasswordPolicy { length: 200, avoid_ambiguous: true, ..Default::default() };
        for _ in 0..10 {
            let pw = generate_password(&policy).unwrap();
            assert!(!pw.bytes().any(|c| AMBIGUOUS.contains(&c)), "不得出现易混淆字符: {pw}");
        }
    }

    #[test]
    fn rejects_empty_pool_and_too_short() {
        let p = PasswordPolicy { length: 16, upper: false, lower: false, digits: false, symbols: false, avoid_ambiguous: false };
        assert!(generate_password(&p).is_err());
        // 4 类但长度 3：放不下每类 1 个
        let p = PasswordPolicy { length: 3, ..Default::default() };
        assert!(generate_password(&p).is_err());
    }

    #[test]
    fn single_class_works() {
        let p = PasswordPolicy { length: 32, upper: false, lower: true, digits: false, symbols: false, avoid_ambiguous: false };
        let pw = generate_password(&p).unwrap();
        assert_eq!(pw.len(), 32);
        assert!(pw.bytes().all(|c| c.is_ascii_lowercase()));
    }
}
