//! C5 智能分组分类器（docs/impl/02 C5）
//!
//! 规则按优先级顺序执行，首个 confidence ≥ 0.85 即返回；
//! >10KB 长文本跳过 Json/Code 规则（防正则回溯卡顿）。

pub struct Classifier;

const ACCEPT_THRESHOLD: f32 = 0.85;
const LONG_TEXT_SKIP: usize = 10 * 1024;

impl Classifier {
    /// 返回 (分组, 置信度)；None = 普通文本不分组
    pub fn classify(text: &str) -> Option<(&'static str, f32)> {
        let t = text.trim();
        if t.is_empty() {
            return None;
        }
        // Secret(0.99) > Url(0.95) > Json(0.9) > Color(0.9) > Code(0.7) > Plain
        if crate::secrets::global().inspect(t).is_some() {
            return Some(("secret", 0.99));
        }
        if t.starts_with("http://") || t.starts_with("https://") {
            return Some(("url", 0.95));
        }
        if t.len() <= LONG_TEXT_SKIP {
            let first = t.chars().next().unwrap();
            if (first == '{' || first == '[')
                && serde_json::from_str::<serde_json::Value>(t).is_ok()
            {
                return Some(("json", 0.9));
            }
            if is_color_literal(t) {
                return Some(("color", 0.9));
            }
            let (score, matched) = code_signal(t);
            if score >= ACCEPT_THRESHOLD && matched {
                return Some(("code", score));
            }
        }
        None
    }
}

fn is_color_literal(t: &str) -> bool {
    let body = t.strip_prefix('#').unwrap_or(t);
    (body.len() == 6 || body.len() == 3) && body.chars().all(|c| c.is_ascii_hexdigit())
}

/// 代码启发式：关键词 + 符号密度（不引入语法分析，docs/impl/02 C5 潜在问题）
fn code_signal(t: &str) -> (f32, bool) {
    const KEYWORDS: [&str; 10] = [
        "fn ", "def ", "class ", "import ", "const ", "let ", "function", "=>", "::", "&&",
    ];
    let hits = KEYWORDS.iter().filter(|k| t.contains(**k)).count();
    if hits == 0 {
        return (0.0, false);
    }
    let symbols = t
        .chars()
        .filter(|c| matches!(c, '{' | '}' | '(' | ')' | ';' | '=' | '<' | '>'))
        .count();
    let density = symbols as f32 / t.len() as f32;
    let score = 0.5 * (hits as f32 / 2.0).min(1.0) + 0.5 * (density / 0.16).min(1.0);
    (score, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_url_json_color_code() {
        assert_eq!(Classifier::classify("https://learn.microsoft.com/"), Some(("url", 0.95)));
        assert_eq!(
            Classifier::classify(r#"{"a": 1, "b": [2, 3]}"#),
            Some(("json", 0.9))
        );
        assert_eq!(Classifier::classify("#4CC2FF"), Some(("color", 0.9)));
        assert!(matches!(
            Classifier::classify("let handle = tokio::spawn(f());"),
            Some(("code", _))
        ));
    }

    #[test]
    fn plain_text_and_secrets() {
        assert_eq!(Classifier::classify("今天下午三点评审会改到四点。"), None);
        assert_eq!(Classifier::classify("sk-abcdefghijklmnopqrstuvwxyz123456"), Some(("secret", 0.99)));
    }
}
