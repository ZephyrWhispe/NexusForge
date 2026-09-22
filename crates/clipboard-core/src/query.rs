//! 搜索语法解析（C6 / 09 §8.2 T-B3-6）：把 `group:` / `type:` 两个前缀从自由文本里
//! 分出去。切分保守到"只认这两个前缀"——把用户想搜的内容误当成语法吃掉，比漏吃
//! 一次过滤更难事后发现（查询结果悄悄变了，输入框里看到的还是自己打的那句）。

/// `type:` 的三档取值，与 `clip_entries.content_type` 列的取值域同一份事实。
pub const CONTENT_TYPES: [&str; 3] = ["text", "image", "files"];

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedSyntax {
    /// 去掉语法 token 后剩余的字面查询串（按原顺序、单空格重连）。
    pub text: String,
    pub group: Option<String>,
    pub content_type: Option<String>,
    /// 出现过 `type:<三档之外>`：调用方据此给出**零命中**，而不是"忽略该条件返回全部"。
    pub unknown_type: bool,
}

pub fn parse_search_syntax(raw: &str) -> ParsedSyntax {
    let mut out = ParsedSyntax::default();
    let mut literal: Vec<String> = Vec::new();
    for token in tokenize(raw) {
        let Some((key, rest)) = split_prefix(&token) else {
            literal.push(token);
            continue;
        };
        match key {
            "group" => {
                let value = strip_quotes(rest);
                if value.is_empty() {
                    // `group:` 后没内容 = 不构成筛选意图，按字面搜；
                    // 若当成"匹配空组名"会把整张列表滤成幽灵条件。
                    literal.push(token);
                } else {
                    out.group = Some(value.to_string());
                }
            }
            "type" => {
                let value = strip_quotes(rest);
                if CONTENT_TYPES.contains(&value) {
                    out.content_type = Some(value.to_string());
                } else {
                    out.unknown_type = true;
                }
            }
            _ => literal.push(token),
        }
    }
    out.text = literal.join(" ");
    out
}

/// `key:value` 形态识别。URL 型 token（含 `://`）不是语法：`http://a:b/c` 的
/// `http` 恰好落在第一个冒号左侧，若只按首个冒号切就会把协议名当成前缀键。
fn split_prefix(token: &str) -> Option<(&str, &str)> {
    if token.contains("://") {
        return None;
    }
    let (key, rest) = token.split_once(':')?;
    if key.is_empty() || rest.is_empty() {
        return None;
    }
    Some((key, rest))
}

/// 引号感知切分：引号内的空白属于同一个 token，未闭合的引号把剩余内容全收进当前
/// token（宁可少切一刀，也不把 `group:"` 后面的词拆成独立条件）。
fn tokenize(raw: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in raw.chars() {
        if ch == '"' {
            quoted = !quoted;
            cur.push(ch);
        } else if !quoted && ch.is_whitespace() {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

fn strip_quotes(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
        .trim()
}

/// 需要转义的正则元字符（与前端 `toLiteralRegex` 逐字符同表，见 queryEscapes 两侧对照测试）。
const REGEX_META: [char; 14] = [
    '\\', '^', '$', '.', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|',
];

/// 字面量转正则（09 §8.2 T-B3-7「永不入库」）：把用户看到的原文变成只匹配它自身的模式。
///
/// 本函数与前端同名实现是**跨语言契约的两端**：设置卡里显示的那条规则必须就是实际生效的
/// 那条，所以两侧各自实现、由两侧测试断言同一份期望字面串（任何一侧改动都会让另一侧的测试变红）。
/// 刻意不加 `^…$` 锚：命中子串即算屏蔽——锚定会让"同一段内容换了个前后缀再来一次"静默漏过。
pub fn to_literal_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if REGEX_META.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    /// 「永不入库」生成的定串规则（09 §8.2 T-B3-7）：期望字面串与前端 `toLiteralRegex`
    /// 的同名测试逐字符一致——两侧各持一份实现，靠同一份期望串钉住跨语言契约。
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-7）字面测试名优先于 rustc 命名惯例
    fn secretRow_neverStore_patternEscapesLiterals() {
        let raw = "sk-abc.1(key)|x";
        let pat = to_literal_regex(raw);
        assert_eq!(pat, r"sk-abc\.1\(key\)\|x");
        let re = Regex::new(&pat).expect("转义结果必须是合法正则");
        assert!(re.is_match(raw), "转义后仍须命中原文自身（正对照）");
        // 不误伤：元字符位换成任意其它字符都不得命中
        assert!(!re.is_match("sk-abcX1keyxx|x"));
        assert!(!re.is_match("sk-abc.1keyxx"));
        assert!(!re.is_match("sk-abc.1(key)x"));
        // 空串与纯中文原样通过（无元字符即不插反斜杠）
        assert_eq!(to_literal_regex("验证码 123456"), "验证码 123456");
    }

    #[test]
    #[allow(non_snake_case)] // 本模块命名随任务书（09 §8.2 T-B3-6）风格
    fn parseSyntax_urlToken_staysLiteralText() {
        let p = parse_search_syntax("看 http://a:b/c 这个链接");
        assert_eq!(p.group, None);
        assert_eq!(p.content_type, None);
        assert!(!p.unknown_type);
        assert_eq!(p.text, "看 http://a:b/c 这个链接");
    }

    #[test]
    #[allow(non_snake_case)] // 本模块命名随任务书（09 §8.2 T-B3-6）风格
    fn parseSyntax_quotedGroupKeepsSpaces() {
        let p = parse_search_syntax(r#"group:"工作 笔记" 需求"#);
        assert_eq!(p.group.as_deref(), Some("工作 笔记"));
        assert_eq!(p.text, "需求");
    }

    #[test]
    #[allow(non_snake_case)] // 本模块命名随任务书（09 §8.2 T-B3-6）风格
    fn parseSyntax_unknownPrefixAndEmptyValueStayLiteral() {
        let p = parse_search_syntax("foo:bar group: type: 其余");
        assert_eq!(p.group, None);
        assert_eq!(p.content_type, None);
        assert!(
            !p.unknown_type,
            "空 type: 值不进 unknown 分支：它连类型条件都没构成"
        );
        assert_eq!(p.text, "foo:bar group: type: 其余");
    }
}
