//! R-I2（D-37·GOV-09 参数级契约）：前端 `src/ipc/client.ts` 每个 invoke 的
//! 命令名必须 ∈ `generate_handler!` 注册表，且内联 payload 的**顶层键集合**
//! ⊆ 对应 Rust 命令签名的参数名集合（Tauri v2 默认 camelCase 收键，
//! snake_case 原形一并放行）。零新依赖：两侧均为手写扫描，与
//! `security_config.rs::registered_commands` 同族先例。
//! 首跑必须真绿；若抓到漂移，逐条裁决，不得静默豁免。

use std::collections::BTreeMap;
use std::path::Path;

fn read(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读取 {} 失败: {e}", path.display()))
}

// ---------------------------------------------------------------- 工具

/// 按**顶层**逗号切分（<> () {} [] 深度计），不含末尾空段
fn split_top_level(args: &str) -> Vec<String> {
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut out = Vec::new();
    for ch in args.chars() {
        match ch {
            '<' | '(' | '{' | '[' => {
                depth += 1;
                cur.push(ch);
            }
            '>' | ')' | '}' | ']' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn to_camel(s: &str) -> String {
    let mut parts = s.split('_');
    let first = parts.next().unwrap_or_default();
    let rest: String = parts
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(h) => h.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect();
    format!("{first}{rest}")
}

// ------------------------------------------------------- Rust 命令面

/// 宿主注入型参数（不出现在 IPC payload 里），按类型子串识别
const INJECTED_TYPES: &[&str] = &[
    "State",
    "AppHandle",
    "Manager",
    "Window",
    "WebviewWindow",
    "UriSchemeResponder",
];

/// 从 `#[...]` 属性串之后扫过空白/注释/其它属性，落到 `fn` 关键字处
fn skip_to_fn(text: &str, from: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = from;
    loop {
        while i < b.len() && (b[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        match text[i..].chars().next().unwrap() {
            '/' if text[i..].starts_with("//") => {
                i = text[i..].find('\n').map(|n| i + n + 1).unwrap_or(b.len());
            }
            '/' if text[i..].starts_with("/*") => {
                i = text[i..].find("*/").map(|e| i + e + 2).unwrap_or(b.len());
            }
            '#' if text[i..].starts_with("#[") => {
                // 跳过另一枚属性（如 #[allow(...)]），含嵌套括号配平
                let mut depth = 0i32;
                let mut j = i;
                while j < b.len() {
                    match b[j] as char {
                        '[' => depth += 1,
                        ']' => {
                            depth -= 1;
                            if depth == 0 {
                                j += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                i = j;
            }
            _ => {
                if text[i..].starts_with("fn ") || text[i..].starts_with("fn(") {
                    return Some(i);
                }
                // pub / pub(crate) / async 等修饰词：跳到下一个空白
                let adv = text[i..]
                    .find(|c: char| c.is_whitespace())
                    .map(|w| i + w)
                    .unwrap_or(b.len());
                if adv == i {
                    return None;
                }
                i = adv;
            }
        }
    }
}

/// 命令名 → 非注入参数名列表；第二返回值 = rename_all 等异形属性命中数
fn command_signatures() -> (BTreeMap<String, Vec<String>>, Vec<String>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("src/commands 目录必须存在")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("rs"))
        .collect();
    files.sort();
    let mut map = BTreeMap::new();
    let mut odd = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let mut cursor = 0usize;
        while let Some(rel) = text[cursor..].find("#[tauri::command") {
            let attr_at = cursor + rel;
            let after = attr_at + "#[tauri::command".len();
            // 异形属性（rename_all 会让"camel+snake 双形放行"的前提失效）
            if let Some(rest) = text.get(after..) {
                let trimmed = rest.trim_start();
                if trimmed.starts_with('(') {
                    odd.push(format!(
                        "{}: #[tauri::command(...)] 带参属性",
                        path.file_name().unwrap().to_string_lossy()
                    ));
                }
            }
            let attr_end = after;
            cursor = attr_end;
            let Some(fn_at) = skip_to_fn(&text, attr_end) else {
                continue;
            };
            let mut name_at = fn_at + "fn".len();
            while text[name_at..].starts_with(|c: char| c.is_whitespace()) {
                name_at += 1;
            }
            let mut j = name_at;
            while j < text.len()
                && !text[j..].starts_with(char::is_whitespace)
                && !text[j..].starts_with('(')
            {
                j += text[j..].chars().next().unwrap().len_utf8();
            }
            let name = text[name_at..j].to_string();
            // 参数表：首个 '(' 到配平 ')'
            let Some(paren_at) = text[j..].find('(').map(|p| j + p) else {
                continue;
            };
            let mut depth = 0i32;
            let mut end = paren_at;
            for (off, ch) in text[paren_at..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = paren_at + off;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let params = text[paren_at + 1..end].to_string();
            let kept = split_top_level(&params)
                .into_iter()
                .filter_map(|seg| {
                    let seg = seg.trim().to_string();
                    let colon = seg.find(':')?;
                    let name = seg[..colon].trim().to_string();
                    let ty = seg[colon + 1..].trim().to_string();
                    if INJECTED_TYPES.iter().any(|t| ty.contains(t)) {
                        return None;
                    }
                    Some(name)
                })
                .collect::<Vec<_>>();
            map.insert(name, kept);
        }
    }
    (map, odd)
}

fn registered_commands() -> Vec<String> {
    let lib = read("src/lib.rs");
    let start = lib
        .find("generate_handler![")
        .expect("invoke_handler 注册块必须存在");
    let block = &lib[start..];
    let end = block.find("])").expect("注册块应闭合");
    block[..end]
        .lines()
        .filter_map(|line| {
            let t = line.trim();
            t.strip_prefix("commands::")
                .and_then(|rest| rest.split(',').next())
                .map(str::to_string)
        })
        .collect()
}

// ------------------------------------------------------ 前端调用面

struct InvokeSite {
    cmd: String,
    keys: Vec<String>,
}

/// 扫过泛型实参 `<...>`（对象类型内的 {} 计深，`=>` 不误减），返回 '>' 后位置
fn skip_generic(text: &str, lt: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut brace = 0i32;
    let mut prev: Option<char> = None;
    for (off, ch) in text[lt..].char_indices() {
        match ch {
            '<' if brace == 0 => depth += 1,
            '>' if brace == 0 => {
                if prev == Some('=') {
                    // `=>`：只是箭头，不封泛型
                } else {
                    depth -= 1;
                    if depth == 0 {
                        return Some(lt + off + 1);
                    }
                }
            }
            '{' => brace += 1,
            '}' => brace -= 1,
            _ => {}
        }
        prev = Some(ch);
    }
    None
}

/// 从 '{' 处扫对象字面量，收集顶层键；返回 (键, 闭合后位置)。
/// spread/计算键/字符串外的畸形形状 ⇒ None（调用方记 odd）
fn object_top_keys(text: &str, open: usize) -> Option<(Vec<String>, usize)> {
    let b: Vec<char> = text[open..].chars().collect();
    let mut i = 1usize; // 跳过 '{'
    let depth = 1i32;
    let mut keys = Vec::new();
    loop {
        // 空白 + 注释
        loop {
            while i < b.len() && b[i].is_whitespace() {
                i += 1;
            }
            if i + 1 < b.len() && b[i] == '/' && b[i + 1] == '/' {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            } else if i + 1 < b.len() && b[i] == '/' && b[i + 1] == '*' {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            } else {
                break;
            }
        }
        if i >= b.len() {
            return None;
        }
        if b[i] == '}' {
            if depth == 1 {
                return Some((keys, open + i + 1));
            }
            return None;
        }
        if depth != 1 {
            return None;
        }
        if b[i] == '.' {
            return None; // spread
        }
        // 键：字符串字面量或标识符
        let key = if b[i] == '"' || b[i] == '\'' {
            let quote = b[i];
            let start = i + 1;
            i += 1;
            while i < b.len() && b[i] != quote {
                if b[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            let k = text[open + start..open + i].to_string();
            i += 1;
            k
        } else if b[i].is_alphabetic() || b[i] == '_' || b[i] == '$' {
            let start = i;
            while i < b.len() && (b[i].is_alphanumeric() || matches!(b[i], '_' | '$')) {
                i += 1;
            }
            text[open + start..open + i].to_string()
        } else {
            return None; // 计算键等畸形形状
        };
        // 键后：':' ⇒ 需跳过值到 depth==1 的 ',' 或 '}'；','/'{…简中}' ⇒ 短写键
        while i < b.len() && b[i].is_whitespace() {
            i += 1;
        }
        match b.get(i) {
            Some(':') => {
                i += 1;
                let mut vd = 0i32;
                loop {
                    if i >= b.len() {
                        return None;
                    }
                    let c = b[i];
                    if c == '"' || c == '\'' || c == '`' {
                        // 值内字符串/模板字面量：整段跳过（内部 , } 不算顶层界标；
                        // 模板 ${…} 嵌套不解析——命中即由后续形状校验兜底记 odd）
                        let q = c;
                        i += 1;
                        while i < b.len() && b[i] != q {
                            if b[i] == '\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    } else if matches!(c, '{' | '(' | '[') {
                        vd += 1;
                    } else if matches!(c, '}' | ')' | ']') {
                        if vd == 0 {
                            break; // 命中对象自身的闭合 '}'
                        }
                        vd -= 1;
                    } else if c == ',' && vd == 0 {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            Some(',') => {
                keys.push(key); // 短写
                i += 1;
            }
            Some('}') => {
                keys.push(key);
                // 不推进：下一轮循环收尾于 '}'
            }
            _ => return None,
        }
    }
}

/// client.ts 的全部 invoke 站点；第二返回值 = 无法静态定形的调用
fn invoke_sites() -> (Vec<InvokeSite>, Vec<String>) {
    let text = read("../src/ipc/client.ts");
    let chars: Vec<char> = text.chars().collect();
    let mut sites = Vec::new();
    let mut odd = Vec::new();
    let needle: Vec<char> = "invoke".chars().collect();
    let mut i = 0usize;
    while i + needle.len() <= chars.len() {
        // 词边界：invoke 前一字符不得是标识符字符
        let boundary_ok =
            i == 0 || !matches!(chars[i - 1], c if c.is_alphanumeric() || c == '_' || c == '$');
        if !boundary_ok || chars[i..i + needle.len()] != needle[..] {
            i += 1;
            continue;
        }
        // 后一字符必须是 '<' 或 '('（真调用），否则是标识符一部分/注释词
        let next = chars.get(i + needle.len()).copied();
        if !matches!(next, Some('<') | Some('(')) {
            i += 1;
            continue;
        }
        let mut j = i + needle.len();
        if next == Some('<') {
            let byte_lt = chars_pos_to_byte(&text, j);
            match skip_generic(&text, byte_lt) {
                Some(byte_after) => {
                    j = byte_to_chars_pos(&text, byte_after);
                }
                None => {
                    odd.push(format!("char {i}: 泛型未配平"));
                    i += 1;
                    continue;
                }
            }
        }
        // 期望 '('
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if chars.get(j) != Some(&'(') {
            odd.push(format!("char {i}: invoke 后非 '('"));
            i += 1;
            continue;
        }
        j += 1;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        // 首参必须字符串字面量
        if chars.get(j) != Some(&'"') && chars.get(j) != Some(&'\'') {
            odd.push(format!("char {i}: 命令名非字面量"));
            i += 1;
            continue;
        }
        let quote = chars[j];
        let start = j + 1;
        j += 1;
        while j < chars.len() && chars[j] != quote {
            if chars[j] == '\\' {
                j += 1;
            }
            j += 1;
        }
        let cmd: String = chars[start..j].iter().collect();
        j += 1;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let keys = match chars.get(j) {
            Some(')') => Vec::new(),
            Some(',') => {
                j += 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if chars.get(j) != Some(&'{') {
                    odd.push(format!("{cmd}: payload 非对象字面量"));
                    i += 1;
                    continue;
                }
                let byte_open = chars_pos_to_byte(&text, j);
                match object_top_keys(&text, byte_open) {
                    Some((keys, byte_after)) => {
                        // 对象后必须紧跟 ')'（无第三实参）
                        let mut k = byte_to_chars_pos(&text, byte_after);
                        while k < chars.len() && chars[k].is_whitespace() {
                            k += 1;
                        }
                        if chars.get(k) != Some(&')') {
                            odd.push(format!("{cmd}: 对象实参后非 ')'"));
                            i += 1;
                            continue;
                        }
                        keys
                    }
                    None => {
                        odd.push(format!("{cmd}: payload 对象无法静态定形"));
                        i += 1;
                        continue;
                    }
                }
            }
            _ => {
                odd.push(format!("char {i}: invoke 首参后畸形"));
                i += 1;
                continue;
            }
        };
        sites.push(InvokeSite { cmd, keys });
        i += needle.len();
    }
    (sites, odd)
}

fn chars_pos_to_byte(text: &str, char_pos: usize) -> usize {
    text.char_indices()
        .nth(char_pos)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

fn byte_to_chars_pos(text: &str, byte_pos: usize) -> usize {
    text[..byte_pos].chars().count()
}

// ------------------------------------------------------------- 断言

#[test]
fn command_surface_matches_registry() {
    let (sigs, odd) = command_signatures();
    assert!(odd.is_empty(), "异形 #[tauri::command(..)] 属性: {odd:?}");
    let registry = registered_commands();
    let missing: Vec<_> = registry.iter().filter(|c| !sigs.contains_key(*c)).collect();
    let extra: Vec<_> = sigs
        .keys()
        .filter(|c| !registry.iter().any(|r| r == *c))
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "解析漂移：注册表有而签名缺 {missing:?}；签名有而注册表缺 {extra:?}"
    );
    assert!(registry.len() >= 250, "正对照：命令面应成规模");
}

#[test]
fn invoke_names_and_payload_keys_match_signatures() {
    let (sigs, _) = command_signatures();
    let (sites, odd) = invoke_sites();
    assert!(
        odd.is_empty(),
        "client.ts 存在无法静态定形的 invoke: {odd:?}"
    );
    assert!(
        sites.len() >= 250,
        "正对照：invoke 站点应成规模（GOV-09 扫描非空洞），实得 {}",
        sites.len()
    );
    let mut errors: Vec<String> = Vec::new();
    for site in &sites {
        let Some(params) = sigs.get(&site.cmd) else {
            errors.push(format!("invoke(\"{}\") 不在命令注册表", site.cmd));
            continue;
        };
        let allowed = |k: &str| params.iter().any(|p| p == k || to_camel(p) == k);
        for key in &site.keys {
            if !allowed(key) {
                errors.push(format!(
                    "invoke(\"{}\") 键 {key:?} 无对应参数（签名: {params:?}）",
                    site.cmd
                ));
            }
        }
    }
    assert!(
        errors.is_empty(),
        "GOV-09 参数级契约违例 {} 处：\n{}",
        errors.len(),
        errors.join("\n")
    );
}

#[test]
fn every_invoke_site_name_is_registered_command() {
    let registry = registered_commands();
    let (sites, _) = invoke_sites();
    let unknown: Vec<_> = sites
        .iter()
        .map(|s| s.cmd.as_str())
        .filter(|c| !registry.iter().any(|r| r == c))
        .collect();
    assert!(unknown.is_empty(), "未注册命令名被 invoke: {unknown:?}");
}
