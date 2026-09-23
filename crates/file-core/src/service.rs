//! FileService（F1–F7 门面）：IPC 层唯一入口；阻塞 IO 由调用方 spawn_blocking。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use host_core::events::{merged_window, Event, EventBus};
use host_core::ports::{Ports, RecycleBinPort, ThumbPort, UsnIndexPort};

use crate::browse::{self, DriveInfo, FileEntry, SortKey};
use crate::conflict::{scan_conflicts, ConflictItem, ConflictPolicy};
use crate::driver::{DriverInfo, DriverRegistry};
use crate::error::{FileError, FILE_REMOTE_FIELD, FILE_REMOTE_MISSING, FILE_REMOTE_NOTIMPL};
use crate::ops::{OpProgress, OpQueue, OpSpec, PendingOp};
use crate::preview::{preview_file, Preview};
use crate::profile::{ProfileStore, RemoteProtocol};
use crate::remote::webdav::WebDavDriver;
use crate::remote::{AuthSecret, HttpsDriver, RemoteDriverInfo, RemoteEntry};
use crate::rename::{apply_plan, build_plan, RenamePlan, RenameRule};
use crate::search::{self, SearchOpts, SearchResult};

/// 已连接远端：连接态是**进程内**事实（不落盘——重启即"未连接"的真实反映，
/// 禁把连接态持久化成假连接，09 §6.2 T-B6-3 数据变更栏）
#[derive(Clone)]
enum ConnectedDriver {
    WebDav(Arc<WebDavDriver>),
    Https(Arc<HttpsDriver>),
    // T-B6-5/6 在此追加各协议臂——分派口唯一，禁第二张连接表
}

impl ConnectedDriver {
    fn info(&self) -> RemoteDriverInfo {
        match self {
            ConnectedDriver::WebDav(d) => {
                let p = d.profile();
                RemoteDriverInfo {
                    driver_id: p.id.clone(),
                    label: d.driver_label(),
                    protocol: p.protocol.as_str().to_owned(),
                    host: p.host.clone(),
                    port: p.port,
                    base_path: p.base_path.clone(),
                    roots: vec![d.base_path()],
                }
            }
            ConnectedDriver::Https(d) => {
                let p = d.profile();
                RemoteDriverInfo {
                    driver_id: p.id.clone(),
                    label: d.driver_label(),
                    protocol: p.protocol.as_str().to_owned(),
                    host: p.host.clone(),
                    port: p.port,
                    base_path: p.base_path.clone(),
                    roots: vec![d.base_path()],
                }
            }
        }
    }

    fn list_entries(&self, path: &str) -> Result<Vec<RemoteEntry>, FileError> {
        match self {
            ConnectedDriver::WebDav(d) => d.list_entries(path),
            // 承重⑨：HTTPS 只有下载腿，浏览明确拒绝而非空表
            ConnectedDriver::Https(_) => Err(FileError::Remote {
                code: FILE_REMOTE_FIELD,
                msg: "HTTP 下载源不支持浏览：该档案只有下载腿（GET/Range），无列目录与写面".into(),
            }),
        }
    }

    fn as_dyn(&self) -> Arc<dyn host_core::storage::StorageDriver> {
        match self {
            ConnectedDriver::WebDav(d) => d.clone(),
            ConnectedDriver::Https(d) => d.clone(),
        }
    }
}

pub struct FileService {
    queue: OpQueue,
    drivers: Arc<DriverRegistry>,
    ports: Arc<Ports>,
    profiles: ProfileStore,
    /// 连接表：键 = `remote:{profile_id}`（承重① 前缀使 `"local"` 顶替结构性
    /// 不可达）；值同时注册进 [`DriverRegistry`]（`register_as` 动态键），两路
    /// 同一 Arc。断线/重启即消失——连接态永不落盘。
    connections: parking_lot::RwLock<HashMap<String, ConnectedDriver>>,
}

