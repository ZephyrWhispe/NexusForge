//! D-27：剪贴板存储热路径 criterion 基准（DESIGN §9.1/§9.2）。
//!
//! 基准矩阵 = 语料规模 {1k, 5k, 20k} × 操作 {fts 中文查询, fts 英文查询, 唯一插入, 去重命中插入}。
//! 基准只做趋势/回归对比（`cargo bench -- --save-baseline <tag>`），门禁线在
//! `tests/perf_thresholds.rs`（p95 断言）。临时目录不可用时整体跳过而非 panic（D-27 验收②）。

use std::sync::atomic::{AtomicUsize, Ordering};

use clipboard_core::store::{ClipStore, NewClip};
use clipboard_core::types::SearchQuery;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn sample_row(i: usize) -> String {
    format!("条目 {i} 剪贴板 样本 内容 clip sample entry {i} cargo build 记录")
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("nf_clip_bench_{tag}_{}", std::process::id()))
}

fn open_store(tag: &str) -> Option<ClipStore> {
    let dir = temp_root(tag);
    let _ = std::fs::remove_dir_all(&dir);
    match ClipStore::open(&dir.join("clipboard.db"), dir.join("blobs")) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("bench 跳过（临时目录不可用）tag={tag}: {e}");
            None
        }
    }
}

fn bench_scale(c: &mut Criterion, n: usize) {
    let tag = format!("seed_{n}");
    let Some(store) = open_store(&tag) else {
        return;
    };
    for i in 0..n {
        if store.insert_row(&NewClip::new(&sample_row(i))).is_err() {
            eprintln!("bench 跳过（种子写入失败）tag={tag}");
            return;
        }
    }

    c.bench_function(&format!("clip_store/fts_zh/{n}"), |b| {
        b.iter(|| {
            let page = store
                .search(&SearchQuery {
                    text: Some("剪贴板".into()),
                    ..Default::default()
                })
                .unwrap();
            black_box(page.items.len())
        })
    });
    c.bench_function(&format!("clip_store/fts_en/{n}"), |b| {
        b.iter(|| {
            let page = store
                .search(&SearchQuery {
                    text: Some("cargo".into()),
                    ..Default::default()
                })
                .unwrap();
            black_box(page.items.len())
        })
    });
    c.bench_function(&format!("clip_store/insert_unique/{n}"), |b| {
        let counter = AtomicUsize::new(0);
        b.iter(|| {
            let i = counter.fetch_add(1, Ordering::Relaxed);
            let id = store
                .insert_row(&NewClip::new(&format!("基准独占载荷 {i} bench payload")))
                .unwrap();
            black_box(id)
        })
    });
    c.bench_function(&format!("clip_store/insert_dedup/{n}"), |b| {
        b.iter(|| black_box(store.insert_row(&NewClip::new(&sample_row(0))).unwrap()))
    });

    let _ = std::fs::remove_dir_all(temp_root(&tag));
}

fn clip_store_benchmarks(c: &mut Criterion) {
    for n in [1_000usize, 5_000, 20_000] {
        bench_scale(c, n);
    }
}

criterion_group!(benches, clip_store_benchmarks);
criterion_main!(benches);
