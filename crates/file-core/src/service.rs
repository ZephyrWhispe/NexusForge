//! FileService（F1–F7 门面）：IPC 层唯一入口；阻塞 IO 由调用方 spawn_blocking。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use host_core::events::{merged_window, Event, EventBus};
use host_core::ports::{Ports, RecycleBinPort, ThumbPort, UsnIndexPort};

use crate::browse::{self, DriveInfo, FileEntry, SortKey};
use crate::conflict::{scan_conflicts, ConflictItem, ConflictPolicy};
use crate::driver::{DriverInfo, DriverRegistry};
use crate::error::{FileError, FILE_REMOTE_FIELD, FILE_REMOTE_MISSING};
use crate::module::FileConfig;
use crate::ops::{OpProgress, OpQueue, OpSpec, PendingOp, ResumeDto, XferStatusDto};
use crate::preset::PresetStore;
use crate::preview::{preview_file, Preview};
use crate::profile::{ProfileStore, RemoteProtocol};
use crate::remote::ftp::{ftp_confirm_gate, ftp_plaintext_guard, FtpDriver};
use crate::remote::ssh::{
    load_known_hosts, sftp_auth_for, tofu_guard, KnownHostsFile, RusshBackend, SftpDriver,
    SshBackend,
};
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
    Sftp(Arc<SftpDriver>),
    Ftp(Arc<FtpDriver>),
    // 分派口唯一，禁第二张连接表（四协议在 T-B6-6 集齐，004 分派臂退役）
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
                    auth_source: crate::profile::auth_source_of(p),
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
                    auth_source: crate::profile::auth_source_of(p),
                }
            }
            ConnectedDriver::Sftp(d) => {
                let p = d.profile();
                RemoteDriverInfo {
                    driver_id: p.id.clone(),
                    label: d.driver_label(),
                    protocol: p.protocol.as_str().to_owned(),
                    host: p.host.clone(),
                    port: p.port,
                    base_path: p.base_path.clone(),
                    roots: vec![d.base_path()],
                    auth_source: crate::profile::auth_source_of(p),
                }
            }
            ConnectedDriver::Ftp(d) => {
                let p = d.profile();
                RemoteDriverInfo {
                    driver_id: p.id.clone(),
                    label: d.driver_label(),
                    protocol: p.protocol.as_str().to_owned(),
                    host: p.host.clone(),
                    port: p.port,
                    base_path: p.base_path.clone(),
                    roots: vec![d.base_path()],
                    auth_source: crate::profile::auth_source_of(p),
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
            ConnectedDriver::Sftp(d) => d.list_entries(path),
            ConnectedDriver::Ftp(d) => d.list_entries(path),
        }
    }

    fn as_dyn(&self) -> Arc<dyn host_core::storage::StorageDriver> {
        match self {
            ConnectedDriver::WebDav(d) => d.clone(),
            ConnectedDriver::Https(d) => d.clone(),
            ConnectedDriver::Sftp(d) => d.clone(),
            ConnectedDriver::Ftp(d) => d.clone(),
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
    /// file 域自己的 known_hosts（T-B6-5）：与 term 的表**互不共享**（缺口登记
    /// 09 §6.3）；open 时 fail-closed 加载——坏文件 ⇒ 服务根本开不起来。
    known_hosts: Arc<KnownHostsFile>,
    /// 测试注入位：SSH 协议腿替身（FakeSsh）。生产路径恒 None ⇒ 每档案
    /// 现场 `RusshBackend::bound`；这不是回退兜底，是分派口的依赖注入槽。
    ssh_backend_override: parking_lot::RwLock<Option<Arc<dyn SshBackend>>>,
    /// 配置真源（09 §6.2 T-B6-6）：Arc 让各消费点（入队闸/连接闸/限速腿）
    /// 读到同一份现场值——apply_config 即写即生效，无重启窗口
    config: Arc<parking_lot::RwLock<FileConfig>>,
    /// 连接预设（内置 + 用户目录，open 时 fail-closed 全量校验；
    /// 预设不是档案，零注册零连接，命令面只读列表）
    presets: PresetStore,
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
        let config = Arc::new(parking_lot::RwLock::new(FileConfig::default()));
        queue.set_max_concurrent(config.read().max_concurrent);
        Ok(Self {
            queue,
            drivers: Arc::new(DriverRegistry::new()),
            ports,
            profiles: ProfileStore::open(&app_data_dir.join("profiles"))?,
            connections: parking_lot::RwLock::new(HashMap::new()),
            known_hosts: load_known_hosts(&app_data_dir.join("known_hosts.json"))?,
            ssh_backend_override: parking_lot::RwLock::new(None),
            config,
            presets: PresetStore::load(&app_data_dir.join("presets"))?,
        })
    }

    // ---- 配置真源（T-B6-6）----

    pub fn config(&self) -> FileConfig {
        self.config.read().clone()
    }

    pub(crate) fn config_arc(&self) -> Arc<parking_lot::RwLock<FileConfig>> {
        self.config.clone()
    }

    /// 写侧唯一口（[`crate::module::FileModule::apply_config`] 校验后经此）：
    /// 现场值 + 队列准入门一次更新，各读侧下一动作即见新值（不重启即生效）
    pub fn set_config(&self, next: FileConfig) {
        self.queue.set_max_concurrent(next.max_concurrent);
        *self.config.write() = next;
    }

    /// 预设列表（只读；坏预设文件在 open 已 fail-closed，这里恒为全好快照）
    pub fn presets(&self) -> Vec<crate::preset::RemotePreset> {
        self.presets.list()
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

    /// 配置真源消费点（纯函数，派发测与入队面共读同一实现）：把运行态配置
    /// 施加到调用方 spec 上。`default_conflict_policy` 非 Ask 时接管调用方
    /// 留下的 Ask（用户说了"别逐单问"，队列不再挂预扫描）；
    /// `delete_to_recycle` 只收紧删除臂——放开方向不存在（开=维持请求，关=强制直删）。
    /// 远端源的回收站红线在队列闸先行（ops.rs 入队 Err 与驱动侧第二道），
    /// 配置开闸也到不了远端。
    pub(crate) fn apply_config_to_spec(cfg: &FileConfig, mut spec: OpSpec) -> OpSpec {
        if spec.kind == crate::ops::OpKind::Delete && !cfg.delete_to_recycle {
            spec.recycle = false;
        }
        if spec.policy == ConflictPolicy::Ask && cfg.default_conflict_policy != ConflictPolicy::Ask
        {
            spec.policy = cfg.default_conflict_policy;
        }
        spec
    }

    /// 入队（Ask 策略先预扫描：有冲突则不入队，返回冲突清单给 UI 决议）。
    /// 配置两消费点经 [`Self::apply_config_to_spec`]（T-B6-6）。
    pub fn enqueue(&self, spec: OpSpec) -> Result<(Option<String>, Vec<ConflictItem>), FileError> {
        let spec = Self::apply_config_to_spec(&self.config(), spec);
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

    /// 恢复（T-B6-7 破坏性 IPC 变更：`String → ResumeDto`）。断点链在返回体
    /// 里明写（`previous_op_id`），前端必须消费新行身份——丢弃返回值续不上链。
    pub fn op_resume(&self, op_id: &str) -> Result<ResumeDto, FileError> {
        let new_op_id = self.queue.resume(op_id)?;
        Ok(ResumeDto {
            op_id: new_op_id,
            previous_op_id: Some(op_id.to_owned()),
        })
    }

    /// 单条传输的 typed 状态（09 §6.2 T-B6-7 `xfer_status`）：查不到 ⇒ Err
    /// 点名，**不回落空壳**（诚实空 vs 错纪律）
    pub fn xfer_status(&self, op_id: &str) -> Result<XferStatusDto, FileError> {
        self.queue
            .status(op_id)
            .map(|p| XferStatusDto::from(&p))
            .ok_or_else(|| FileError::NoSuchOp(op_id.to_owned()))
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
    /// 四协议自 T-B6-6 集齐（004"后续行"分派臂退役——每条腿都在册）：
    /// WebDAV（T-B6-3）、HTTPS 下载腿（T-B6-4）、SFTP（T-B6-5）、FTP 明文腿
    /// （T-B6-6，入表前先过 [`ftp_plaintext_guard`] 三闸）。SFTP 臂的 TOFU
    /// 守卫同样在**入表之前**：未受信主机键连"存在一条连接"这一事实都不该留下。
    /// `allow_plaintext_once`（T-B6-8 第三闸）：非回环明文连接的**逐次**用户明示，
    /// 不带即 `Err(FILE_REMOTE_008)` 拒在建连之前；对本函数无副作用记忆。
    pub fn connect(
        &self,
        profile_id: &str,
        secret: Option<AuthSecret>,
        allow_plaintext_once: bool,
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
                ConnectedDriver::Https(Arc::new(HttpsDriver::new(
                    profile.clone(),
                    self.config_arc(),
                )))
            }
            RemoteProtocol::Sftp => {
                let auth = sftp_auth_for(&profile, secret)?;
                let backend: Arc<dyn SshBackend> = match self.ssh_backend_override.read().clone() {
                    Some(injected) => injected,
                    None => Arc::new(RusshBackend::bound(
                        &profile,
                        auth.clone(),
                        self.known_hosts.clone(),
                    )),
                };
                // 探测会话只走 KEX（凭据不出网）；Unknown/Changed 在此 Err，
                // 表与注册器都还没动——"拒"是真的拒
                tofu_guard(
                    &*backend,
                    &self.known_hosts,
                    &profile.host,
                    profile.port,
                    &profile.user,
                    &auth,
                )?;
                ConnectedDriver::Sftp(Arc::new(SftpDriver::new(
                    profile.clone(),
                    auth,
                    backend,
                    self.known_hosts.clone(),
                )))
            }
            RemoteProtocol::Ftp => {
                // 明文总闸在建驱动之前裁决（惰建连：驱动 new 零网络，
                // 被拒的明文档案不可能留下半条连接的事实源）；第三闸（逐次
                // 确认，T-B6-8）同点位——两闸都在入表前，拒了就是没连
                ftp_plaintext_guard(&profile, self.config().insecure_plaintext)?;
                ftp_confirm_gate(&profile, allow_plaintext_once)?;
                ConnectedDriver::Ftp(Arc::new(FtpDriver::new(profile.clone(), secret)))
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

    /// TOFU 首见的唯一出路：用户在对话框里逐字核对后显式确认。
    /// `fingerprint` 是**完整描述符**（"算法名 SHA256:base64"整串，来自
    /// 001 错误消息点名）——接受的是这一枚键，不是"这台主机"；非 SFTP
    /// 档案没有主机键概念，拒绝而非空操作。
    pub fn fingerprint_ack(&self, profile_id: &str, fingerprint: &str) -> Result<(), FileError> {
        let profile = self
            .profiles
            .get(profile_id)
            .ok_or_else(|| FileError::Remote {
                code: FILE_REMOTE_MISSING,
                msg: format!("档案不存在: {profile_id}（不为野地址记主机键）"),
            })?;
        if profile.protocol != RemoteProtocol::Sftp {
            return Err(FileError::Remote {
                code: FILE_REMOTE_FIELD,
                msg: format!(
                    "指纹确认仅适用 SFTP 档案：{} 是 {} 腿",
                    profile_id,
                    profile.protocol.as_str()
                ),
            });
        }
        self.known_hosts
            .accept(&profile.host, profile.port, fingerprint)
    }

    /// 测试位：注入 SSH 协议腿替身（FakeSsh），让 TOFU/驱动臂在无网可达的
    /// CI 里走真分派口。生产命令面不暴露任何等价入口。
    #[cfg(test)]
    pub(crate) fn set_ssh_backend_for_test(&self, backend: Arc<dyn SshBackend>) {
        *self.ssh_backend_override.write() = Some(backend);
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
        // T-B6-6 取代（落地补记④）：四协议分派臂集齐，004"后续行"臂退役
        // （FILE_REMOTE_NOTIMPL 常量仍留在码表，但已无任何档案走得进去）。
        // 名字保留原语义——"拒而非静默空"：FTP 臂的拒现在是明文三闸（006），
        // 总闸开了才放行（惰性驱动：connect 握手都不发生，见 ftp.rs 字面测名）。
        let (svc, root) = svc_fixture("connect004");
        let mut p = remote_sample("remote:ftp-pub", crate::profile::RemoteProtocol::Ftp);
        p.host = "ftp.example.org".into();
        svc.profiles().save(p).unwrap();
        let e = svc.connect("remote:ftp-pub", None, false).unwrap_err();
        assert!(
            matches!(&e, FileError::Remote { code, msg } if *code == crate::error::FILE_REMOTE_PLAINTEXT && msg.contains("insecure_plaintext")),
            "非回环 FTP 未开闸须报 006 点名总闸，实得 {e}"
        );
        // 正对照：用户显式开闸后同站点放行——明文是用户的决定，不是假就绪
        // （第三闸随 T-B6-8：逐次确认参数一并给出 true；其单独判据见
        // `plaintextNonLoopback_requiresExplicitConfirm_eachConnection`）
        svc.set_config(crate::module::FileConfig {
            insecure_plaintext: true,
            ..Default::default()
        });
        let info = svc.connect("remote:ftp-pub", None, true).unwrap();
        assert_eq!(info.protocol, "ftp");
        assert!(svc.detach("remote:ftp-pub"), "开闸连接须真实入表");
        // 档案不存在同样拒（凭据不喂给野地址）——004 退役后此臂仍在
        let e = svc.connect("remote:ghost", None, false).unwrap_err();
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
        let info = svc.connect("remote:dav1", None, false).unwrap();
        assert_eq!(info.driver_id, "remote:dav1");
        assert_eq!(info.protocol, "webdav");
        assert_eq!(info.roots, vec!["/dav".to_owned()]);
        let local = svc.driver("local").expect("local 驱动应原封不动");
        assert_eq!(local.id(), "local");
        assert_eq!(local.label(), "本地磁盘");
        assert!(svc.driver("remote:dav1").is_some(), "远端按动态键可寻址");
        assert_eq!(svc.drivers().len(), 2);
        // 重连替换：同档案两连不产生第二条注册项
        svc.connect("remote:dav1", None, false).unwrap();
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
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-8）字面测试名优先于 rustc 命名惯例
    fn plaintextNonLoopback_requiresExplicitConfirm_eachConnection() {
        // 第三闸两臂：带确认参 → 放行；不带 → Err(FILE_REMOTE_008) 且消息含
        // "需用户明示"。"FakeBackend 零出站包"的可观测形态：假 FTP 站（绑
        // 0.0.0.0 的监听器，档案 host 用 127.0.0.2 —— looks_like_loopback 按
        // 精确名单判，非名单内即走逐次确认，fail-closed 方向）在整场测试里
        // accept 计数恒零：拒绝发生在建连之前，连"拨号"这个动作都不曾发生。
        use std::net::TcpListener;
        use std::sync::mpsc;
        let listener = TcpListener::bind("0.0.0.0:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stream.is_ok() {
                    let _ = tx.send(());
                }
            }
        });
        let dial_probe = |label: &str| {
            assert!(
                rx.try_recv().is_err(),
                "{label}：假 FTP 站收到了入站连接——拒绝必须发生在出网之前"
            );
        };
        let (svc, root) = svc_fixture("plainconfirm");
        let mut p = remote_sample("remote:ftp-conf", crate::profile::RemoteProtocol::Ftp);
        p.host = "127.0.0.2".into();
        p.port = port;
        svc.profiles().save(p).unwrap();
        svc.set_config(crate::module::FileConfig {
            insecure_plaintext: true, // 总闸开——本闸测的是逐次确认，不测 006
            ..Default::default()
        });
        // 臂一：不带确认参 → 008 且消息含"需用户明示"；表里不留半条连接
        let e = svc.connect("remote:ftp-conf", None, false).unwrap_err();
        assert!(
            matches!(&e, FileError::Remote { code, msg }
                if *code == crate::error::FILE_REMOTE_PLAIN_CONFIRM && msg.contains("需用户明示")),
            "非回环明文未确认须报 008 含\"需用户明示\"，实得 {e}"
        );
        assert!(svc.remote_drivers().is_empty(), "被拒连接不得入表");
        assert!(
            svc.remote_list("remote:ftp-conf", "/").is_err(),
            "拒后浏览须诚实报未连接"
        );
        dial_probe("拒绝臂");
        // 臂二：带确认参 → 放行（惰性驱动，仍零出站）
        let info = svc.connect("remote:ftp-conf", None, true).unwrap();
        assert_eq!(info.protocol, "ftp");
        dial_probe("放行臂（connect 本身不出网）");
        // "每次连接"：确认不被记忆——同档案再连不带参仍是 008（无记住这档的出路）
        let e = svc.connect("remote:ftp-conf", None, false).unwrap_err();
        assert!(
            matches!(&e, FileError::Remote { code, .. } if *code == crate::error::FILE_REMOTE_PLAIN_CONFIRM),
            "第三闸逐次生效，第二次不带参必须仍拒，实得 {e}"
        );
        dial_probe("逐次臂");
        // 正对照：回环豁免本闸（联调形状不带确认参也放行）
        let mut lp = remote_sample("remote:ftp-loop", crate::profile::RemoteProtocol::Ftp);
        lp.host = "127.0.0.1".into();
        lp.port = port;
        svc.profiles().save(lp).unwrap();
        svc.connect("remote:ftp-loop", None, false).unwrap();
        // 正对照二：非明文腿不受本闸约束（webdav 档案 host 非回环，无确认参放行）
        let mut wp = remote_sample("remote:dav-conf", crate::profile::RemoteProtocol::WebDav);
        wp.host = "dav.example.com".into();
        svc.profiles().save(wp).unwrap();
        svc.connect("remote:dav-conf", None, false).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        // 收尾再探一次：全程零出站（含 webdav/回环这些"放行"臂——connect 惰性）
        dial_probe("终局");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-8）字面测试名优先于 rustc 命名惯例
    fn authSource_columnReportsSourceNotValue() {
        // auth_source 是"来源列"：五档各报其位，凭据值绝不出现在列里
        // （夹具口令 SUPER_SECRET_VALUE 若在返回体任何一处，序列化文本比对即红）。
        use crate::profile::AuthSource;
        use crate::remote::ssh::FakeSsh;
        let (svc, root) = svc_fixture("authcol");
        let mut p = remote_sample("remote:sftp-col", crate::profile::RemoteProtocol::Sftp);
        p.host = "sftp.example.invalid".into();
        p.port = 2222;
        p.auth = crate::profile::AuthKind::VaultEntry {
            entry_id: "0198f2c7-3a4e-7a10-9b6a-2f1c8d5e4b3a".into(),
        };
        svc.profiles().save(p).unwrap();
        let fp = "ssh-ed25519 SHA256:COLfpxxxxxxxxxxxxxx";
        let fake = Arc::new(FakeSsh::new(fp));
        svc.set_ssh_backend_for_test(fake.clone());
        svc.fingerprint_ack("remote:sftp-col", fp).unwrap();
        let secret = Some(crate::remote::AuthSecret {
            header: None,
            password: Some(zeroize::Zeroizing::new("SUPER_SECRET_VALUE".into())),
        });
        let info = svc.connect("remote:sftp-col", secret, false).unwrap();
        assert_eq!(info.auth_source, AuthSource::VaultEntry);
        let text = serde_json::to_string(&info).unwrap();
        assert!(
            text.contains("\"auth_source\":\"vault_entry\""),
            "列须序列化，实得 {text}"
        );
        assert!(
            !text.contains("SUPER_SECRET"),
            "凭据值绝不出现在列里: {text}"
        );
        assert!(
            !text.contains("entry_id"),
            "指针值也不入返回体——来源列只到档位: {text}"
        );
        // 五档全序：逐个档案投影 auth_source_of（键名逐字钉住 wire 形状）
        let arms: Vec<(crate::profile::AuthKind, &str)> = vec![
            (crate::profile::AuthKind::Anonymous, "\"anonymous\""),
            (
                crate::profile::AuthKind::SshKey {
                    key_path: "C:/keys/id".into(),
                },
                "\"key_file\"",
            ),
            (
                crate::profile::AuthKind::VaultEntry {
                    entry_id: "e1".into(),
                },
                "\"vault_entry\"",
            ),
            (crate::profile::AuthKind::SessionPassword, "\"session\""),
            (crate::profile::AuthKind::PromptEachTime, "\"typed\""),
        ];
        for (kind, want) in arms {
            let mut q = remote_sample("remote:col-x", crate::profile::RemoteProtocol::WebDav);
            q.auth = kind;
            let got = serde_json::to_value(crate::profile::auth_source_of(&q)).unwrap();
            assert_eq!(got.as_str().unwrap(), want.trim_matches('"'));
        }
        // 默认档案（PromptEachTime）投影 typed 且 remote_drivers 列表同样带列
        let listed = svc.remote_drivers();
        assert_eq!(listed[0].auth_source, AuthSource::VaultEntry);
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
        let info = svc.connect("remote:hs1", None, false).unwrap();
        assert_eq!(info.protocol, "https");
        assert_eq!(svc.remote_drivers().len(), 1);
        let e = svc.remote_list("remote:hs1", "/").unwrap_err();
        assert!(
            matches!(&e, FileError::Remote { code, msg } if *code == FILE_REMOTE_FIELD && msg.contains("HTTP 下载源不支持浏览")),
            "HTTPS 浏览须报 005 点名下载腿，实得 {e}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[allow(non_snake_case)]
    fn enqueue_appliesDefaultPolicy_whenCallerLeftAsk() {
        // 配置真源消费点（T-B6-6）：`default_conflict_policy` 非 Ask 时接管
        // 调用方留下的 Ask——用户说了"别逐单问"，队列就不该再挂预扫描返回冲突。
        let (svc, root) = svc_fixture("policy");
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("a.txt"), b"x").unwrap();
        std::fs::write(dst.join("a.txt"), b"y").unwrap();
        let spec = || crate::ops::OpSpec {
            kind: crate::ops::OpKind::Copy,
            srcs: vec![crate::ops::OpEndpoint::local(src.join("a.txt"))],
            dst: crate::ops::OpEndpoint::local(dst.clone()),
            policy: ConflictPolicy::Ask,
            recycle: false,
        };
        // 默认（Ask）：冲突预扫描原样返回
        let (op, conflicts) = svc.enqueue(spec()).unwrap();
        assert!(op.is_none() && conflicts.len() == 1, "Ask 缺省须回冲突清单");
        // 改闸为 Rename：同一次 Ask 调用被配置接管，直接入队
        svc.set_config(crate::module::FileConfig {
            default_conflict_policy: ConflictPolicy::Rename,
            ..Default::default()
        });
        let (op, conflicts) = svc.enqueue(spec()).unwrap();
        assert!(
            op.is_some() && conflicts.is_empty(),
            "非 Ask 缺省必须接管 Ask 调用位"
        );
        let op_id = op.unwrap();
        let _ = svc.op_cancel(&op_id);
        let _ = std::fs::remove_dir_all(&root);
    }
}