impl FileService {
    /// 打开服务：建 pending_ops 目录 + 启动 2 worker（docs/impl/05 F2）。
    /// 进度回调 → EventBus operation.* 主题。
    pub fn open(
        app_data_dir: &Path,
        bus: Arc<EventBus>,
        ports: Arc<Ports>,
    ) -> Result<Self, FileError> {
        let store = app_data_dir.join("pending_ops");
        let bus_cb = bus.clone();
        let cb: crate::ops::ProgressFn = Arc::new(move |p: OpProgress| {
            // D-03 统一背压：operation.progress 走总线合并发布（200ms 窗口，
            // 阈值登记于 TOPIC_REGISTRY），key=op_id 使各操作独立合并
            let mut payload = serde_json::to_value(&p).unwrap_or_default();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("key".into(), serde_json::Value::String(p.op_id.clone()));
            }
            let _ = bus_cb.publish_merged(
                Event::new("operation.progress", "file", payload),
                &p.op_id,
                merged_window("operation.progress"),
            );
            match p.state {
                crate::ops::OpState::Done => {
                    // 终态不等窗口：先冲刷最新进度，再发完成事件
                    bus_cb.flush_merged("operation.progress", &p.op_id);
                    let _ = bus_cb.publish(Event::new(
                        "operation.done",
                        "file",
                        serde_json::json!({ "op_id": p.op_id, "kind": p.kind }),
                    ));
                }
                crate::ops::OpState::Failed | crate::ops::OpState::Canceled => {
                    bus_cb.flush_merged("operation.progress", &p.op_id);
                    let _ = bus_cb.publish(Event::new(
                        "operation.failed",
                        "file",
                        serde_json::json!({
                            "op_id": p.op_id, "kind": p.kind, "state": p.state, "error": p.error
                        }),
                    ));
                }
                _ => {}
            }
        });
        let mut queue = OpQueue::new(store, 2, cb)?;
        if let Some(port) = ports.get::<dyn RecycleBinPort>() {
            queue.set_recycle_port(port);
        }
        Ok(Self {
            queue,
            drivers: Arc::new(DriverRegistry::new()),
            ports,
            profiles: ProfileStore::open(&app_data_dir.join("profiles"))?,
            connections: parking_lot::RwLock::new(HashMap::new()),
        })
    }

    // ---- F1 浏览 ----

    pub fn list_dir(
        &self,
        path: &Path,
        sort: SortKey,
        asc: bool,
    ) -> Result<Vec<FileEntry>, FileError> {
        browse::list_dir(path, sort, asc)
    }

    pub fn breadcrumbs(&self, path: &Path) -> Vec<(String, PathBuf)> {
        browse::breadcrumbs(path)
    }

    pub fn drives(&self) -> Vec<DriveInfo> {
        browse::drives()
    }

    pub fn mkdir(&self, path: &Path) -> Result<(), host_core::error::AppError> {
        self.drivers
            .get("local")
            .expect("LocalDriver 内置")
            .mkdir(path)
    }

    pub fn rename_entry(&self, from: &Path, to: &Path) -> Result<(), host_core::error::AppError> {
        self.drivers
            .get("local")
            .expect("LocalDriver 内置")
            .rename(from, to)
    }

    // ---- F2/F3 操作队列 ----

    /// 入队（Ask 策略先预扫描：有冲突则不入队，返回冲突清单给 UI 决议）
    pub fn enqueue(&self, spec: OpSpec) -> Result<(Option<String>, Vec<ConflictItem>), FileError> {
        if spec.kind == crate::ops::OpKind::Copy || spec.kind == crate::ops::OpKind::Move {
            // T-B6-2：Ask 预扫描只覆盖本地端点（fs 事实源）；远端端点跳过预扫描，
            // 冲突引擎本身已在 conflict_pairs 收口为唯一一份，远端事实源接线随协议行落地
            let locals: Option<(Vec<PathBuf>, PathBuf)> = spec
                .srcs
                .iter()
                .map(|e| e.as_local().cloned())
                .collect::<Option<Vec<_>>>()
                .and_then(|srcs| spec.dst.as_local().cloned().map(|dst| (srcs, dst)));
            if let Some((srcs, dst)) = locals {
                let dst_dir = if dst.is_dir() {
                    dst
                } else {
                    dst.parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from("."))
                };
                let conflicts = scan_conflicts(&srcs, &dst_dir);
                if !conflicts.is_empty() && spec.policy == ConflictPolicy::Ask {
                    return Ok((None, conflicts));
                }
            }
        }
        let op_id = self.queue.enqueue(spec)?;
        Ok((Some(op_id), vec![]))
    }

    pub fn ops_active(&self) -> Vec<OpProgress> {
        self.queue.active()
    }

    pub fn ops_pending(&self) -> Vec<PendingOp> {
        self.queue.pending()
    }

    pub fn op_pause(&self, op_id: &str) -> Result<(), FileError> {
        self.queue.pause(op_id)
    }

    /// 恢复（返回新 op_id，前端据此刷新追踪对象）
    pub fn op_resume(&self, op_id: &str) -> Result<String, FileError> {
        self.queue.resume(op_id)
    }

    pub fn op_cancel(&self, op_id: &str) -> Result<(), FileError> {
        self.queue.cancel(op_id)
    }

    pub fn op_drop_pending(&self, op_id: &str) -> Result<bool, FileError> {
        self.queue.drop_pending(op_id)
    }

    /// 崩溃恢复：启动时扫描 pending_ops 重新入队（Copy/Move 走断点续传）
    pub fn resume_pending(&self) -> usize {
        let mut resumed = 0;
        for p in self.queue.pending() {
            match self.queue.resume(&p.op_id) {
                Ok(_) => resumed += 1,
                Err(e) => tracing::warn!(op_id = %p.op_id, error = %e, "崩溃恢复重入队失败"),
            }
        }
        resumed
    }

    // ---- F4 预览 ----

    pub fn preview(&self, path: &Path) -> Result<Preview, FileError> {
        let thumb = self.ports.get::<dyn ThumbPort>();
        preview_file(path, thumb.as_deref(), 256, 64 * 1024)
    }

    // ---- F5 搜索 ----

    pub fn search(&self, opts: &SearchOpts) -> Result<SearchResult, FileError> {
        let usn = self.ports.get::<dyn UsnIndexPort>();
        search::search(usn.as_deref(), opts)
    }

    // ---- F6 驱动 ----

    pub fn drivers(&self) -> Vec<DriverInfo> {
        self.drivers.list()
    }

    /// 按寻址键取驱动（本地 = "local"；远端 = `remote:{profile_id}`，
    /// 未连接返回 None——不假造驱动）
    pub fn driver(&self, id: &str) -> Option<Arc<dyn host_core::storage::StorageDriver>> {
        self.drivers.get(id)
    }

    // ---- B6 远端连接（T-B6-3）----

    /// 连接：档案必须已建（凭据不喂给野地址），协议分派口唯一。
    /// 同档案重连 = 整体替换旧连接（新口令生效，旧驱动随表项一同退役）。
    /// WebDAV（T-B6-3）与 HTTPS 下载腿（T-B6-4）在册；其余协议明确拒绝
    /// 而非静默空驱动（禁假就绪）。
    pub fn connect(
        &self,
        profile_id: &str,
        secret: Option<AuthSecret>,
    ) -> Result<RemoteDriverInfo, FileError> {
        let profile = self
            .profiles
            .get(profile_id)
            .ok_or_else(|| FileError::Remote {
                code: FILE_REMOTE_MISSING,
                msg: format!("档案不存在: {profile_id}（未建档的站点不建连接）"),
            })?;
        let connected = match profile.protocol {
            RemoteProtocol::WebDav => {
                ConnectedDriver::WebDav(Arc::new(WebDavDriver::new(profile.clone(), secret)))
            }
            RemoteProtocol::Https => {
                // HTTPS 下载腿无凭据面（匿名 GET）；喂进来的 secret 就地退役，
                // 不落到任何驱动字段（口令只进有认证协议的腿）
                drop(secret);
                ConnectedDriver::Https(Arc::new(HttpsDriver::new(profile.clone())))
            }
            other => {
                let row = match other {
                    RemoteProtocol::Sftp => "T-B6-5（SFTP 驱动）",
                    RemoteProtocol::Ftp => "T-B6-6（FTP 明文驱动）",
                    RemoteProtocol::WebDav | RemoteProtocol::Https => {
                        unreachable!("已在上面分派")
                    }
                };
                return Err(FileError::Remote {
                    code: FILE_REMOTE_NOTIMPL,
                    msg: format!(
                        "协议 {} 的驱动由 09 §6.2 {row} 交付，本行不假就绪（档案 {profile_id}）",
                        other.as_str()
                    ),
                });
            }
        };
        let info = connected.info();
        self.connections
            .write()
            .insert(profile_id.to_owned(), connected.clone());
        self.drivers.register_as(profile_id, connected.as_dyn());
        // 连接的写侧副作用只有 last_used 时刻（档案盘上永不记凭据与连接态）
        let mut touched = profile;
        touched.last_used_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        self.profiles.save(touched)?;
        Ok(info)
    }

    /// 断线（幂等）：连接表与注册表两侧同步退役，回传是否真实移除过
    pub fn detach(&self, driver_id: &str) -> bool {
        let removed = self.connections.write().remove(driver_id).is_some();
        self.drivers.unregister(driver_id);
        removed
    }

    /// 远端列目录：未连接的 id 诚实报错，**不回落空表**（假就绪红线）。
    /// 浏览链刻意不走 `browse::list_dir`/`file_list`（那要动 FileEntry.path
    /// 语义，属 §6.3 档 (a)）——远端浏览是独立子视图，见 09 §6.2 T-B6-3 落地补记。
    pub fn remote_list(&self, driver_id: &str, path: &str) -> Result<Vec<RemoteEntry>, FileError> {
        let connected = self
            .connections
            .read()
            .get(driver_id)
            .cloned()
            .ok_or_else(|| FileError::Remote {
                code: FILE_REMOTE_MISSING,
                msg: format!("远端驱动未连接: {driver_id}（诚实报未连接，不回落空表）"),
            })?;
        connected.list_entries(path)
    }

    pub fn remote_drivers(&self) -> Vec<RemoteDriverInfo> {
        let mut v: Vec<RemoteDriverInfo> =
            self.connections.read().values().map(|c| c.info()).collect();
        v.sort_by(|a, b| a.driver_id.cmp(&b.driver_id));
        v
    }

    // ---- B6 远程档案（T-B6-1）----

    pub fn profiles(&self) -> &ProfileStore {
        &self.profiles
    }

    // ---- F7 批量重命名 ----

    pub fn rename_plan(
        &self,
        dir: &Path,
        names: &[String],
        rule: &RenameRule,
    ) -> Result<Vec<RenamePlan>, FileError> {
        build_plan(dir, names, rule)
    }

    pub fn rename_apply(&self, plans: &[RenamePlan]) -> Result<usize, FileError> {
        apply_plan(plans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conflict::ConflictPolicy;
    use crate::ops::{OpEndpoint, OpKind};
    use crate::profile::RemoteProfile;
    use tokio::sync::broadcast::error::TryRecvError;

    /// D-03 回归：operation.progress 经 EventBus::publish_merged 合并发布
    /// （200ms 窗口），终态经 flush_merged 立即冲刷——多分块复制的进度快照
    /// 数远大于总线事件数，且 state=done 快照必达、不滞后于 operation.done。
    #[test]
    fn progress_merges_on_bus_and_done_flushes_immediately() {
        let root = std::env::temp_dir().join(format!("nf_file_svc_{}", uuid::Uuid::now_v7()));
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        // 10MB → 3 个 4MB 分块 + Running/Done 态迁移，Reporter 快照上报 ≥5 次
        std::fs::write(src.join("big.bin"), vec![7u8; 10 * 1024 * 1024]).unwrap();

        let bus = Arc::new(EventBus::new());
        let svc =
            FileService::open(&root.join("store"), bus.clone(), Arc::new(Ports::new())).unwrap();
        let mut prog = bus.subscribe("operation.progress").unwrap();
        let mut done = bus.subscribe("operation.done").unwrap();
        let (op_id, conflicts) = svc
            .enqueue(OpSpec {
                kind: OpKind::Copy,
                srcs: vec![OpEndpoint::local(src.join("big.bin"))],
                dst: OpEndpoint::local(dst.clone()),
                policy: ConflictPolicy::Overwrite,
                recycle: false,
            })
            .unwrap();
        assert!(conflicts.is_empty());
        let op_id = op_id.unwrap();

        let start = std::time::Instant::now();
        let mut progress = Vec::new();
        loop {
            while let Ok(ev) = prog.try_recv() {
                progress.push(ev);
            }
            match done.try_recv() {
                Ok(_) => break,
                Err(TryRecvError::Empty) => {}
                Err(other) => panic!("operation.done 通道异常: {other:?}"),
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(30),
                "复制未完成"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // 冲刷语义：done 到达后，done 快照已在（或即刻在）progress 通道中
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while std::time::Instant::now() < deadline {
            while let Ok(ev) = prog.try_recv() {
                progress.push(ev);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::metadata(dst.join("big.bin")).unwrap().len(),
            10 * 1024 * 1024,
            "文件应完整复制"
        );
        assert!(!progress.is_empty(), "至少应收到合并后的进度事件");
        assert!(
            progress
                .iter()
                .any(|e| e.payload["state"] == "done" && e.payload["op_id"] == op_id.as_str()),
            "flush_merged 应保证 state=done 的终态进度快照立即送达"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- T-B6-3 连接面（09 §6.2 字面测名）----

    fn remote_sample(id: &str, protocol: crate::profile::RemoteProtocol) -> RemoteProfile {
        RemoteProfile {
            id: id.into(),
            label: "联调站".into(),
            protocol,
            host: "127.0.0.1".into(),
            port: 8080,
            user: "me".into(),
            base_path: "/dav".into(),
            auth: crate::profile::AuthKind::PromptEachTime,
            preset_id: None,
            last_used_ms: 0,
        }
    }

    fn svc_fixture(tag: &str) -> (FileService, PathBuf) {
        let root = std::env::temp_dir().join(format!("nf_file_svc_{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let svc = FileService::open(
            &root.join("store"),
            Arc::new(EventBus::new()),
            Arc::new(Ports::new()),
        )
        .unwrap();
        (svc, root)
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-3）字面测试名优先于 rustc 命名惯例
    fn connect_unsupportedProtocol_errs004NamingNextRow() {
        let (svc, root) = svc_fixture("connect004");
        for (proto, row) in [
            (crate::profile::RemoteProtocol::Sftp, "T-B6-5"),
            (crate::profile::RemoteProtocol::Ftp, "T-B6-6"),
        ] {
            let p = remote_sample("remote:next", proto);
            svc.profiles().save(p).unwrap();
            let e = svc.connect("remote:next", None).unwrap_err();
            assert!(
                matches!(&e, FileError::Remote { code, msg } if *code == crate::error::FILE_REMOTE_NOTIMPL && msg.contains(row)),
                "{proto:?} 臂须以 FILE_REMOTE_004 点名后续行 {row}，实得 {e}"
            );
        }
        // 档案不存在同样拒（凭据不喂给野地址）
        let e = svc.connect("remote:ghost", None).unwrap_err();
        assert!(
            matches!(&e, FileError::Remote { code, .. } if *code == FILE_REMOTE_MISSING),
            "未建档站点须报 001，实得 {e}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-3）字面测试名优先于 rustc 命名惯例
    fn connect_hijacksNoLocalDriver() {
        // 端到端正对照（承重① 顶替红线）：连上远端后 driver("local") 仍是
        // LocalDriver——notes-core 经 StoragePort 取的那把驱动永不被换
        let (svc, root) = svc_fixture("hijack");
        svc.profiles()
            .save(remote_sample(
                "remote:dav1",
                crate::profile::RemoteProtocol::WebDav,
            ))
            .unwrap();
        let info = svc.connect("remote:dav1", None).unwrap();
        assert_eq!(info.driver_id, "remote:dav1");
        assert_eq!(info.protocol, "webdav");
        assert_eq!(info.roots, vec!["/dav".to_owned()]);
        let local = svc.driver("local").expect("local 驱动应原封不动");
        assert_eq!(local.id(), "local");
        assert_eq!(local.label(), "本地磁盘");
        assert!(svc.driver("remote:dav1").is_some(), "远端按动态键可寻址");
        assert_eq!(svc.drivers().len(), 2);
        // 重连替换：同档案两连不产生第二条注册项
        svc.connect("remote:dav1", None).unwrap();
        assert_eq!(svc.remote_drivers().len(), 1);
        assert_eq!(svc.drivers().len(), 2);
        // 断线两侧同步退役，local 依然屹立
        assert!(svc.detach("remote:dav1"));
        assert!(!svc.detach("remote:dav1"), "幂等断线回 false");
        assert!(svc.driver("remote:dav1").is_none());
        assert!(svc.driver("local").is_some());
        // last_used 已 touch（连接只留时刻，不留凭据）
        assert!(svc.profiles().get("remote:dav1").unwrap().last_used_ms > 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-3）字面测试名优先于 rustc 命名惯例
    fn remoteBrowse_unknownDriverId_errsNotEmpty() {
        // 不假绿：未连接 id → Err(FILE_REMOTE_001) 而非空表
        let (svc, root) = svc_fixture("browse001");
        let e = svc.remote_list("remote:nope", "/").unwrap_err();
        assert!(
            matches!(&e, FileError::Remote { code, msg } if *code == FILE_REMOTE_MISSING && msg.contains("remote:nope")),
            "须点名未连接的 id，实得 {e}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-4）字面测试名优先于 rustc 命名惯例
    fn httpsDriver_listRefusesNotEmptyList_serviceMouth() {
        // T-B6-4 换臂的端到端证据：Https 从 004"后续行交付"变成真连接，
        // 而它的浏览面经 service 口仍必须 Err(005)——登记为在册 ≠ 假就绪。
        let (svc, root) = svc_fixture("https005");
        svc.profiles()
            .save(remote_sample(
                "remote:hs1",
                crate::profile::RemoteProtocol::Https,
            ))
            .unwrap();
        let info = svc.connect("remote:hs1", None).unwrap();
        assert_eq!(info.protocol, "https");
        assert_eq!(svc.remote_drivers().len(), 1);
        let e = svc.remote_list("remote:hs1", "/").unwrap_err();
        assert!(
            matches!(&e, FileError::Remote { code, msg } if *code == FILE_REMOTE_FIELD && msg.contains("HTTP 下载源不支持浏览")),
            "HTTPS 浏览须报 005 点名下载腿，实得 {e}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
