//! 存储驱动契约（D-02 上移自 file-core::driver）：跨模块共享的存储抽象。
//!
//! 规则：notes-core 等消费方一律依赖本 trait 与 [`StoragePort`]，**禁止**直接
//! 依赖 file-core（DESIGN O1 模块间零横向依赖）；file-core 提供 LocalDriver
//! 实现与注册桥（FileStoragePort），由宿主 src-tauri 注册进 Ports。
//! 错误类型统一 AppError（模块内 FileError 经 From 转换后跨层传播）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::ports::Port;

/// 目录条目（IPC DTO，字段与前端 FileEntryDto 对齐——缺字段是静默失败重灾区）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    /// 原始路径（展示用，不带 \\?\ 前缀）
    pub path: PathBuf,
    pub is_dir: bool,
    /// 字节；目录为 0
    pub size: u64,
    /// 毫秒时间戳（前端 new Date(ms) 直用）
    pub modified_ms: i64,
    /// 小写扩展名（不含点，目录为空）
    pub ext: String,
    pub hidden: bool,
}

/// 网盘/存储驱动抽象（docs/impl/05 F6）
pub trait StorageDriver: Send + Sync {
    /// 唯一标识："local" | "smb" | "webdav" | ...
    fn id(&self) -> &'static str;
    /// UI 显示名
    fn label(&self) -> String;
    /// 该驱动呈现的根目录（本地盘符 / 挂载点）
    fn roots(&self) -> Vec<PathBuf>;
    fn list(&self, path: &Path) -> Result<Vec<FileEntry>, AppError>;
    fn mkdir(&self, path: &Path) -> Result<(), AppError>;
    /// remove(false) 走直删；recycle 参数仅本地驱动支持
    fn remove(&self, path: &Path, recycle: bool) -> Result<(), AppError>;
    /// move/rename（同驱动内）
    fn rename(&self, from: &Path, to: &Path) -> Result<(), AppError>;
    /// (free, total)；无配额概念返回 None
    fn quota(&self, _path: &Path) -> Option<(u64, u64)> {
        None
    }
    /// 读取文件内容（notes-core N5 多存储后端复用；远程驱动按能力实现）
    fn read_file(&self, _path: &Path) -> Result<Vec<u8>, AppError> {
        Err(AppError::module("FILE_OPS_005", "该驱动不支持 read_file", None))
    }
    /// 写出文件内容（驱动负责建父目录；本地实现走 tmp+rename 原子替换）
    fn write_file(&self, _path: &Path, _data: &[u8]) -> Result<(), AppError> {
        Err(AppError::module("FILE_OPS_005", "该驱动不支持 write_file", None))
    }
}

/// 驱动描述（IPC DTO）
#[derive(Clone, Debug, Serialize)]
pub struct DriverInfo {
    pub id: String,
    pub label: String,
    pub roots: Vec<PathBuf>,
}

/// 存储驱动访问端口：file-core 实现（包装 DriverRegistry），宿主注册；
/// notes-core 等消费方经 ctx.ports 取驱动，不直依 file-core。
pub trait StoragePort: Port {
    fn driver(&self, id: &str) -> Option<Arc<dyn StorageDriver>>;
    fn list_drivers(&self) -> Vec<DriverInfo>;
}
