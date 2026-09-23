//! B6 远端面（09 §6.2 T-B6-3 起）。
//!
//! 承重①(b)：远端路径全程是 `/` 分隔的 `String`（[`RemoteEntry::path`]），
//! `PathBuf` 只允许在 [`to_file_entry`] 一处物化（前端浏览表沿用 FileEntry
//! 形状的代价；协议行 webdav.rs 本体 grep `PathBuf` 恒为 0，批次判据钉住）。
//! 承重⑫：[`AuthSecret`] 只进不出——手写 `Deserialize`、手写 `Debug`（只报
//! 在场），**故意不实现 `Serialize`**，凭据值不可能出现在任何命令返回体。

pub mod http;
pub mod webdav;

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use host_core::storage::{FileEntry, StorageDriver};
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{FileError, FILE_REMOTE_FIELD};

pub use http::{
    classify_resume, range_plan, remote_error_message, resume_offset, throttle_share_kbps,
    DownloadOutcome, HttpsDriver, Resumable,
};
pub use webdav::WebDavDriver;

/// 远端目录条目（路径为服务端给定的百分号编码形状，`/` 分隔；解码只发生在
/// `name` 展示面——协议细节见 [`webdav`]）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_ms: i64,
}

fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(0) | None => String::new(),
        Some(i) => name[i + 1..].to_lowercase(),
    }
}

impl RemoteEntry {
    /// **全模块唯一 PathBuf 物化点**（承重①(b) 的边界）：把斜杠远程路径塞进
    /// FileEntry.path 只为复用前端浏览表形状，任何 fs 语义都不得从它派生。
    pub fn to_file_entry(&self) -> FileEntry {
        FileEntry {
            name: self.name.clone(),
            path: PathBuf::from(&self.path),
            is_dir: self.is_dir,
            size: self.size,
            modified_ms: self.modified_ms,
            ext: ext_of(&self.name),
            hidden: self.name.starts_with('.'),
        }
    }
}

/// 连接期凭据（只进不出，承重⑫）：`header` = 完整 Authorization 值，
/// `password` = 逐次口令（Basic 由 user + 它现拼）。两枚至多一枚在场。
pub struct AuthSecret {
    pub header: Option<Zeroizing<String>>,
    pub password: Option<Zeroizing<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct AuthSecretRaw {
    header: Option<String>,
    password: Option<String>,
}

impl<'de> Deserialize<'de> for AuthSecret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = AuthSecretRaw::deserialize(d)?;
        Ok(AuthSecret {
            header: raw.header.map(Zeroizing::new),
            password: raw.password.map(Zeroizing::new),
        })
    }
}

impl fmt::Debug for AuthSecret {
    /// 只报在场，不报值（任何日志/Debug 面都摸不到口令）
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthSecret")
            .field(
                "header",
                &self.header.as_ref().map(|_| "<present, redacted>"),
            )
            .field(
                "password",
                &self.password.as_ref().map(|_| "<present, redacted>"),
            )
            .finish()
    }
}

/// 已连接远端驱动的对外描述（命令返回体：键集恒等于本结构字段集，
/// 凭据字段在类型层面就不存在 ⇒ `webdavAuth_secretNeverSerialized` 的判据面）
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RemoteDriverInfo {
    pub driver_id: String,
    pub label: String,
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub base_path: String,
    /// 远端根（String 形状——承重⑮ 的另一份事实源来自 profile.base_path）
    pub roots: Vec<String>,
}

// 专用多线程小 runtime：StorageDriver trait 是同步的（worker 线程直接调用），
// 而 reqwest 是异步的——桥接放这里，协议文件保持 async 纯净。
fn remote_rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("nf-file-remote")
            .enable_all()
            .build()
            .expect("file-core remote runtime 构建失败")
    })
}

/// 从任意线程（含 tokio 上下文，如 `#[tokio::test]`）同步等待远端 IO：
/// spawn 到专用 runtime + 通道阻塞收取，避开「在同一 current-thread
/// runtime 上嵌套 block_on」的重入限制。
pub(crate) fn remote_block_on<T: Send + 'static>(
    fut: impl Future<Output = T> + Send + 'static,
) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    remote_rt().handle().clone().spawn(async move {
        let _ = tx.send(fut.await);
    });
    rx.recv()
        .expect("file-core remote runtime 意外停摆，远端请求无法收取")
}

/// 供 WebDavDriver::new 在 runtime 上下文内构建 reqwest::Client
pub(crate) fn remote_enter() -> tokio::runtime::EnterGuard<'static> {
    remote_rt().enter()
}

