//! 阶段三验收 3（docs/impl/06 尾部清单）：PDF 合并 100 个文件，内存峰值 < 300MB。
//!
//! 自定义 global allocator 统计进程峰值内存（测试二进制独占进程，无干扰）。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::path::PathBuf;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(now, Ordering::SeqCst);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        CURRENT.fetch_sub(layout.size(), Ordering::SeqCst);
        System.dealloc(ptr, layout);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const LIMIT_MB: usize = 300;

/// 构造 100 个 PDF：每份 10 页，每页带 ~40KB 内容流（真实-ish 输入规模 ≈ 40MB）
#[test]
fn pdf_merge_100_files_peak_memory_under_300mb() {
    let dir: PathBuf = std::env::temp_dir().join("nf_editor_accept_pdf");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 每页填充内容：重复文本块使内容流达到 ~40KB（lopdf 压缩前）
    let filler = "NexusForge acceptance test line with some ASCII payload padding. ".repeat(600); // ≈ 39KB
    let mut inputs: Vec<PathBuf> = Vec::with_capacity(100);
    for i in 0..100usize {
        let p = dir.join(format!("doc_{i:03}.pdf"));
        editor_core::pdf::make_test_pdf(&p, 10, &filler).expect("构造测试 PDF 失败");
        inputs.push(p);
    }

    let before = PEAK.load(Ordering::SeqCst);
    let output = dir.join("merged.pdf");
    let result = editor_core::pdf::merge(&inputs, &output).expect("合并失败");
    let after = PEAK.load(Ordering::SeqCst);

    assert_eq!(result.pages, 1000, "合并后页数不符");
    assert!(output.exists());

    // 验收：合并过程增量峰值 < 300MB
    let peak_delta_mb = (after - before) / 1024 / 1024;
    assert!(
        peak_delta_mb < LIMIT_MB,
        "合并内存峰值 {peak_delta_mb}MB ≥ {LIMIT_MB}MB"
    );
    eprintln!("验收3：100 文件 / 1000 页合并，增量峰值 {peak_delta_mb}MB < {LIMIT_MB}MB");

    let _ = std::fs::remove_dir_all(&dir);
}
