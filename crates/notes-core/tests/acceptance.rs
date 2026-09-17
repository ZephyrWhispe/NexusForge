//! 阶段三验收 2（docs/impl/06 尾部清单）：
//! 笔记外部修改（模拟 VS Code 改 md）10s 内索引同步；重命名引用改写零失败。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use file_core::driver::DriverRegistry;
use notes_core::NoteLibrary;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nf_notes_accept_{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn lib(tag: &str) -> NoteLibrary {
    let d = tmpdir(tag);
    let reg = DriverRegistry::new();
    NoteLibrary::open(d.join("vault"), &d.join("notes.db"), reg.get("local").unwrap()).unwrap()
}

#[test]
fn external_edit_syncs_within_10s_and_rename_never_fails() {
    let l = lib("main");

    // 初始库：两篇笔记 + 双链
    l.create("index.md", "# 索引\n见 [[guide]] 与 [[todo]]\n").unwrap();
    l.create("guide.md", "# 指南\n内容\n").unwrap();
    l.create("todo.md", "# 待办\n内容\n").unwrap();

    // --- 场景 A：外部编辑器修改 todo.md（新增内容与标签）+ 新建新笔记 ---
    std::fs::write(l.root().join("todo.md"), "# 待办\n改过 #urgent\n见 [[index]]\n").unwrap();
    std::fs::create_dir_all(l.root().join("sub")).unwrap();
    std::fs::write(l.root().join("sub/new-note.md"), "# 新笔记\n[[guide]]\n").unwrap();

    // sync 收敛必须在 10s 内完成（验收线：模拟用户在 VS Code 保存后回来刷新面板的窗口）
    let started = Instant::now();
    let r = l.sync().unwrap();
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(10), "sync 超时：{elapsed:?}");
    assert_eq!(r.updated, 1, "外部修改未收敛");
    assert_eq!(r.added, 1, "外部新建未索引");
    assert_eq!(r.total, 4);

    // 索引内容校验：标签与反链
    let todo = l.index().get("todo.md").unwrap().unwrap();
    assert!(todo.tags.contains(&"urgent".to_string()));
    let back = l.backlinks("index.md").unwrap();
    assert!(back.iter().any(|b| b.src == "todo.md"), "外部新增链接未入反链");

    // --- 场景 B：重命名引用改写循环（10 轮 × 4 处引用，零失败）---
    // 外部再改一次内容，确保改写面对真实磁盘状态
    std::fs::write(
        l.root().join("index.md"),
        "# 索引\n见 [[guide]] 与 [[todo]] 与 [[sub/new-note]] 与 [[guide.md]]\n",
    )
    .unwrap();
    l.sync().unwrap();

    for round in 0..10u32 {
        let old = format!("sub/new-note-{round}.md");
        let new = format!("sub/renamed-{round}.md");
        std::fs::write(l.root().join(&old), "# 迁移\n").unwrap();
        l.sync().unwrap();
        // index.md 增加指向 old 的链接
        let content = std::fs::read_to_string(l.root().join("index.md")).unwrap();
        std::fs::write(l.root().join("index.md"), format!("{content}\n[[{}]]\n", stem_of(&old))).unwrap();
        l.sync().unwrap();

        l.rename(&old, &new).unwrap_or_else(|e| panic!("第 {round} 轮重命名失败: {e}"));

        // 引用已改写
        let after = std::fs::read_to_string(l.root().join("index.md")).unwrap();
        assert!(after.contains(&format!("[[{}]]", stem_of(&new))), "第 {round} 轮引用未改写");
        assert!(!after.contains(&format!("[[{}]]", stem_of(&old))), "第 {round} 轮残留旧引用");
        // 磁盘状态
        assert!(!l.root().join(&old).exists());
        assert!(l.root().join(&new).exists());
    }
}

fn stem_of(rel: &str) -> String {
    std::path::Path::new(rel)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap()
}

#[test]
fn rename_conflict_and_missing_are_clean_errors() {
    let l = lib("err");
    l.create("a.md", "x").unwrap();
    l.create("b.md", "[[a]]").unwrap();
    l.reindex().unwrap();
    // 目标已存在
    assert!(l.rename("a.md", "b.md").is_err());
    // 源不存在
    assert!(l.rename("ghost.md", "c.md").is_err());
    // 同路径
    assert!(l.rename("a.md", "a.md").is_err());
    // 越界路径
    assert!(l.rename("a.md", "../evil.md").is_err());
}