impl StorageDriver for WebDavDriver {
    /// 自报 id 是协议名（`&'static str` 契约）；寻址键 `remote:{profile_id}`
    /// 由 [`crate::driver::DriverRegistry::register_as`] 注入，两者不是一回事。
    fn id(&self) -> &'static str {
        "webdav"
    }
    fn label(&self) -> String {
        self.driver_label()
    }
    fn roots(&self) -> Vec<PathBuf> {
        vec![self.base_path().into()]
    }
    fn list(&self, path: &Path) -> Result<Vec<FileEntry>, host_core::error::AppError> {
        let entries = self
            .list_entries(&path.to_string_lossy())
            .map_err(host_core::error::AppError::from)?;
        Ok(entries.iter().map(RemoteEntry::to_file_entry).collect())
    }
    fn mkdir(&self, path: &Path) -> Result<(), host_core::error::AppError> {
        self.mkdir_remote(&path.to_string_lossy())
            .map_err(host_core::error::AppError::from)
    }
    fn remove(&self, path: &Path, recycle: bool) -> Result<(), host_core::error::AppError> {
        if recycle {
            // 承重⑥：远端无回收站（入队闸已在 T-B6-2 立，这里是驱动侧第二道）
            return Err(
                FileError::Unsupported("远端驱动不支持回收站：请改用彻底删除".into()).into(),
            );
        }
        self.remove_remote(&path.to_string_lossy())
            .map_err(host_core::error::AppError::from)
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<(), host_core::error::AppError> {
        self.rename_remote(&from.to_string_lossy(), &to.to_string_lossy())
            .map_err(host_core::error::AppError::from)
    }
}

/// 承重⑨：HTTPS 驱动**只有下载腿**——浏览/建目录/删除/改名一律诚实拒绝
/// （假就绪 = 空表，拒才是诚实）。下载面见 [`http::HttpsDriver::download_to`]。
impl StorageDriver for HttpsDriver {
    fn id(&self) -> &'static str {
        "https"
    }
    fn label(&self) -> String {
        self.driver_label()
    }
    fn roots(&self) -> Vec<PathBuf> {
        vec![self.base_path().into()]
    }
    fn list(&self, _path: &Path) -> Result<Vec<FileEntry>, host_core::error::AppError> {
        Err(no_browse_err())
    }
    fn mkdir(&self, _path: &Path) -> Result<(), host_core::error::AppError> {
        Err(no_browse_err())
    }
    fn remove(&self, _path: &Path, _recycle: bool) -> Result<(), host_core::error::AppError> {
        Err(no_browse_err())
    }
    fn rename(&self, _from: &Path, _to: &Path) -> Result<(), host_core::error::AppError> {
        Err(no_browse_err())
    }
}

fn no_browse_err() -> host_core::error::AppError {
    FileError::Remote {
        code: FILE_REMOTE_FIELD,
        msg: "HTTP 下载源不支持浏览：该档案只有下载腿（GET/Range），无列目录与写面".into(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-3）字面测试名优先于 rustc 命名惯例
    fn webdavList_hrefsNeverBecomePathBuf() {
        // 承重①(b) 机检面（代码侧）：协议解析产出的 path 是斜杠 String，
        // 逐字保真服务端编码形状；唯一 PathBuf 物化在 to_file_entry，
        // 且只回显同一字符串——grep "PathBuf" webdav.rs == 0 由批次判据钉。
        let e = RemoteEntry {
            name: "报告 2026.txt".into(),
            path: "/dav/%E6%8A%A5%E5%91%8A%202026.txt".into(),
            is_dir: false,
            size: 7,
            modified_ms: 0,
        };
        let fe = e.to_file_entry();
        assert_eq!(fe.path.to_string_lossy(), e.path, "边界只回显，不重排斜杠");
        assert_eq!(fe.name, e.name);
        assert_eq!(fe.ext, "txt");
        assert_eq!(
            std::fs::metadata(&fe.path).err().map(|e| e.kind()),
            Some(std::io::ErrorKind::NotFound),
            "物化路径不得被当作本地事实源（它只是展示形状）"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-3）字面测试名优先于 rustc 命名惯例
    fn webdavAuth_secretNeverSerialized() {
        // 行为面证据链：① AuthSecret 类型没有 Serialize（本测试能通过编译即
        // 反证——下面只调 Deserialize/Debug）；② 命令返回体 RemoteDriverInfo
        // 键集恰等其字段集，无 header/password 两键；③ Debug 只报在场。
        let secret: AuthSecret =
            serde_json::from_str(r#"{"header":null,"password":"sesame-open"}"#).unwrap();
        let dbg = format!("{secret:?}");
        assert!(!dbg.contains("sesame"), "Debug 面泄露口令: {dbg}");
        assert!(dbg.contains("present"), "正对照：Debug 须报出在场性");
        let info = RemoteDriverInfo {
            driver_id: "remote:t3".into(),
            label: "WebDAV t3".into(),
            protocol: "webdav".into(),
            host: "dav.example.com".into(),
            port: 443,
            base_path: "/dav".into(),
            roots: vec!["/dav".into()],
        };
        let json: serde_json::Value = serde_json::to_value(&info).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "base_path",
                "driver_id",
                "host",
                "label",
                "port",
                "protocol",
                "roots"
            ],
            "命令返回体键集不得含凭据位"
        );
        // 非法入参 fail-closed：多余凭据外的未知键拒收（deny_unknown_fields）
        assert!(serde_json::from_str::<AuthSecret>(r#"{"nope":"x"}"#).is_err());
    }
}
