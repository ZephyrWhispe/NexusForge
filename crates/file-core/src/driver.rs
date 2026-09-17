//! F6 存储驱动抽象（docs/impl/05 F6）：统一 list/read/write/mkdir/remove/move/quota。
//!
//! v1 内置 LocalDriver（本地盘/UNC）；smb/ftp/webdav/s3 经 rclone sidecar 包装驱动
//! 在后续里程碑接入（注册表已留扩展位）。UI 呈现统一目录树。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::Serialize;

use crate::browse::{self, FileEntry};
use crate::error::FileError;

/// 网盘/存储驱动抽象（docs/impl/05 F6）
pub trait StorageDriver: Send + Sync {
    /// 唯一标识："local" | "smb" | "webdav" | ...
    fn id(&self) -> &'static str;
    /// UI 显示名
    fn label(&self) -> String;
    /// 该驱动呈现的根目录（本地盘符 / 挂载点）
    fn roots(&self) -> Vec<PathBuf>;
    fn list(&self, path: &Path) -> Result<Vec<FileEntry>, FileError>;
    fn mkdir(&self, path: &Path) -> Result<(), FileError>;
    /// remove(false) 走直删；recycle 参数仅本地驱动支持
    fn remove(&self, path: &Path, recycle: bool) -> Result<(), FileError>;
    /// move/rename（同驱动内）
    fn rename(&self, from: &Path, to: &Path) -> Result<(), FileError>;
    /// (free, total)；无配额概念返回 None
    fn quota(&self, _path: &Path) -> Option<(u64, u64)> {
        None
    }
    /// 读取文件内容（notes-core N5 多存储后端复用；远程驱动按能力实现）
    fn read_file(&self, _path: &Path) -> Result<Vec<u8>, FileError> {
        Err(FileError::Unsupported("read_file".into()))
    }
    /// 写出文件内容（驱动负责建父目录；本地实现走 tmp+rename 原子替换）
    fn write_file(&self, _path: &Path, _data: &[u8]) -> Result<(), FileError> {
        Err(FileError::Unsupported("write_file".into()))
    }
}

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
    fn list(&self, path: &Path) -> Result<Vec<FileEntry>, FileError> {
        browse::list_dir(path, browse::SortKey::Name, true)
    }
    fn mkdir(&self, path: &Path) -> Result<(), FileError> {
        std::fs::create_dir_all(crate::browse::to_long_path(path))?;
        Ok(())
    }
    fn remove(&self, path: &Path, _recycle: bool) -> Result<(), FileError> {
        let long = crate::browse::to_long_path(path);
        if long.is_dir() {
            std::fs::remove_dir_all(&long)?;
        } else if long.is_file() {
            std::fs::remove_file(&long)?;
        } else {
            return Err(FileError::NotFound(
                crate::browse::display_path(path).to_string_lossy().into_owned(),
            ));
        }
        Ok(())
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<(), FileError> {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(crate::browse::to_long_path(parent))?;
        }
        std::fs::rename(crate::browse::to_long_path(from), crate::browse::to_long_path(to))?;
        Ok(())
    }
    fn read_file(&self, path: &Path) -> Result<Vec<u8>, FileError> {
        Ok(std::fs::read(crate::browse::to_long_path(path))?)
    }
    fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), FileError> {
        let long = crate::browse::to_long_path(path);
        if let Some(parent) = long.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // tmp + rename 原子替换，防半写损坏
        let tmp = long.with_extension("nf-tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, &long)?;
        Ok(())
    }
}

/// 驱动描述（IPC DTO）
#[derive(Clone, Debug, Serialize)]
pub struct DriverInfo {
    pub id: String,
    pub label: String,
    pub roots: Vec<PathBuf>,
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
        Self { drivers: RwLock::new(vec![Arc::new(LocalDriver)]) }
    }

    /// 注册驱动（同 id 重复注册以最后者为准）
    pub fn register(&self, driver: Arc<dyn StorageDriver>) {
        let mut v = self.drivers.write().expect("驱动注册表写锁");
        v.retain(|d| d.id() != driver.id());
        v.push(driver);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn StorageDriver>> {
        self.drivers
            .read()
            .expect("驱动注册表读锁")
            .iter()
            .find(|d| d.id() == id)
            .cloned()
    }

    pub fn list(&self) -> Vec<DriverInfo> {
        self.drivers
            .read()
            .expect("驱动注册表读锁")
            .iter()
            .map(|d| DriverInfo {
                id: d.id().to_owned(),
                label: d.label(),
                roots: d.roots(),
            })
            .collect()
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
        local.rename(&d.join("a/b/f.txt"), &d.join("a/b/g.txt")).unwrap();
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
            fn list(&self, _p: &Path) -> Result<Vec<FileEntry>, FileError> {
                Ok(vec![])
            }
            fn mkdir(&self, _p: &Path) -> Result<(), FileError> {
                Ok(())
            }
            fn remove(&self, _p: &Path, _r: bool) -> Result<(), FileError> {
                Ok(())
            }
            fn rename(&self, _f: &Path, _t: &Path) -> Result<(), FileError> {
                Ok(())
            }
        }
        let reg = DriverRegistry::new();
        reg.register(Arc::new(Fake));
        assert_eq!(reg.get("local").unwrap().label(), "假驱动");
        assert_eq!(reg.list().len(), 1);
    }
}
