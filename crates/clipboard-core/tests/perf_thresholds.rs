//! D-27：DESIGN §9.1 阈值断言（搜索响应 < 50ms、捕获入库 < 100ms）。
//!
//! 与 `benches/clip_store.rs` 同一热路径，但走常规 `cargo test`：任何提交让 §9.1
//! 的机器可判定线生效。判定用 p95（100 次采样），不是单次极值——共享 runner 的
//! 抖动由分位数吸收。若 debug 档在本机误报，按 D-27 决策②允许切 `#[ignore]` +
//! release 专跑并在 DECISIONS 完成证据中记录实测数字。

use std::time::Instant;

use clipboard_core::store::ClipStore;
use clipboard_core::types::SearchQuery;

/// §9.1 字面阈值（毫秒，不放宽）
const FTS_P95_MS: f64 = 50.0;
const CAPTURE_P95_MS: f64 = 100.0;

/// p 分位（0.0<p<=1.0）：升序后取 ceil(p·n)−1 位的毫秒值
fn percentile_ms(samples: &[f64], p: f64) -> f64 {
    assert!(!samples.is_empty());
    assert!((0.0..=1.0).contains(&p));
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let idx = ((sorted.len() as f64 * p).ceil() as usize - 1).min(sorted.len() - 1);
    sorted[idx]
}

fn temp_store(tag: &str) -> ClipStore {
    let dir = std::env::temp_dir().join(format!("nf_clip_perf_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ClipStore::open(&dir.join("clipboard.db"), dir.join("blobs")).expect("临时目录应可建库")
}

fn seed(store: &ClipStore, n: usize) {
    for i in 0..n {
        store
            .insert(
                &format!("条目 {i} 剪贴板 样本 内容 clip sample entry {i} cargo build 记录"),
                None,
                false,
                None,
                "local",
            )
            .expect("种子写入");
    }
}

/// 验收①负例自检：判定函数本身能抓出慢序列（证明断言不是恒真）
#[test]
fn percentile_detects_slow_series_across_threshold() {
    let fast = vec![1.0; 100];
    let mut slow = vec![1.0; 90];
    slow.extend(vec![FTS_P95_MS * 3.0; 10]); // 10% 样本越限 → p95 必越限
    assert!(percentile_ms(&fast, 0.95) < FTS_P95_MS, "快序列必须通过");
    assert!(percentile_ms(&slow, 0.95) >= FTS_P95_MS, "慢序列必须失败");
    assert_eq!(percentile_ms(&[5.0, 1.0, 3.0], 0.5), 3.0);
    assert_eq!(percentile_ms(&[2.0, 9.0], 1.0), 9.0);
}

#[test]
fn fts_search_p95_below_50ms_at_5k_corpus() {
    let store = temp_store("fts5k");
    seed(&store, 5_000);

    let mut samples = Vec::with_capacity(100);
    for i in 0..100 {
        let text = if i % 2 == 0 { "剪贴板" } else { "cargo" };
        let t = Instant::now();
        let page = store
            .search(&SearchQuery {
                text: Some(text.into()),
                ..Default::default()
            })
            .expect("搜索不报错");
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
        assert!(!page.items.is_empty(), "{text} 应命中语料");
    }
    let p95 = percentile_ms(&samples, 0.95);
    println!("§9.1 FTS 搜索 p95={p95:.3}ms（阈值 {FTS_P95_MS}ms，5k 语料）");
    assert!(
        p95 < FTS_P95_MS,
        "FTS 搜索 p95 {p95:.3}ms 超 §9.1 阈值 {FTS_P95_MS}ms"
    );
}

#[test]
fn capture_insert_p95_below_100ms_unique_and_dedup() {
    let store = temp_store("capture");
    seed(&store, 1_000);

    let mut samples = Vec::with_capacity(100);
    for i in 0..50 {
        let t = Instant::now();
        store
            .insert(
                &format!("捕获唯一载荷 {i} capture unique payload"),
                None,
                false,
                None,
                "local",
            )
            .expect("唯一插入");
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    for i in 0..50 {
        let t = Instant::now();
        store
            .insert(
                &format!(
                    "条目 {} 剪贴板 样本 内容 clip sample entry {} cargo build 记录",
                    i, i
                ),
                None,
                false,
                None,
                "local",
            )
            .expect("去重命中插入");
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let p95 = percentile_ms(&samples, 0.95);
    println!("§9.1 捕获入库 p95={p95:.3}ms（阈值 {CAPTURE_P95_MS}ms，唯一+去重各 50 次）");
    assert!(
        p95 < CAPTURE_P95_MS,
        "捕获入库 p95 {p95:.3}ms 超 §9.1 阈值 {CAPTURE_P95_MS}ms"
    );
}
