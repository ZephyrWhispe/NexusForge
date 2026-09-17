//! D2 模糊匹配打分（docs/impl/05 D2）：
//! `score = 0.5*前缀命中率 + 0.3*子序列连续度 + 0.2*使用频次`。
//! 频次带 30 天半衰期衰减：`count * exp(-Δdays/30)`。
//! 纯逻辑无 IO；匹配不区分大小写；子序列不命中返回 None（整体淘汰）。

const W_PREFIX: f64 = 0.5;
const W_CONTIGUITY: f64 = 0.3;
const W_FREQ: f64 = 0.2;
const HALF_LIFE_DAYS: f64 = 30.0;

/// 单条模糊匹配结果
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FuzzyHit {
    /// 前缀/词首命中率 [0,1]：完全前缀 1.0；词首（空格/大小写边界/分隔符后）0.8；否则 0
    pub prefix: f64,
    /// 子序列连续度 [0,1]：最长连续命中段长度 / 查询长度
    pub contiguity: f64,
}

impl FuzzyHit {
    fn total(&self, freq: f64) -> f64 {
        W_PREFIX * self.prefix + W_CONTIGUITY * self.contiguity + W_FREQ * freq
    }
}

/// 查询 name 是否命中 query；命中返回打分要素，未命中（非子序列）返回 None
pub fn fuzzy_match(query: &str, name: &str) -> Option<FuzzyHit> {
    let q: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    if q.is_empty() {
        return Some(FuzzyHit { prefix: 0.0, contiguity: 1.0 });
    }
    let n: Vec<char> = name.chars().flat_map(|c| c.to_lowercase()).collect();
    if n.is_empty() {
        return None;
    }

    // 前缀/词首判定
    let is_full_prefix = n.len() >= q.len() && n[..q.len()].iter().eq(q.iter());
    let word_start = !is_full_prefix
        && (is_boundary_start(&q, name) || is_acronym_prefix(&q, name));
    let prefix = if is_full_prefix {
        1.0
    } else if word_start {
        0.8
    } else {
        0.0
    };

    // 子序列匹配 + 最长连续段
    let mut qi = 0usize;
    let mut run = 0usize;
    let mut best_run = 0usize;
    for &nc in &n {
        if qi < q.len() && nc == q[qi] {
            qi += 1;
            run += 1;
            best_run = best_run.max(run);
        } else {
            run = 0;
        }
    }
    if qi < q.len() {
        return None; // 非子序列 → 整体淘汰
    }

    Some(FuzzyHit {
        prefix,
        contiguity: best_run as f64 / q.len() as f64,
    })
}

/// 词首判定：query 与 name 的某个词边界对齐（起始处、空格/-/_/. 之后、驼峰大写处）。
/// 必须在**原始** name 上判定（lowercase 会丢大小写边界信息）。
fn is_boundary_start(q: &[char], name: &str) -> bool {
    let raw: Vec<char> = name.chars().collect();
    for i in 0..raw.len() {
        let boundary = i == 0
            || matches!(raw[i - 1], ' ' | '-' | '_' | '.')
            || raw[i].is_uppercase();
        if boundary {
            let rest: Vec<char> = raw[i..].iter().flat_map(|c| c.to_lowercase()).collect();
            if rest.starts_with(q) {
                return true;
            }
        }
    }
    false
}

/// 驼峰缩写判定：query 是 name 各词首字母缩写的前缀（如 "np" → NodePad）
fn is_acronym_prefix(q: &[char], name: &str) -> bool {
    let mut firsts = String::new();
    let mut prev_is_sep = true;
    let mut prev_upper = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            prev_is_sep = true;
            prev_upper = false;
            continue;
        }
        let boundary = prev_is_sep || (c.is_uppercase() && !prev_upper);
        if boundary && c.is_alphabetic() {
            firsts.extend(c.to_lowercase());
        }
        prev_is_sep = false;
        prev_upper = c.is_uppercase();
    }
    let query: String = q.iter().collect();
    firsts.starts_with(&query)
}

/// 频次衰减分 [0,1)：`1 - exp(-weighted/8)`，weighted = Σ count*exp(-Δdays/30)
pub fn freq_score(weighted: f64) -> f64 {
    1.0 - (-weighted / 8.0).exp()
}

/// 单条记录的衰减权重：`count * exp(-Δdays/30)`
pub fn decay(count: u32, last_used_days_ago: f64) -> f64 {
    count as f64 * (-(last_used_days_ago / HALF_LIFE_DAYS)).exp()
}

/// 综合打分（外部已算出 fuzzy 要素与频次时）
pub fn total_score(hit: &FuzzyHit, weighted_freq: f64) -> f64 {
    hit.total(freq_score(weighted_freq))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_prefix_scores_higher_than_word_start() {
        let full = fuzzy_match("goo", "Google").unwrap();
        let word = fuzzy_match("goo", "New Google Chrome").unwrap();
        assert_eq!(full.prefix, 1.0);
        assert_eq!(word.prefix, 0.8);
        assert!(full.total(0.0) > word.total(0.0));
    }

    #[test]
    fn non_subsequence_is_none() {
        assert!(fuzzy_match("xyz", "Chrome").is_none());
    }

    #[test]
    fn contiguity_prefers_consecutive_match() {
        // "chr" 连续命中 contiguity=1
        let a = fuzzy_match("chr", "chrome").unwrap();
        // "cr" 命中但中间隔 h，连续段=1，contiguity=0.5
        let b = fuzzy_match("cr", "chrome").unwrap();
        assert!((a.contiguity - 1.0).abs() < 1e-9);
        assert!((b.contiguity - 0.5).abs() < 1e-9);
        assert!(a.total(0.0) > b.total(0.0));
    }

    #[test]
    fn case_insensitive() {
        assert!(fuzzy_match("CHR", "chrome").is_some());
        assert!(fuzzy_match("记事", "记事本").is_some());
    }

    #[test]
    fn boundary_word_start_detection() {
        // "rec" 在 "Recycle Bin" 词首
        let hit = fuzzy_match("rec", "Recycle Bin").unwrap();
        assert!(hit.prefix >= 0.8);
        // 驼峰缩写："np" 命中 NodePad 各词首字母
        let camel = fuzzy_match("np", "NodePad").unwrap();
        assert!(camel.prefix >= 0.8, "驼峰缩写应算词首");
        // 缩写须按词首顺序："pn" 不是子序列词首（p 在 n 前？no）——纯子序列仍可命中但 prefix=0
        let sub = fuzzy_match("ep", "NodePad").unwrap();
        assert!(sub.prefix < 0.8);
    }

    #[test]
    fn empty_query_matches_everything() {
        assert!(fuzzy_match("", "anything").is_some());
    }

    #[test]
    fn freq_decays_over_time() {
        let fresh = decay(10, 0.0);
        let month = decay(10, 30.0);
        let year = decay(10, 365.0);
        assert!(fresh > month && month > year);
        assert!((month - 10.0 * std::f64::consts::E.powf(-1.0)).abs() < 1e-9);
        // 衰减分单调
        assert!(freq_score(fresh) > freq_score(year));
        assert!(freq_score(0.0) == 0.0);
    }
}
