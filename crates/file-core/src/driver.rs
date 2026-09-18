//! F6 存储驱动抽象（docs/impl/05 F6）：统一 list/read/write/mkdir/remove/move/quota。
//!
//! D-02：trait 与 DTO 已上移至 host-core::storage（notes-core 等消费方经
//! [`host_core::storage::StoragePort`] 取驱动，不直依本 crate）；本文件保留
//! 本地驱动实现与注册表。v1 内置 LocalDriver（本地盘/UNC）；smb/ftp/webdav/s3
//! 经 rclone sidecar 包装驱动在后续里程碑接入（注册表已留扩展位）。

use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use host_core::error::AppError;
use host_core::storage::StoragePort;

// D-02：trait/DTO 住 host-core::storage；此处 pub use 保持 file_core::driver::* API
pub use host_core::storage::{DriverInfo, FileEntry, StorageDriver};

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
    fn id(&self) -> &'static str {
        "local"
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

/// 驱动注册表：静态内置 + 动态注册（rclone sidecar 预留）
pub struct DriverRegistry {
    drivers: RwLock<Vec<Arc<dyn StorageDriver>>>,
}

impl Default for DriverRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self {
            drivers: RwLock::new(vec![Arc::new(LocalDriver)]),
        }
    }

    /// 注册驱动（同 id 重复注册以最后者为准）
    pub fn register(&self, driver: Arc<dyn StorageDriver>) {
        let mut v = self.drivers.write();
        v.retain(|d| d.id() != driver.id());
        v.push(driver);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn StorageDriver>> {
        self.drivers.read().iter().find(|d| d.id() == id).cloned()
    }

    pub fn list(&self) -> Vec<DriverInfo> {
        self.drivers
            .read()
            .iter()
            .map(|d| DriverInfo {
                id: d.id().to_owned(),
                label: d.label(),
                roots: d.roots(),
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
    fn dynamic_driver_registration_replaces_same_id() {
        struct Fake;
        impl StorageDriver for Fake {
            fn id(&self) -> &'static str {
                "local"
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
}
