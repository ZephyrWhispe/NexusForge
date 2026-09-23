//! F6 存储驱动抽象（docs/impl/05 F6）：统一 list/read/write/mkdir/remove/move/quota。
//!
//! D-02：trait 与 DTO 已上移至 host-core::storage（notes-core 等消费方经
//! [`host_core::storage::StoragePort`] 取驱动，不直依本 crate）；本文件保留
//! 本地驱动实现与注册表。v1 内置 LocalDriver（本地盘/UNC）；B6 远端连接
//! 经 [`DriverRegistry::register_as`] 以 `remote:{profile_id}` 动态键入表
//! （Netdisk/Rclone 明示不做，见 09 §6.3）。

use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use host_core::error::AppError;
use host_core::storage::StoragePort;

// D-02：trait/DTO 住 host-core::storage；此处 pub use 保持 file_core::driver::* API
// （T-B6-11 起随泛化增出能力声明与写流柄）
pub use host_core::storage::{
    DriverCapabilities, DriverInfo, FileEntry, StorageDriver, WriteCommit,
};

use crate::browse;

/// 本地 IO 错误统一映射（错误码沿用 FILE_OPS_001）
fn io_err(e: std::io::Error) -> AppError {
    AppError::module("FILE_OPS_001", e.to_string(), None)
}

// ---------------------------------------------------------------------------
// 本地驱动
// ---------------------------------------------------------------------------

/// 本地盘驱动：委托 [`crate::browse`] 与 std::fs
pub struct LocalDriver;

impl StorageDriver for LocalDriver {
    fn id(&self) -> String {
        "local".into()
    }
    fn label(&self) -> String {
        "本地磁盘".into()
    }
    fn roots(&self) -> Vec<PathBuf> {
        browse::drives().into_iter().map(|d| d.path).collect()
    }
    fn list(&self, path: &Path) -> Result<Vec<FileEntry>, AppError> {
        browse::list_dir(path, browse::SortKey::Name, true).map_err(Into::into)
    }
    fn mkdir(&self, path: &Path) -> Result<(), AppError> {
        std::fs::create_dir_all(crate::browse::to_long_path(path)).map_err(io_err)?;
        Ok(())
    }
    fn remove(&self, path: &Path, _recycle: bool) -> Result<(), AppError> {
        let long = crate::browse::to_long_path(path);
        if long.is_dir() {
            std::fs::remove_dir_all(&long).map_err(io_err)?;
        } else if long.is_file() {
            std::fs::remove_file(&long).map_err(io_err)?;
        } else {
            return Err(crate::error::FileError::NotFound(
                crate::browse::display_path(path)
                    .to_string_lossy()
                    .into_owned(),
            )
            .into());
        }
        Ok(())
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<(), AppError> {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(crate::browse::to_long_path(parent)).map_err(io_err)?;
        }
        std::fs::rename(
            crate::browse::to_long_path(from),
            crate::browse::to_long_path(to),
        )
        .map_err(io_err)?;
        Ok(())
    }
    fn read_file(&self, path: &Path) -> Result<Vec<u8>, AppError> {
        std::fs::read(crate::browse::to_long_path(path)).map_err(io_err)
    }
    fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), AppError> {
        let long = crate::browse::to_long_path(path);
        if let Some(parent) = long.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        // tmp + rename 原子替换，防半写损坏
        let tmp = long.with_extension("nf-tmp");
        std::fs::write(&tmp, data).map_err(io_err)?;
        std::fs::rename(&tmp, &long).map_err(io_err)?;
        Ok(())
    }
}

/// 注册表条目：`key` 是寻址 id（静态驱动 = `driver.id()`；B6 远端动态驱动 =
/// `remote:{profile_id}`，见 09 §6.1 ① 前缀裁定——`"local"` 结构性不可顶替）。
struct Registered {
    key: String,
    driver: Arc<dyn StorageDriver>,
}

/// 驱动注册表：静态内置 + 动态注册（B6 远端连接态即住这里，进程内、不落盘）
pub struct DriverRegistry {
    drivers: RwLock<Vec<Registered>>,
}

impl Default for DriverRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DriverRegistry {
    pub fn new() -> Self {
        let local: Arc<dyn StorageDriver> = Arc::new(LocalDriver);
        Self {
            drivers: RwLock::new(vec![Registered {
                key: local.id().to_owned(),
                driver: local,
            }]),
        }
    }

    /// 注册驱动（以驱动自报 id 寻址；同 id 重复注册以最后者为准）
    pub fn register(&self, driver: Arc<dyn StorageDriver>) {
        let key = driver.id().to_owned();
        self.register_as(&key, driver);
    }

