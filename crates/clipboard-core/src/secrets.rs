//! C4 敏感数据保护（docs/impl/02 C4）
//!
//! 规则链按序短路命中；命中内容经 CryptoPort(DPAPI) 加密后入库，
//! preview 永远为遮蔽文案，**禁止**明文进日志（tracing 事件同理）。

use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    PrivateKey,
    Jwt,
    ApiKey,
    CreditCard,
    IdNumber,
    PhoneNumber,
}

impl SecretKind {
    pub fn label(&self) -> &'static str {
        match self {
            SecretKind::PrivateKey => "私钥",
            SecretKind::Jwt => "JWT",
            SecretKind::ApiKey => "API Key",
            SecretKind::CreditCard => "银行卡",
            SecretKind::IdNumber => "身份证",
            SecretKind::PhoneNumber => "手机号",
        }
    }
}

pub struct SecretFilter {
    rules: Vec<(SecretKind, Regex)>,
}

impl Default for SecretFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretFilter {
    pub fn new() -> Self {
        let rules = vec![
            (
                SecretKind::PrivateKey,
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----".to_string(),
            ),
            (SecretKind::Jwt, r"^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.".to_string()),
            (SecretKind::ApiKey, r"sk-[A-Za-z0-9]{20,}".to_string()),
            (SecretKind::ApiKey, r"ghp_[A-Za-z0-9]{36}".to_string()),
            (SecretKind::ApiKey, r"AKIA[0-9A-Z]{16}".to_string()),
            // 仅当整条内容就是手机号时才判敏感（避免误伤普通文本）
            (SecretKind::PhoneNumber, r"^1[3-9]\d{9}$".to_string()),
            (SecretKind::IdNumber, r"^\d{17}[\dXx]$".to_string()),
            (SecretKind::CreditCard, r"^\d{13,19}$".to_string()),
        ];
        let rules = rules
            .into_iter()
            .filter_map(|(k, p)| Regex::new(&p).ok().map(|r| (k, r)))
            .collect();
        Self { rules }
    }

    /// 返回命中类别；None = 非敏感。规则按序短路。
    pub fn inspect(&self, text: &str) -> Option<SecretKind> {
        let trimmed = text.trim();
        for (kind, re) in &self.rules {
            let strong = matches!(kind, SecretKind::PrivateKey | SecretKind::Jwt | SecretKind::ApiKey);
            let hit = if strong { re.is_match(trimmed) } else { re.is_match(trimmed) };
            if hit {
                // 数字类需二次校验（Luhn / 身份证校验位），防误伤
                match kind {
                    SecretKind::CreditCard => {
                        let digits: String =
                            trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
                        if luhn_valid(&digits) {
                            return Some(*kind);
                        }
                    }
                    SecretKind::IdNumber => {
                        if id_checksum_valid(trimmed) {
                            return Some(*kind);
                        }
                    }
                    _ => return Some(*kind),
                }
            }
        }
        None
    }
}

static FILTER: OnceLock<SecretFilter> = OnceLock::new();
pub fn global() -> &'static SecretFilter {
    FILTER.get_or_init(SecretFilter::new)
}

/// Luhn 校验（银行卡）
pub fn luhn_valid(digits: &str) -> bool {
    if digits.len() < 13 {
        return false;
    }
    let mut sum = 0;
    let mut dbl = false;
    for c in digits.chars().rev() {
        let mut d = match c.to_digit(10) {
            Some(d) => d,
            None => return false,
        };
        if dbl {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
        dbl = !dbl;
    }
    sum % 10 == 0
}

/// 18 位身份证校验位验证
pub fn id_checksum_valid(id: &str) -> bool {
    const WEIGHTS: [u32; 17] = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    const CHECK: [char; 11] = ['1', '0', 'X', '9', '8', '7', '6', '5', '4', '3', '2'];
    let chars: Vec<char> = id.chars().collect();
    if chars.len() != 18 {
        return false;
    }
    let sum: u32 = chars[..17]
        .iter()
        .enumerate()
        .map(|(i, c)| c.to_digit(10).unwrap_or(0) * WEIGHTS[i])
        .sum();
    CHECK[(sum % 11) as usize].eq_ignore_ascii_case(&chars[17])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_api_key() {
        assert_eq!(global().inspect("sk-abcdefghijklmnopqrstuvwxyz123456"), Some(SecretKind::ApiKey));
        assert_eq!(global().inspect("ghp_abcdefghijklmnopqrstuvwxyz0123456789"), Some(SecretKind::ApiKey));
    }

    #[test]
    fn detects_pem_and_jwt() {
        assert_eq!(
            global().inspect("-----BEGIN RSA PRIVATE KEY-----"),
            Some(SecretKind::PrivateKey)
        );
        assert_eq!(
            global().inspect("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abc"),
            Some(SecretKind::Jwt)
        );
    }

    #[test]
    fn luhn_guards_credit_card() {
        // Luhn 合法卡号（测试号段）
        assert_eq!(global().inspect("4111111111111111"), Some(SecretKind::CreditCard));
        // 数字串但 Luhn 不合法 → 非敏感
        assert_eq!(global().inspect("1234567890123456"), None);
    }

    #[test]
    fn phone_only_exact_match() {
        assert_eq!(global().inspect("13812345678"), Some(SecretKind::PhoneNumber));
        assert_eq!(global().inspect("电话 13812345678 记录"), None);
    }

    #[test]
    fn normal_text_not_sensitive() {
        assert_eq!(global().inspect("设计原则：能用 Windows API 就不用跨平台抽象"), None);
        assert_eq!(global().inspect("https://learn.microsoft.com/windows/"), None);
    }
}
