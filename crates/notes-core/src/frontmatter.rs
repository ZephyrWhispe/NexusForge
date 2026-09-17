//! 极简 frontmatter 解析与双链/标签提取（docs/impl/06 N1/N2）。
//!
//! 不引入完整 YAML 依赖：仅支持 `key: value` 与 `tags: [a, b]` / `tags:` 下的
//! `- a` 列表两种形态（v1 只消费 title/tags 字段，其余忽略）。
//! 双链语法 `[[目标|别名]]`（别名可省）；标签 `#tag`（# 后紧跟非空白非 #）。

use regex::Regex;
use std::sync::OnceLock;

/// frontmatter 提取结果
#[derive(Clone, Debug, Default)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub tags: Vec<String>,
}

/// 剥离并解析 frontmatter（--- 围栏）；无 frontmatter 返回空 + 原文
pub fn split_frontmatter(text: &str) -> (Frontmatter, &str) {
    let trimmed = text.trim_start_matches('\u{feff}');
    let Some(rest) = trimmed.strip_prefix("---") else {
        return (Frontmatter::default(), text);
    };
    let after = rest.trim_start_matches('\r');
    let Some(end) = after.find("\n---") else {
        return (Frontmatter::default(), text);
    };
    let block = &after[..end];
    // end 指向结束围栏前的 '\n'；end+4 跳过 "\n---"，剩余可能为 "\n正文"（LF）或 "\r\n正文"（CRLF）
    let body = &after[end + 4..];
    let body = body.strip_prefix('\r').unwrap_or(body);
    let body = body.strip_prefix('\n').unwrap_or(body);
    (parse_frontmatter(block), body)
}

/// 解析 frontmatter 块（极简 key: value / tags 列表）
pub fn parse_frontmatter(block: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let mut in_tags_list = false;
    for raw in block.lines() {
        let line = raw.trim_end_matches('\r');
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(item) = t.strip_prefix("- ") {
            if in_tags_list {
                let v = item.trim().trim_matches('"');
                if !v.is_empty() {
                    fm.tags.push(v.to_string());
                }
            }
            continue;
        }
        in_tags_list = false;
        if let Some((k, v)) = t.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim();
            match key.as_str() {
                "title" if !val.is_empty() => {
                    fm.title = Some(val.trim_matches('"').to_string());
                }
                "tags" => {
                    if val.is_empty() {
                        in_tags_list = true; // 列表形态在后续 "- x" 行
                    } else {
                        // 行内形态 tags: [a, b] 或 tags: a, b
                        let inner = val
                            .trim_start_matches('[')
                            .trim_end_matches(']')
                            .split(',')
                            .map(|s| s.trim().trim_matches('"').to_string())
                            .filter(|s| !s.is_empty());
                        fm.tags.extend(inner);
                    }
                }
                _ => {}
            }
        }
    }
    fm
}

fn link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[\[([^\]\|]+)(?:\|([^\]]*))?\]\]").expect("链接正则"))
}

fn tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?:^|[\s(（【])#([^\s#。，、！？：；""')）】]+)"#).expect("标签正则"))
}

/// 提取双链目标（trim 后；空目标忽略）
pub fn extract_links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for cap in link_re().captures_iter(text) {
        let dst = cap[1].trim();
        if !dst.is_empty() && !out.iter().any(|e: &String| e == dst) {
            out.push(dst.to_string());
        }
    }
    out
}

/// 提取正文 #标签（frontmatter tags 由调用方合并）
pub fn extract_tags(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for cap in tag_re().captures_iter(text) {
        let tag = cap[1].trim();
        if !tag.is_empty() && !out.iter().any(|e: &String| e == tag) {
            out.push(tag.to_string());
        }
    }
    out
}

/// 从正文取标题：首个 `# H1` 行；无则 None（调用方回退文件名 stem）
pub fn first_h1(text: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(h) = t.strip_prefix("# ") {
            let h = h.trim();
            if !h.is_empty() {
                return Some(h.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_title_and_tags() {
        let src = "---\ntitle: 我的笔记\ntags: [rust, tauri]\nother: x\n---\n# 正文\n";
        let (fm, body) = split_frontmatter(src);
        assert_eq!(fm.title.as_deref(), Some("我的笔记"));
        assert_eq!(fm.tags, vec!["rust", "tauri"]);
        assert!(body.starts_with("# 正文"));
    }

    #[test]
    fn frontmatter_tags_list_form() {
        let src = "---\ntags:\n- alpha\n- \"beta\"\n---\nbody";
        let (fm, _) = split_frontmatter(src);
        assert_eq!(fm.tags, vec!["alpha", "beta"]);
    }

    #[test]
    fn no_frontmatter_returns_body() {
        let (fm, body) = split_frontmatter("# 纯正文\n[[a]]\n");
        assert!(fm.title.is_none());
        assert_eq!(body, "# 纯正文\n[[a]]\n");
    }

    #[test]
    fn links_and_alias() {
        let text = "见 [[目标]] 与 [[目标|别名]] 及 [[sub/note.md]]，[[ ]] 空忽略";
        assert_eq!(
            extract_links(text),
            vec!["目标", "sub/note.md"]
        );
    }

    #[test]
    fn tags_skip_headings() {
        let text = "# 这不是标签\n正文 #rust 与 #中文标签，行首#紧贴也算";
        assert_eq!(extract_tags(text), vec!["rust", "中文标签"]);
    }

    #[test]
    fn h1_found() {
        assert_eq!(first_h1("前置\n# 标题一\n## 子"), Some("标题一".into()));
        assert_eq!(first_h1("无标题"), None);
    }
}
