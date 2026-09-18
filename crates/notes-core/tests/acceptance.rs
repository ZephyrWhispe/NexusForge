//! 阶段三验收 2（docs/impl/06 尾部清单）：
//! 笔记外部修改（模拟 VS Code 改 md）10s 内索引同步；重命名引用改写零失败。
//! D-02 回归：NotesModule 经 StoragePort 注入驱动完成 init（不再直依 file-core）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use host_core::error::AppError;
use host_core::events::EventBus;
use host_core::module::{Module, ModuleContext};
use host_core::ports::Ports;
use host_core::storage::{DriverInfo, FileEntry, StorageDriver, StoragePort};
use notes_core::{NoteLibrary, NotesModule};

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nf_notes_accept_{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// D-02：验收不再借 file-core；自带最小本地盘驱动
struct FsDriver;

fn io_err(e: std::io::Error) -> AppError {
    AppError::module("FILE_OPS_001", e.to_string(), None)
}

impl StorageDriver for FsDriver {
    fn id(&self) -> &'static str {
        "local"
    }
    fn label(&self) -> String {
        "本地磁盘(测试)".into()
    }
    fn roots(&self) -> Vec<PathBuf> {
        vec![]
    }
    fn list(&self, path: &Path) -> Result<Vec<FileEntry>, AppError> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(path).map_err(io_err)? {
            let e = e.map_err(io_err)?;
            let md = e.metadata().ok();
            let name = e.file_name().to_string_lossy().into_owned();
            out.push(FileEntry {
                hidden: name.starts_with('.'),
                is_dir: md.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                size: md.as_ref().map(|m| m.len()).unwrap_or(0),
                modified_ms: md
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
                ext: e
                    .path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_lowercase())
                    .unwrap_or_default(),
                name,
                path: e.path(),
            });
        }
        Ok(out)
    }
    fn mkdir(&self, path: &Path) -> Result<(), AppError> {
        std::fs::create_dir_all(path).map_err(io_err)
    }
    fn remove(&self, path: &Path, _recycle: bool) -> Result<(), AppError> {
        if path.is_dir() {
            std::fs::remove_dir_all(path).map_err(io_err)
        } else {
            std::fs::remove_file(path).map_err(io_err)
        }
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<(), AppError> {
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p).map_err(io_err)?;
        }
        std::fs::rename(from, to).map_err(io_err)
    }
    fn read_file(&self, path: &Path) -> Result<Vec<u8>, AppError> {
        std::fs::read(path).map_err(io_err)
    }
    fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), AppError> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(io_err)?;
        }
        let tmp = path.with_extension("nf-tmp");
        std::fs::write(&tmp, data).map_err(io_err)?;
        std::fs::rename(&tmp, path).map_err(io_err)
    }
}

/// D-02 注入面：Ports 注册的 StoragePort 桥
struct TestStoragePort {
    driver: Arc<dyn StorageDriver>,
}
impl StoragePort for TestStoragePort {
    fn driver(&self, id: &str) -> Option<Arc<dyn StorageDriver>> {
        (id == "local").then(|| self.driver.clone())
    }
    fn list_drivers(&self) -> Vec<DriverInfo> {
        vec![DriverInfo { id: "local".into(), label: "本地磁盘(测试)".into(), roots: vec![] }]
    }
}

fn lib(tag: &str) -> NoteLibrary {
    let d = tmpdir(tag);
    NoteLibrary::open(d.join("vault"), &d.join("notes.db"), Arc::new(FsDriver)).unwrap()
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

/// D-02 回归：init 从 Ports 取 StoragePort 注入驱动；缺端口必须明确报错（不静默自建）
#[test]
fn module_init_resolves_driver_via_storage_port() {
    let dir = tmpdir("init");
    let ports = Arc::new(Ports::new());
    ports.register::<dyn StoragePort>(Arc::new(TestStoragePort { driver: Arc::new(FsDriver) }));
    let ctx = Arc::new(ModuleContext {
        app_data_dir: dir.clone(),
        ports,
        event_bus: Arc::new(EventBus::new()),
    });
    let module = NotesModule::new(&dir);
    module.init(ctx).unwrap();
    let lib = module.library().expect("init 后 library 应可用");
    lib.create("d02.md", "# 经端口注入的驱动\n").unwrap();
    assert_eq!(lib.read("d02.md").unwrap().0, "# 经端口注入的驱动\n");

    // 负例：未注册 StoragePort 时 init 必须失败并给出可行动错误
    let dir2 = tmpdir("init-missing");
    let ctx2 = Arc::new(ModuleContext {
        app_data_dir: dir2.clone(),
        ports: Arc::new(Ports::new()),
        event_bus: Arc::new(EventBus::new()),
    });
    let err = NotesModule::new(&dir2).init(ctx2).unwrap_err();
    assert!(
        err.to_string().contains("StoragePort"),
        "错误应指明 StoragePort 缺失，实际: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}