    /// 以显式 id 注册（T-B6-3）：`StorageDriver::id()` 是 `&'static str`，
    /// 承载不了 `remote:{profile_id}` 这类动态 id，寻址键由注册口注入。
    /// 注意与 [`register`](Self::register) 的顶替域不同：这里按 `key` 去重，
    /// 不同 profile 的两个 webdav 驱动（自报 id 同为 `"webdav"`）互不顶替。
    pub fn register_as(&self, key: &str, driver: Arc<dyn StorageDriver>) {
        let mut v = self.drivers.write();
        v.retain(|r| r.key != key);
        v.push(Registered {
            key: key.to_owned(),
            driver,
        });
    }

    /// 注销（幂等）：回传是否真实移除了一个条目
    pub fn unregister(&self, key: &str) -> bool {
        let mut v = self.drivers.write();
        let before = v.len();
        v.retain(|r| r.key != key);
        before != v.len()
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn StorageDriver>> {
        self.drivers
            .read()
            .iter()
            .find(|r| r.key == id)
            .map(|r| r.driver.clone())
    }

    /// 浏览/传输链的驱动解析唯一分派口（T-B6-11）：`None` 或空 ⇒ `"local"`
    /// （既有 `file_list` 调用零扰动的缺省臂）；点名取不存在的驱动 ⇒ None，
    /// 由调用方诚实报错——**禁回落到 local 假装服务成功**。
    pub fn read_of(&self, driver_id: Option<&str>) -> Option<Arc<dyn StorageDriver>> {
        match driver_id.filter(|s| !s.is_empty()) {
            Some(id) => self.get(id),
            None => self.get("local"),
        }
    }

    pub fn list(&self) -> Vec<DriverInfo> {
        self.drivers
            .read()
            .iter()
            .map(|r| DriverInfo {
                id: r.key.clone(),
                label: r.driver.label(),
                roots: r.driver.roots(),
            })
            .collect()
    }
}

/// StoragePort 桥：宿主注册进 Ports，notes-core 等消费方经 ctx.ports 取驱动
/// （D-02：消除模块间横向依赖）
pub struct FileStoragePort {
    registry: Arc<DriverRegistry>,
}

impl FileStoragePort {
    pub fn new(registry: Arc<DriverRegistry>) -> Self {
        Self { registry }
    }
}

impl StoragePort for FileStoragePort {
    fn driver(&self, id: &str) -> Option<Arc<dyn StorageDriver>> {
        self.registry.get(id)
    }
    fn list_drivers(&self) -> Vec<DriverInfo> {
        self.registry.list()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_file_driver_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn local_driver_built_in_and_operates() {
        let reg = DriverRegistry::new();
        assert!(reg.get("local").is_some());
        assert!(reg.list().iter().any(|d| d.id == "local"));

        let d = tmpdir("ops");
        let local = reg.get("local").unwrap();
        local.mkdir(&d.join("a/b")).unwrap();
        std::fs::write(d.join("a/b/f.txt"), b"x").unwrap();
        let entries = local.list(&d.join("a/b")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "f.txt");
        local
            .rename(&d.join("a/b/f.txt"), &d.join("a/b/g.txt"))
            .unwrap();
        assert!(d.join("a/b/g.txt").exists());
        local.remove(&d.join("a/b/g.txt"), false).unwrap();
        assert!(!d.join("a/b/g.txt").exists());
        local.remove(&d.join("a"), false).unwrap();
        assert!(!d.join("a").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-3 判据面）字面测试名优先于 rustc 命名惯例
    fn registerAs_dynamicKey_neverTouchesLocal() {
        // 承重① 顶替红线的注册表侧机检：动态键 `remote:…` 入表后，
        // "local" 条目原封不动（同 id 顶替域只在 key 相等时生效）
        struct FakeRemote;
        impl StorageDriver for FakeRemote {
            fn id(&self) -> String {
                "webdav".into()
            }
            fn label(&self) -> String {
                "远端假驱动".into()
            }
            fn roots(&self) -> Vec<PathBuf> {
                vec![]
            }
            fn list(&self, _p: &Path) -> Result<Vec<FileEntry>, AppError> {
                Ok(vec![])
            }
            fn mkdir(&self, _p: &Path) -> Result<(), AppError> {
                Ok(())
            }
            fn remove(&self, _p: &Path, _r: bool) -> Result<(), AppError> {
                Ok(())
            }
            fn rename(&self, _f: &Path, _t: &Path) -> Result<(), AppError> {
                Ok(())
            }
        }
        let reg = DriverRegistry::new();
        let local_before = reg.get("local").unwrap().label();
        reg.register_as("remote:alpha", Arc::new(FakeRemote));
        // 第二枚同为 webdav 自报 id 的动态驱动：互不顶替（去重域是 key 不是 id()）
        reg.register_as("remote:beta", Arc::new(FakeRemote));
        assert_eq!(reg.get("local").unwrap().label(), local_before);
        assert_eq!(reg.list().len(), 3);
        assert!(reg.get("webdav").is_none(), "自报静态 id 不是寻址键");
        assert!(reg.unregister("remote:alpha"));
        assert!(!reg.unregister("remote:alpha"), "幂等注销回 false");
        assert_eq!(reg.list().len(), 2);
    }

    #[test]
    fn dynamic_driver_registration_replaces_same_id() {
        struct Fake;
        impl StorageDriver for Fake {
            fn id(&self) -> String {
                "local".into()
            }
            fn label(&self) -> String {
                "假驱动".into()
            }
            fn roots(&self) -> Vec<PathBuf> {
                vec![]
            }
            fn list(&self, _p: &Path) -> Result<Vec<FileEntry>, AppError> {
                Ok(vec![])
            }
            fn mkdir(&self, _p: &Path) -> Result<(), AppError> {
                Ok(())
            }
            fn remove(&self, _p: &Path, _r: bool) -> Result<(), AppError> {
                Ok(())
            }
            fn rename(&self, _f: &Path, _t: &Path) -> Result<(), AppError> {
                Ok(())
            }
        }
        let reg = DriverRegistry::new();
        reg.register(Arc::new(Fake));
        assert_eq!(reg.get("local").unwrap().label(), "假驱动");
        assert_eq!(reg.list().len(), 1);
    }

    #[test]
    fn storage_port_bridge_resolves_local_driver() {
        // D-02 回归：消费方视角经 StoragePort 拿驱动，行为与注册表一致
        let reg = Arc::new(DriverRegistry::new());
        let port = FileStoragePort::new(reg.clone());
        assert!(port.driver("nope").is_none());
        let local = port.driver("local").expect("local 驱动应在");
        assert_eq!(local.id(), "local");
        assert!(port.list_drivers().iter().any(|d| d.id == "local"));
        // 动态注册即时可见（同一注册表实例）
        port.driver("local").unwrap();
        assert_eq!(reg.list().len(), 1);
    }

    // ---- T-B6-11 驱动抽象泛化（09 §6.2 字面测名）----

    use host_core::storage::DriverCapabilities;

    /// 可编程假远端：动词全记账（trait 形状不塌的端到端替身）
    struct ScriptedRemote {
        calls: Arc<parking_lot::Mutex<Vec<String>>>,
    }

    impl ScriptedRemote {
        fn new() -> Self {
            Self {
                calls: Arc::new(parking_lot::Mutex::new(Vec::new())),
            }
        }
    }

    impl StorageDriver for ScriptedRemote {
        fn id(&self) -> String {
            "scripted".into()
        }
        fn label(&self) -> String {
            "脚本远端".into()
        }
        fn roots(&self) -> Vec<PathBuf> {
            vec!["/srv".into()]
        }
        fn list(&self, _p: &Path) -> Result<Vec<FileEntry>, AppError> {
            self.calls.lock().push("list".into());
            Ok(vec![FileEntry {
                name: "a.txt".into(),
                path: "/srv/a.txt".into(),
                is_dir: false,
                size: 3,
                modified_ms: 0,
                ext: "txt".into(),
                hidden: false,
            }])
        }
        fn mkdir(&self, _p: &Path) -> Result<(), AppError> {
            self.calls.lock().push("mkdir".into());
            Ok(())
        }
        fn remove(&self, _p: &Path, recycle: bool) -> Result<(), AppError> {
            // 假驱动同样守回收站红线：带 recycle 下来即 panic（执行器第三道闸的镜像）
            assert!(!recycle, "远端不得收到 recycle=true");
            self.calls.lock().push("remove".into());
            Ok(())
        }
        fn rename(&self, _f: &Path, _t: &Path) -> Result<(), AppError> {
            self.calls.lock().push("rename".into());
            Ok(())
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    fn remoteDriver_satisfiesStorageDriver() {
        // 编译期：泛型断言 + trait 对象两处都不塌（id()->String 放宽后仍成立）
        fn assert_driver<T: StorageDriver + ?Sized>() {}
        assert_driver::<LocalDriver>();
        assert_driver::<ScriptedRemote>();
        assert_driver::<dyn StorageDriver>();
        // 端到端：假驱动经注册表以 trait 对象走一遍浏览/建目录/删除三动词
        let reg = DriverRegistry::new();
        let fake = ScriptedRemote::new();
        let calls = fake.calls.clone();
        reg.register_as("remote:s", Arc::new(fake));
        let drv = reg.read_of(Some("remote:s")).expect("动态键可寻址");
        let entries = drv.list(Path::new("/srv")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.txt");
        drv.mkdir(Path::new("/srv/sub")).unwrap();
        drv.remove(Path::new("/srv/sub"), false).unwrap();
        assert_eq!(*calls.lock(), vec!["list", "mkdir", "remove"]);
        // 自报 id 是协议名（"scripted"），寻址键是注入的 "remote:s"——两件事
        assert_eq!(drv.id(), "scripted");
        assert!(
            reg.get("scripted").is_none(),
            "自报 id 不是寻址键（语义零变）"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    fn capabilities_localDriverDefaultsUnchanged() {
        // 正对照：LocalDriver 不覆写 capabilities ⇒ 新增位逐字为缺省形
        // （全 false + Whole）——"能力声明"是纯增量面，既有本地行为零扰动
        let caps = LocalDriver.capabilities();
        assert_eq!(caps, DriverCapabilities::default());
        assert!(!caps.browse && !caps.mkcol && !caps.delete);
        assert!(!caps.permanent_delete_only && !caps.rename_same_driver);
        assert_eq!(
            caps.resume,
            host_core::storage::Resumable::Whole,
            "未声明即 Whole（不承诺任何断点）"
        );
        // 未覆写流式腿的驱动同样落在缺省声明上（假远端只做了动词，没做腿）
        assert_eq!(
            ScriptedRemote::new().capabilities(),
            DriverCapabilities::default()
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    fn readStream_unimplementedDriver_refusesNotPanics() {
        // 承重⑨：默认臂是 Err 而非 unwrap/panic——未实现流式的驱动对
        // read_stream/write_stream 都诚实拒绝，码表沿用 FILE_OPS_005
        let r = LocalDriver.read_stream(Path::new("C:/x"), 0);
        let e = match r {
            Err(e) => e,
            Ok(_) => panic!("本地驱动不该有 read_stream 腿（本行只声明不接线）"),
        };
        assert_eq!(e.code(), "FILE_OPS_005");
        let w = ScriptedRemote::new().write_stream(Path::new("/srv/x"));
        let e = match w {
            Err(e) => e,
            Ok(_) => panic!("未实现写流的假驱动必须走默认臂拒绝（不得给出门）"),
        };
        assert_eq!(e.code(), "FILE_OPS_005");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    fn registry_unregister_keepsLocalDriver() {
        // 承重①(c)：注销一个远端 id 后 "local" 仍在原位（注册表退役不连坐）
        let reg = DriverRegistry::new();
        reg.register_as("remote:x", Arc::new(ScriptedRemote::new()));
        assert_eq!(reg.list().len(), 2);
        assert!(reg.unregister("remote:x"));
        assert!(!reg.unregister("remote:x"), "幂等注销回 false");
        let local = reg.read_of(None).expect("local 恒在默认臂");
        assert_eq!(local.id(), "local");
        assert_eq!(local.label(), "本地磁盘");
        assert!(
            reg.read_of(Some("remote:x")).is_none(),
            "read_of 不回落 local——取不到就是取不到"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名优先于 rustc 命名惯例
    fn sameIdRegistration_stillReplacesWithinNamespace() {
        // 保留既有语义（dynamic_driver_registration_replaces_same_id 的同谱
        // 正名版）：register() 顶替同自报 id 的行为对 "local" 命名空间不变——
        // 防"为远端而破坏本地注册语义"
        struct Dup;
        impl StorageDriver for Dup {
            fn id(&self) -> String {
                "local".into()
            }
            fn label(&self) -> String {
                "第二次注册者".into()
            }
            fn roots(&self) -> Vec<PathBuf> {
                vec![]
            }
            fn list(&self, _p: &Path) -> Result<Vec<FileEntry>, AppError> {
                Ok(vec![])
            }
            fn mkdir(&self, _p: &Path) -> Result<(), AppError> {
                Ok(())
            }
            fn remove(&self, _p: &Path, _r: bool) -> Result<(), AppError> {
                Ok(())
            }
            fn rename(&self, _f: &Path, _t: &Path) -> Result<(), AppError> {
                Ok(())
            }
        }
        let reg = DriverRegistry::new();
        reg.register(Arc::new(ScriptedRemote::new())); // id=scripted，不撞 local
        reg.register(Arc::new(Dup)); // id=local ⇒ 顶替默认条目
        assert_eq!(reg.list().len(), 2, "scripted 条目不受 local 顶替连坐");
        assert_eq!(reg.get("local").unwrap().label(), "第二次注册者");
    }
}
