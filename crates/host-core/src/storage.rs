//! 存储驱动契约（D-02 上移自 file-core::driver）：跨模块共享的存储抽象。
//!
//! 规则：notes-core 等消费方一律依赖本 trait 与 [`StoragePort`]，**禁止**直接
//! 依赖 file-core（DESIGN O1 模块间零横向依赖）；file-core 提供 LocalDriver
//! 实现与注册桥（FileStoragePort），由宿主 src-tauri 注册进 Ports。
//! 错误类型统一 AppError（模块内 FileError 经 From 转换后跨层传播）。

use std::io::{Read, Write};
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
    /// Unix 权限位（低 12 位，含 setuid/setgid/sticky）。None = 无事实源
    /// （Windows 本地盘与无权限语义的协议腿）——面板属性页该行不显示。
    /// `serde(default)`：加键=旧快照可读（T-B7-25 双域旧读兼容）。
    #[serde(default)]
    pub mode: Option<u32>,
    /// 符号链接目标（字面路径串）。None = 非链接或无事实源。
    #[serde(default)]
    pub symlink_target: Option<String>,
}

/// 续传档位（T-B6-11 自 file-core::remote::http 上移——[`DriverCapabilities`]
/// 引用它，而 host-core 不得反向依赖 file-core；D-02 同谱先例）。
/// serde 形状逐字不变（snake_case），`Resumable` 的 wire 值零迁移。
/// 语义纪律随行搬移：`Range` = 对端真回了 206 才承诺；`Append` 预留给
/// SFTP/FTP 续写腿；`Whole` = 只能整取——无事实源就不承诺。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resumable {
    Range,
    Append,
    Whole,
}

/// 驱动能力只读声明（09 §6.2 T-B6-11）：**缺省全 false + Whole**——未声明的
/// 能力就是没有（假就绪红线的类型形态）。消费方是 T-B6-11 的远端队列执行器
/// 与诚实拒绝消息；[`LocalDriver`](crate 内) 不覆写本方法即行为零变。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct DriverCapabilities {
    /// 有列目录面（HTTPS 下载腿为 false——拒绝而非空表）
    pub browse: bool,
    pub mkcol: bool,
    pub delete: bool,
    /// 删除只能彻底（远端无回收站，承重⑥的声明面）
    pub permanent_delete_only: bool,
    /// 断点续取档位声明；无腿即 Whole
    pub resume: Resumable,
    /// rename 只在同驱动内成立（跨驱动移动 = 复制+删除，禁假"移动"语义）
    pub rename_same_driver: bool,
}

impl Default for DriverCapabilities {
    fn default() -> Self {
        Self {
            browse: false,
            mkcol: false,
            delete: false,
            permanent_delete_only: false,
            resume: Resumable::Whole,
            rename_same_driver: false,
        }
    }
}

/// 写流收口柄（T-B6-11）：`Box<dyn Write + Send>` 给不出"提交是否成功"的
/// 事实源（STOR 的 226 / PUT 的响应码都在收尾才出现），drop 又吞错——
/// 故写腿的完成裁决走显式 [`finish`](WriteCommit::finish)，泵完字节 ≠ 传成。
pub trait WriteCommit: Write + Send {
    /// 冲刷并关闭传输、校验对端收尾回执；失败 Err 点名（幂等性由调用方
    /// 的"finish 后不再写"纪律保证）
    fn finish(self: Box<Self>) -> Result<(), AppError>;
}

/// 网盘/存储驱动抽象（docs/impl/05 F6）
pub trait StorageDriver: Send + Sync {
    /// 唯一标识（T-B6-11 自 `&'static str` 放宽为 `String`：动态
    /// `remote:{profile_id}` 得以自报）。**自报 id ≠ 寻址键**——注册表寻址
    /// 仍走 `register_as` 注入的键（同 id 多档案互不顶替的语义零变）。
    fn id(&self) -> String;
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
    /// 能力只读声明（缺省全 false——不覆写即"只会本地三动词"的诚实形状）
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities::default()
    }
    /// 流式读（T-B6-11）：返回的读流**恰从 `offset` 字节起**——对端不履约
    /// 即 Err 点名（禁静默从头给字节）。未实现者走默认臂诚实拒绝，
    /// 既有 `Unsupported(_) => "FILE_OPS_005"` 码表逐字不动。
    fn read_stream(&self, _path: &Path, _offset: u64) -> Result<Box<dyn Read + Send>, AppError> {
        Err(AppError::module(
            "FILE_OPS_005",
            "该驱动不支持 read_stream",
            None,
        ))
    }
    /// 流式写（T-B6-11）：完成裁决在 [`WriteCommit::finish`]（泵完 ≠ 传成）。
    fn write_stream(&self, _path: &Path) -> Result<Box<dyn WriteCommit>, AppError> {
        Err(AppError::module(
            "FILE_OPS_005",
            "该驱动不支持 write_stream",
            None,
        ))
    }
    /// 读取文件内容（notes-core N5 多存储后端复用；远程驱动按能力实现）
    fn read_file(&self, _path: &Path) -> Result<Vec<u8>, AppError> {
        Err(AppError::module(
            "FILE_OPS_005",
            "该驱动不支持 read_file",
            None,
        ))
    }
    /// 写出文件内容（驱动负责建父目录；本地实现走 tmp+rename 原子替换）
    fn write_file(&self, _path: &Path, _data: &[u8]) -> Result<(), AppError> {
        Err(AppError::module(
            "FILE_OPS_005",
            "该驱动不支持 write_file",
            None,
        ))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// T-B7-25 双域旧读兼容（host-core 侧一枚；RemoteEntry 侧同名测在
    /// file-core remote/mod.rs）：**手写的旧七键字节**必须原样读进新结构，
    /// 新两键落 None=无事实源（不是 0、不是空串——那都是假装有值）。
    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-25）字面测试名优先于 rustc 命名惯例
    fn mode_serdeDefault_legacyEntriesJson_stillReads() {
        let legacy = r#"{"name":"a.txt","path":"C:\\x\\a.txt","is_dir":false,"size":3,
            "modified_ms":1700000000000,"ext":"txt","hidden":false}"#;
        let e: FileEntry = serde_json::from_str(legacy).expect("旧七键快照必须可读（零迁移红线）");
        assert_eq!(e.name, "a.txt");
        assert_eq!(e.mode, None, "旧数据无权限位事实源⇒None，不得回落 0o000");
        assert_eq!(e.symlink_target, None);
        // 正对照：新键在场则原样读出（写侧字节夹具回环）
        let with = serde_json::to_string(&e).unwrap();
        let back: FileEntry = serde_json::from_str(&with).unwrap();
        assert_eq!(back.mode, None);
        let e2 = FileEntry {
            mode: Some(0o644),
            symlink_target: Some("/etc/passwd".into()),
            ..e.clone()
        };
        let json = serde_json::to_string(&e2).unwrap();
        let back2: FileEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back2.mode, Some(0o644));
        assert_eq!(back2.symlink_target.as_deref(), Some("/etc/passwd"));
    }
}
