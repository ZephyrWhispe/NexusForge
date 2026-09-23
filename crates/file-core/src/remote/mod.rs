//! B6 远端面（09 §6.2 T-B6-3 起）。
//!
//! 承重①(b)：远端路径全程是 `/` 分隔的 `String`（[`RemoteEntry::path`]），
//! `PathBuf` 只允许在 [`to_file_entry`] 一处物化（前端浏览表沿用 FileEntry
//! 形状的代价；协议行 webdav.rs 本体 grep `PathBuf` 恒为 0，批次判据钉住）。
//! 承重⑫：[`AuthSecret`] 只进不出——手写 `Deserialize`、手写 `Debug`（只报
//! 在场），**故意不实现 `Serialize`**，凭据值不可能出现在任何命令返回体。

pub mod ftp;
pub mod http;
pub mod ssh;
pub mod webdav;

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

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
/// 凭据字段在类型层面就不存在 ⇒ `webdavAuth_secretNeverSerialized` 的判据面）。
/// `auth_source`（T-B6-8）只报凭据**来源**不报值——B5 `addr_source` 单源纪律的
/// 连接面对镜像。
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
    pub auth_source: crate::profile::AuthSource,
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

/// SFTP 驱动有完整浏览与写面（不像 HTTPS 只有下载腿）。TOFU 守卫不在这里——
/// 它在分派口 [`crate::service::FileService::connect`] 建连前已毕，且每条
/// 操作会话在 [`ssh::RusshBackend`] 内以 store 二次校验；驱动侧再设第二道
/// 回收站闸（承重⑥，与 WebDAV 臂同一口径）。
impl StorageDriver for ssh::SftpDriver {
    fn id(&self) -> &'static str {
        "sftp"
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

/// FTP 与 WebDAV 同谱：完整浏览与写面，回收站恒拒（承重⑥第二道闸）；
/// 明文总闸不在这里——它在 [`crate::service::FileService::connect`]
/// 建驱动之前已由 [`ftp::ftp_plaintext_guard`] 毕（驱动侧不设第二张闸皮）。
impl StorageDriver for ftp::FtpDriver {
    fn id(&self) -> &'static str {
        "ftp"
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

// ---------------------------------------------------------------------------
// 限速令牌桶（09 §6.2 T-B6-6：`download_kbps` 的执行体；upload 侧随 T-B6-7）
// ---------------------------------------------------------------------------

/// 纯核裁决：本批 `bytes` 消费后需暂停多久。`rate_bytes_per_sec = 0` ⇒ 恒
/// `Duration::ZERO`（0 = 不限，配置真源的诚实语义）。预算随真实流逝补给、
/// 封顶一秒流量（禁攒出"开机到现在没下载"的突发豁免）。
/// 偏离任务书签名（`allow(bytes,&mut Instant,&mut u64)`）登记为落地补记：
/// 无 rate 参数的桶不是桶，是计时器。
pub fn throttle_allow(
    bytes: u64,
    last: &mut Instant,
    budget: &mut u64,
    rate_bytes_per_sec: u64,
) -> Duration {
    if rate_bytes_per_sec == 0 {
        return Duration::ZERO;
    }
    let now = Instant::now();
    let refill = now
        .checked_duration_since(*last)
        .map(|d| (d.as_secs_f64() * rate_bytes_per_sec as f64) as u64)
        .unwrap_or(0);
    *budget = (*budget + refill).min(rate_bytes_per_sec);
    *last = now;
    let cost = bytes.max(1);
    if *budget >= cost {
        *budget -= cost;
        Duration::ZERO
    } else {
        let deficit = cost - *budget;
        *budget = 0;
        Duration::from_secs_f64(deficit as f64 / rate_bytes_per_sec as f64)
    }
}

/// 带状态的桶（协议腿泵用）：包一层 `(rate, last, budget)` 三态
#[derive(Clone, Debug)]
pub struct ThrottleGate {
    rate_bytes_per_sec: u64,
    last: Instant,
    budget: u64,
}

impl ThrottleGate {
    /// `kbps = 0` ⇒ 不限速
    pub fn new(kbps: u32) -> Self {
        Self {
            rate_bytes_per_sec: kbps as u64 * 1024,
            last: Instant::now(),
            budget: 0,
        }
    }
    pub fn is_unlimited(&self) -> bool {
        self.rate_bytes_per_sec == 0
    }
    /// 消费 `bytes`，返回调用方应 sleep 的时长（ZERO = 放行）
    pub fn allow(&mut self, bytes: u64) -> Duration {
        throttle_allow(
            bytes,
            &mut self.last,
            &mut self.budget,
            self.rate_bytes_per_sec,
        )
    }
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
            auth_source: crate::profile::AuthSource::Typed,
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
                "auth_source",
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

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-6）字面测试名优先于 rustc 命名惯例
    fn throttleGate_zeroMeansUnlimited() {
        // 0 = 不限：天文数字连发也不暂停一拍
        let mut gate = ThrottleGate::new(0);
        assert!(gate.is_unlimited());
        for _ in 0..64 {
            assert_eq!(
                gate.allow(1 << 30),
                Duration::ZERO,
                "0 值闸不得积累任何暂停"
            );
        }
        // 正对照：10 KB/s 桶下 20 KB 一次性消费必须回暂停时长（>0），
        // 且消息面（is_unlimited）如实翻转
        let mut gate = ThrottleGate::new(10);
        assert!(!gate.is_unlimited());
        let pause = gate.allow(20 * 1024);
        assert!(
            pause > Duration::ZERO && pause < Duration::from_secs(5),
            "20KB 走 10KB/s 桶应暂停约 2s，实得 {pause:?}"
        );
        // 纯核臂：补给封顶一秒流量——last 拨回一小时也不许攒出突发豁免
        let mut last = Instant::now() - Duration::from_secs(3600);
        let mut budget = 0u64;
        let pause = throttle_allow(2048, &mut last, &mut budget, 1024);
        assert!(
            pause >= Duration::ZERO && pause < Duration::from_millis(1200),
            "补给封顶 1s 流量：2KB 至多等约 1s，实得 {pause:?}"
        );
        // 0 速率纯核臂：状态位一个都不许动（缺省即透明）
        let mut last2 = Instant::now();
        let keep = last2;
        let mut budget2 = 42u64;
        assert_eq!(
            throttle_allow(1 << 20, &mut last2, &mut budget2, 0),
            Duration::ZERO
        );
        assert_eq!(last2, keep, "0=不限不得篡改时间事实源");
        assert_eq!(budget2, 42, "0=不限不得动预算");
    }
}
