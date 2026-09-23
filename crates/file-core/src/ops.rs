//! F2 异步操作队列（docs/impl/05 F2）：复制/移动/删除/压缩/解压。
//!
//! 模型：全局 mpsc 队列 + N worker（默认 2，std 线程——进度发布走 host-core 同步
//! broadcast，无需 tokio）。每个 Op：
//! - 入队先落 `{store_dir}/{op_id}.json`（崩溃恢复扫描点，docs/impl/01 S6.5）
//! - 大文件 4MB 分块 + 逐块读满（非末块必须 read_exact，短块会造成零洞——M4 K9 教训）
//! - 进度快照逐块上报，限频合并统一由 EventBus::publish_merged 承担（D-03，200ms 窗口）
//! - 断点续传：Copy/Move 记录 (file_index, bytes_done)，resume 时 seek 续传
//! - 删除：recycle=true 走 [`RecycleBinPort`]（SHFileOperationW 回收站），无端口降级直删

use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use host_core::ports::RecycleBinPort;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zip::read::ZipArchive;
use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::browse::to_long_path;
use crate::conflict::{resolve_target, unique_target, ConflictPolicy};
use crate::error::FileError;

/// 分块大小（docs/impl/05 K6/F2 统一 4MB）
pub const CHUNK: usize = 4 * 1024 * 1024;
/// 断点持久化间隔（单文件内复制字节量）
const CHECKPOINT_EVERY: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Copy,
    Move,
    Delete,
    Compress,
    Extract,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpState {
    #[default]
    Queued,
    Running,
    Paused,
    Done,
    Failed,
    Canceled,
}

/// T-B6-2（09 §6.2）操作端点：本地路径或"驱动 + 远端 String 路径"。
/// untagged ⇒ 旧 `pending_ops/*.json` 里的裸字符串（含 Windows 反斜杠与中文）
/// 继续落 [`OpEndpoint::Local`]，零迁移、零版本协商（承重⑤）。
/// 远端臂的路径是 `/` 分隔 String——不进 PathBuf，防 Windows 分隔符污染（承重①( b)）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpEndpoint {
    Local(PathBuf),
    Remote { driver_id: String, path: String },
}

impl OpEndpoint {
    pub fn local(p: impl Into<PathBuf>) -> Self {
        OpEndpoint::Local(p.into())
    }
    /// 唯一本地取形口：非本地给 None（调用方按方向拒绝，禁静默当本地处理）
    pub fn as_local(&self) -> Option<&PathBuf> {
        match self {
            OpEndpoint::Local(p) => Some(p),
            OpEndpoint::Remote { .. } => None,
        }
    }
    pub fn display(&self) -> String {
        match self {
            OpEndpoint::Local(p) => p.display().to_string(),
            OpEndpoint::Remote { driver_id, path } => format!("{driver_id}:{path}"),
        }
    }
}

/// 传输方向（面板文案与断点算法共读的唯一派生值，禁两处各判，承重④）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    #[default]
    Local,
    Upload,
    Download,
}

/// 方向唯一算式（承重④）：全本地→Local；dst 远且 srcs 全本地→Upload；
/// srcs 全远且 dst 本地→Download；混合两端→拒（禁"当本地复制处理"）。
pub fn direction_of(srcs: &[OpEndpoint], dst: &OpEndpoint) -> Result<TransferDirection, FileError> {
    let src_remote = srcs
        .iter()
        .filter(|e| matches!(e, OpEndpoint::Remote { .. }))
        .count();
    let dst_remote = matches!(dst, OpEndpoint::Remote { .. });
    let direction = match (src_remote == 0, src_remote == srcs.len(), dst_remote) {
        (true, _, false) => TransferDirection::Local,
        (true, _, true) => TransferDirection::Upload,
        (false, true, false) => TransferDirection::Download,
        _ => {
            return Err(FileError::Remote {
                code: crate::error::FILE_REMOTE_MIXED,
                msg: format!(
                    "混合端点不可表达（{src_remote}/{n} 源为远端、目标远端={dst_remote}）：一次操作只允许跨一次系统边界",
                    n = srcs.len()
                ),
            })
        }
    };
    Ok(direction)
}

/// 目标桶键（纯函数，零 fs 调用，承重④ 对 target_for 隐式 Path 算术的收口）：
/// 本地取 `Path::parent`，远端取最后一个 `/` 之前的前缀。
pub fn parent_key(e: &OpEndpoint) -> String {
    match e {
        OpEndpoint::Local(p) => p
            .parent()
            .map(|x| x.display().to_string())
            .unwrap_or_default(),
        OpEndpoint::Remote { path, .. } => match path.rfind('/') {
            Some(0) => "/".to_string(),
            Some(i) => path[..i].to_string(),
            None => String::new(),
        },
    }
}

/// 操作规格（IPC 提交）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpSpec {
    pub kind: OpKind,
    pub srcs: Vec<OpEndpoint>,
    /// Copy/Move：目标目录或文件；Compress：zip 输出路径；Extract：解压根目录
    pub dst: OpEndpoint,
    pub policy: ConflictPolicy,
    /// Delete 专用：true 走回收站（需 RecycleBinPort），false 直删；
    /// 远端源 + recycle=true 在入队即拒（远端无回收站，承重⑥）
    #[serde(default)]
    pub recycle: bool,
}

/// 断点：续传从扁平计划的 file_index 个文件开始，该文件前 bytes_done 字节已完成
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub file_index: usize,
    pub bytes_done: u64,
}

/// 崩溃恢复持久化条目（pending_ops/{op_id}.json）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingOp {
    pub op_id: String,
    pub kind: OpKind,
    pub srcs: Vec<OpEndpoint>,
    pub dst: OpEndpoint,
    pub policy: ConflictPolicy,
    pub recycle: bool,
    #[serde(default)]
    pub file_index: usize,
    #[serde(default)]
    pub bytes_done: u64,
    pub created_ms: i64,
    /// T-B6-7 加键（serde default ⇒ 旧 pending 文件零迁移）：本条由哪一枚
    /// 旧 op resume 而来（断点链落盘，重开服务不丢）
    #[serde(default)]
    pub resumed_from: Option<String>,
}

/// 进度快照（事件 payload 与 active() 返回共用）
#[derive(Clone, Debug, Serialize)]
pub struct OpProgress {
    pub op_id: String,
    pub kind: OpKind,
    pub state: OpState,
    /// 当前处理中的文件名
    pub current: String,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub error: Option<String>,
    /// T-B6-2 加键（向后兼容）：入队时由 [`direction_of`] 唯一派生
    pub direction: TransferDirection,
    /// T-B6-7 加键：**对端声明才承诺**——本地/未获事实源恒 None（禁给本地复制
    /// 编一个"可续传"），远端下载腿接线（T-B6-11）后由 classify_resume 供值
    #[serde(default)]
    pub resumable: Option<crate::remote::Resumable>,
    /// T-B6-7 加键：resume 产新 op 时指回旧 op（断点链面板可分辨）；
    /// 随 PendingOp 落盘 ⇒ 重开服务链不丢
    #[serde(default)]
    pub resumed_from: Option<String>,
}

/// 传输状态 DTO（09 §6.2 T-B6-7：新命令 `xfer_status` 而非 `file_op_status`，
/// 与 file_op_pause/resume 命名族同谱）。`XferState = OpState`（复用而非另立
/// 镜像，防两套终态）。
#[derive(Clone, Debug, Serialize)]
pub struct XferStatusDto {
    pub op_id: String,
    pub kind: OpKind,
    pub direction: TransferDirection,
    pub state: OpState,
    pub current: String,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub resumable: Option<crate::remote::Resumable>,
    pub error: Option<String>,
    pub resumed_from: Option<String>,
}

pub type XferState = OpState;

impl From<&OpProgress> for XferStatusDto {
    fn from(p: &OpProgress) -> Self {
        Self {
            op_id: p.op_id.clone(),
            kind: p.kind,
            direction: p.direction,
            state: p.state,
            current: p.current.clone(),
            files_done: p.files_done,
            files_total: p.files_total,
            bytes_done: p.bytes_done,
            bytes_total: p.bytes_total,
            resumable: p.resumable,
            error: p.error.clone(),
            resumed_from: p.resumed_from.clone(),
        }
    }
}

/// resume 的返回形状（T-B6-7 破坏性 IPC 变更：`String → ResumeDto`）：
/// 新行身份 `op_id` + 断点链来源 `previous_op_id`，前端必须消费而非丢弃。
#[derive(Clone, Debug, Serialize)]
pub struct ResumeDto {
    pub op_id: String,
    pub previous_op_id: Option<String>,
}

pub(crate) struct OpControl {
    paused: AtomicBool,
    canceled: AtomicBool,
}

impl OpControl {
    fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            canceled: AtomicBool::new(false),
        }
    }
}

enum Gate {
    Go,
    Pause,
    Cancel,
}

fn gate(ctl: &OpControl) -> Gate {
    if ctl.canceled.load(Ordering::SeqCst) {
        Gate::Cancel
    } else if ctl.paused.load(Ordering::SeqCst) {
        Gate::Pause
    } else {
        Gate::Go
    }
}

/// worker 内部控制流
#[derive(Debug)]
enum Flow {
    Done,
    Paused,
    Canceled,
    Failed(String),
}

impl Flow {
    fn io(e: std::io::Error) -> Self {
        Flow::Failed(e.to_string())
    }
    fn msg(e: impl Into<String>) -> Self {
        Flow::Failed(e.into())
    }
}

impl From<Gate> for Flow {
    fn from(g: Gate) -> Self {
        match g {
            Gate::Go => Flow::Done,
            Gate::Pause => Flow::Paused,
            Gate::Cancel => Flow::Canceled,
        }
    }
}

struct Job {
    op_id: String,
    spec: OpSpec,
    direction: TransferDirection,
    checkpoint: Option<Checkpoint>,
    /// resume 产新 op 时的旧 op_id（进 OpProgress.resumed_from）
    resumed_from: Option<String>,
    ctl: Arc<OpControl>,
    store_dir: PathBuf,
    recycle: Option<Arc<dyn RecycleBinPort>>,
}

/// 本地端点唯一取形口（承重①(b)：非本地绝不静默当 PathBuf 用）。
/// worker 侧方向拒绝在前，这里 Err 属 fail-closed 兜底而非业务分支。
fn expect_local(e: &OpEndpoint) -> Result<PathBuf, Flow> {
    e.as_local().cloned().ok_or_else(|| {
        Flow::msg(format!(
            "远端端点 {} 的执行器自 T-B6-3 起接线，本臂只认本地路径",
            e.display()
        ))
    })
}

/// Flow 直返函数里的本地取形早退（? 只活在 Result<_, Flow> 上下文）
macro_rules! local {
    ($e:expr) => {
        match expect_local($e) {
            Ok(p) => p,
            Err(flow) => return flow,
        }
    };
}

/// 进度回调（模块把它接到 EventBus：payload key=op_id 供 UI 去抖订阅）
pub type ProgressFn = Arc<dyn Fn(OpProgress) + Send + Sync>;

/// 并发准入门（09 §6.2 T-B6-6 `max_concurrent` 的执行体）：worker 线程数在
/// 建队时定死（内置上限 2），配置只收紧不超发——`set_max_concurrent` 夹到
/// `1..=workers`，取到 job 的 worker 先过门再执行。
struct AdmitGate {
    limit: std::sync::atomic::AtomicUsize,
    workers: usize,
    running: Mutex<usize>,
    cvar: parking_lot::Condvar,
}

impl AdmitGate {
    fn new(workers: usize) -> Self {
        Self {
            limit: std::sync::atomic::AtomicUsize::new(workers),
            workers,
            running: Mutex::new(0),
            cvar: parking_lot::Condvar::new(),
        }
    }
    fn set_limit(&self, n: usize) {
        self.limit.store(n.clamp(1, self.workers), Ordering::SeqCst);
        self.cvar.notify_all();
    }
    fn acquire(&self) {
        let mut g = self.running.lock();
        loop {
            if *g < self.limit.load(Ordering::SeqCst) {
                *g += 1;
                return;
            }
            // parking_lot 就地更新 guard，超时返回只用于重查（防丢唤醒也防永睡）
            self.cvar
                .wait_for(&mut g, std::time::Duration::from_millis(200));
        }
    }
    fn release(&self) {
        *self.running.lock() -= 1;
        self.cvar.notify_all();
    }
}

pub struct OpQueue {
    tx: Option<std::sync::mpsc::Sender<Job>>,
    ctls: Mutex<HashMap<String, Arc<OpControl>>>,
    /// 活跃/近期操作最新进度快照（active() 用；worker 回调前先更新）
    latest: Arc<Mutex<HashMap<String, OpProgress>>>,
    store_dir: PathBuf,
    cb: ProgressFn,
    recycle: Option<Arc<dyn RecycleBinPort>>,
    admit: Arc<AdmitGate>,
    _workers: Vec<std::thread::JoinHandle<()>>,
}

impl OpQueue {
    /// 构建并启动 workers（docs/impl/05 F2：默认 2）
    pub fn new(store_dir: PathBuf, workers: usize, cb: ProgressFn) -> Result<Self, FileError> {
        std::fs::create_dir_all(&store_dir)?;
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let latest: Arc<Mutex<HashMap<String, OpProgress>>> = Arc::new(Mutex::new(HashMap::new()));
        let mut handles = Vec::new();
        let workers = workers.max(1);
        let admit = Arc::new(AdmitGate::new(workers));
        for _ in 0..workers {
            let rx = rx.clone();
            let cb = cb.clone();
            let latest = latest.clone();
            let admit = admit.clone();
            handles.push(std::thread::spawn(move || {
                // 快照更新包装：active() 永远拿到最新状态（每 worker 构造一次）
                let sink: ProgressFn = Arc::new(move |p: OpProgress| {
                    latest.lock().insert(p.op_id.clone(), p.clone());
                    cb(p);
                });
                loop {
                    // 持锁阻塞 recv：有 job 的 worker 取走后立即放锁，其余 worker 依次排队
                    let job = { rx.lock().recv() };
                    let Ok(job) = job else { return }; // 发送端关闭 → 退出
                    admit.acquire(); // max_concurrent 准入门（配置收紧时排队等位）
                    run_job(job, &sink);
                    admit.release();
                }
            }));
        }
        Ok(Self {
            tx: Some(tx),
            ctls: Mutex::new(HashMap::new()),
            latest,
            store_dir,
            cb,
            recycle: None,
            admit,
            _workers: handles,
        })
    }

    /// 配置真源 `max_concurrent` 的落点：夹到 `1..=内置 worker 数`
    /// （线程在建队时定死，配置不许超发线程——无事实源的承诺不给）
    pub fn set_max_concurrent(&self, n: usize) {
        self.admit.set_limit(n);
    }

    /// 注册回收站端口（Delete recycle=true 需要；未注册降级直删并告警）
    pub fn set_recycle_port(&mut self, port: Arc<dyn RecycleBinPort>) {
        self.recycle = Some(port);
    }

    /// 入队（Ask 策略的预扫描由上层 [`crate::conflict::scan_conflicts`] 完成）
    pub fn enqueue(&self, spec: OpSpec) -> Result<String, FileError> {
        self.enqueue_with_checkpoint(spec, None, None)
    }

    /// `resumed_from` 第三参属 T-B6-7 签名偏离登记：断点链要随 PendingOp
    /// 落盘才谈得上"重开服务链不丢"，二参口给不出这个事实源。
    pub fn enqueue_with_checkpoint(
        &self,
        spec: OpSpec,
        checkpoint: Option<Checkpoint>,
        resumed_from: Option<String>,
    ) -> Result<String, FileError> {
        // 方向唯一算式（承重④）：入队即裁决，混合端点 Err(FILE_REMOTE_003)，
        // 禁静默"当本地复制处理"
        let direction = direction_of(&spec.srcs, &spec.dst)?;
        // 承重⑥：远端源无回收站——recycle=true 在入队即拒（修前该臂静默直删）
        if spec.kind == OpKind::Delete
            && spec.recycle
            && spec
                .srcs
                .iter()
                .any(|e| matches!(e, OpEndpoint::Remote { .. }))
        {
            return Err(FileError::BadState(
                "远端源无回收站：回收站只覆盖本地盘，请改用彻底删除".into(),
            ));
        }
        let op_id = Uuid::now_v7().to_string();
        let op_id2 = op_id.clone();
        persist_pending(
            &self.store_dir,
            &PendingOp {
                op_id: op_id.clone(),
                kind: spec.kind,
                srcs: spec.srcs.clone(),
                dst: spec.dst.clone(),
                policy: spec.policy,
                recycle: spec.recycle,
                file_index: checkpoint.map(|c| c.file_index).unwrap_or(0),
                bytes_done: checkpoint.map(|c| c.bytes_done).unwrap_or(0),
                created_ms: now_ms(),
                resumed_from: resumed_from.clone(),
            },
        )?;
        let ctl = Arc::new(OpControl::new());
        self.ctls.lock().insert(op_id.clone(), ctl.clone());
        let progress = OpProgress {
            op_id: op_id.clone(),
            kind: spec.kind,
            state: OpState::Queued,
            current: String::new(),
            files_done: 0,
            files_total: 0,
            bytes_done: 0,
            bytes_total: 0,
            error: None,
            direction,
            resumable: None,
            resumed_from,
        };
        (self.cb)(progress.clone());
        let job_resumed = progress.resumed_from.clone();
        self.latest.lock().insert(op_id.clone(), progress);
        self.tx
            .as_ref()
            .expect("队列发送端存活")
            .send(Job {
                op_id: op_id2,
                spec,
                direction,
                checkpoint,
                resumed_from: job_resumed,
                ctl,
                store_dir: self.store_dir.clone(),
                recycle: self.recycle.clone(),
            })
            .map_err(|_| FileError::BadState("操作队列已关闭".into()))?;
        Ok(op_id)
    }

    /// 暂停（下一个块边界生效）
    pub fn pause(&self, op_id: &str) -> Result<(), FileError> {
        self.ctl(op_id)?.paused.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// 恢复：从 checkpoint 重新入队（原 Job 已在暂停点退出）。
    /// 返回新的 op_id；旧 pending 记录随之清理（新 op_id 下重建）。
    pub fn resume(&self, op_id: &str) -> Result<String, FileError> {
        let pending_path = self.store_dir.join(format!("{op_id}.json"));
        if !pending_path.exists() {
            return Err(FileError::NoSuchOp(op_id.to_owned()));
        }
        let raw = std::fs::read(&pending_path)?;
        let pending: PendingOp =
            serde_json::from_slice(&raw).map_err(|e| FileError::BadState(e.to_string()))?;
        if pending.op_id != op_id {
            return Err(FileError::BadState("pending 文件与 op_id 不符".into()));
        }
        let spec = OpSpec {
            kind: pending.kind,
            srcs: pending.srcs,
            dst: pending.dst,
            policy: pending.policy,
            recycle: pending.recycle,
        };
        let checkpoint = (pending.file_index > 0 || pending.bytes_done > 0).then_some(Checkpoint {
            file_index: pending.file_index,
            bytes_done: pending.bytes_done,
        });
        let new_op_id = self.enqueue_with_checkpoint(spec, checkpoint, Some(op_id.to_owned()))?;
        // 旧 pending 清理（新 op_id 下已重建；避免重复恢复）
        remove_pending(&self.store_dir, op_id);
        Ok(new_op_id)
    }

    /// 取消（下一个块边界生效；部分文件清理）
    pub fn cancel(&self, op_id: &str) -> Result<(), FileError> {
        let ctl = self.ctl(op_id)?;
        ctl.canceled.store(true, Ordering::SeqCst);
        ctl.paused.store(false, Ordering::SeqCst); // 取消优先于暂停
        Ok(())
    }

    fn ctl(&self, op_id: &str) -> Result<Arc<OpControl>, FileError> {
        self.ctls
            .lock()
            .get(op_id)
            .cloned()
            .ok_or_else(|| FileError::NoSuchOp(op_id.to_owned()))
    }

    /// 活跃/近期操作快照
    pub fn active(&self) -> Vec<OpProgress> {
        let mut v: Vec<OpProgress> = self.latest.lock().values().cloned().collect();
        v.sort_by(|a, b| a.op_id.cmp(&b.op_id));
        v
    }

    /// 单条操作的状态事实源（`xfer_status` 命令的口）：只读 `latest` 表，
    /// 查不到即 None——不为"看不见"编出"不存在"，也不回落空壳
    pub fn status(&self, op_id: &str) -> Option<OpProgress> {
        self.latest.lock().get(op_id).cloned()
    }

    /// 崩溃恢复扫描：pending_ops 目录里未完成的操作（docs/impl/01 S6.5）
    pub fn pending(&self) -> Vec<PendingOp> {
        read_pending(&self.store_dir)
    }

    /// 删除单条 pending 记录（完成后由 worker 自动清理；此处供 UI 显式丢弃）
    pub fn drop_pending(&self, op_id: &str) -> Result<bool, FileError> {
        let p = self.store_dir.join(format!("{op_id}.json"));
        if p.exists() {
            std::fs::remove_file(&p)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 关闭队列（丢弃发送端，worker 处理完存量后退出；测试用）
    pub fn close(mut self) {
        self.tx.take();
    }
}

impl Drop for OpQueue {
    fn drop(&mut self) {
        self.tx.take(); // 触发 worker 退出
    }
}

use host_core::util::now_ms;

// ---------------------------------------------------------------------------
// pending 持久化（原子写）
// ---------------------------------------------------------------------------

fn persist_pending(dir: &Path, pending: &PendingOp) -> Result<(), FileError> {
    let path = dir.join(format!("{}.json", pending.op_id));
    let tmp = dir.join(format!("{}.json.tmp", pending.op_id));
    let mut f = File::create(&tmp)?;
    f.write_all(
        serde_json::to_vec(pending)
            .map_err(|e| FileError::BadState(e.to_string()))?
            .as_slice(),
    )?;
    f.sync_all()?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn remove_pending(dir: &Path, op_id: &str) {
    let _ = std::fs::remove_file(dir.join(format!("{op_id}.json")));
}

pub fn read_pending(dir: &Path) -> Vec<PendingOp> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut out = Vec::new();
    for item in rd.flatten() {
        let p = item.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(raw) = std::fs::read(&p) {
            if let Ok(pending) = serde_json::from_slice::<PendingOp>(&raw) {
                out.push(pending);
            }
        }
    }
    out.sort_by_key(|p| p.created_ms);
    out
}

// ---------------------------------------------------------------------------
// Worker 执行
// ---------------------------------------------------------------------------

struct Reporter<'a> {
    cb: &'a ProgressFn,
    cur: OpProgress,
}

impl Reporter<'_> {
    fn set_state(&mut self, state: OpState) {
        self.cur.state = state;
        self.emit();
    }
    /// 上报进度快照。限频与合并统一由 EventBus::publish_merged 承担
    /// （D-03：operation.progress 200ms 窗口登记于 TOPIC_REGISTRY），
    /// 本模块不再自建节流计时器；终态由接线方 flush_merged 立即冲刷。
    fn emit(&mut self) {
        (self.cb)(self.cur.clone());
    }
}

fn run_job(job: Job, cb: &ProgressFn) {
    let Job {
        op_id,
        spec,
        direction,
        checkpoint,
        resumed_from,
        ctl,
        store_dir,
        recycle,
    } = job;
    let kind = spec.kind;
    let mut rep = Reporter {
        cb,
        cur: OpProgress {
            op_id,
            kind,
            state: OpState::Running,
            current: String::new(),
            files_done: 0,
            files_total: 0,
            bytes_done: 0,
            bytes_total: 0,
            error: None,
            direction,
            resumable: None,
            resumed_from,
        },
    };
    rep.set_state(OpState::Running);

    // 远端方向诚实拒绝（协议腿 T-B6-3/4/5/6 已立；队列执行器接线随驱动
    // 抽象泛化一并归 T-B6-11——本行只交付状态类型化与断点真值，执行不假绿）：
    let result = if direction != TransferDirection::Local {
        Flow::msg(format!(
            "远端传输（方向={direction:?}）的队列执行器接线归 09 §6.2 T-B6-11：协议腿已立于 T-B6-3..6，本操作未执行，禁假就绪"
        ))
    } else {
        match spec.kind {
            OpKind::Copy | OpKind::Move => {
                run_copy_move(&spec, checkpoint, &ctl, &mut rep, &store_dir)
            }
            OpKind::Delete => run_delete(&spec, &ctl, &mut rep, recycle.as_ref()),
            OpKind::Compress => run_compress(&spec, &ctl, &mut rep),
            OpKind::Extract => run_extract(&spec, &ctl, &mut rep),
        }
    };

    match result {
        Flow::Done => {
            remove_pending(&store_dir, &rep.cur.op_id);
            rep.set_state(OpState::Done);
        }
        Flow::Paused => rep.set_state(OpState::Paused),
        Flow::Canceled => {
            remove_pending(&store_dir, &rep.cur.op_id);
            rep.set_state(OpState::Canceled);
        }
        Flow::Failed(e) => {
            remove_pending(&store_dir, &rep.cur.op_id);
            // 承重③收口：远端方向的错误面只存脱敏口输出（OpProgress.error →
            // 总线事件 → pending_ops 一条链上不再出现 URL query 凭据/响应体原文）；
            // 本地臂逐字不变（io::Error.to_string() 原样，既有测零扰动）
            rep.cur.error = Some(if direction == TransferDirection::Local {
                e
            } else {
                crate::remote::remote_error_message(&e)
            });
            rep.set_state(OpState::Failed);
        }
    }
}

/// 展开后的扁平传输项
struct CopyItem {
    src: PathBuf,
    dst: PathBuf,
    size: u64,
}

/// dst 归一化：dst 是已存在目录 → dst/源名；否则 dst 即目标文件
fn target_for(src: &Path, dst: &Path) -> PathBuf {
    if dst.is_dir() {
        dst.join(src.file_name().unwrap_or_default())
    } else {
        dst.to_path_buf()
    }
}

/// 展开 srcs → 扁平 (src, dst, size) 计划；目录递归（walkdir）。
/// 资源管理器语义：
/// - dst 为已存在目录 → 目录源在 dst 下重建同名子树（dst/src_name/...）
/// - dst 不存在 → 目录源内容直接落到 dst（dst 作为新目录名）
/// - 文件源 → target_for 归一化
fn expand_plan(spec: &OpSpec) -> Result<Vec<CopyItem>, Flow> {
    let mut items = Vec::new();
    let dst = expect_local(&spec.dst)?;
    for src_ep in &spec.srcs {
        let src = expect_local(src_ep)?;
        let long_src = to_long_path(&src);
        if long_src.is_dir() {
            if dst.is_file() {
                return Err(Flow::msg(format!(
                    "目标是文件而源是目录: {}",
                    src.display()
                )));
            }
            // 基准：dst 已存在 → 源父目录（保留源目录名）；否则 → 源本身（内容直达 dst）
            let base = if dst.is_dir() {
                long_src.parent().unwrap_or(Path::new("/"))
            } else {
                long_src.as_path()
            };
            for entry in walkdir::WalkDir::new(&long_src).follow_links(false) {
                let entry = entry.map_err(|e| Flow::msg(e.to_string()))?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let md = entry.metadata().map_err(|e| Flow::msg(e.to_string()))?;
                let rel = entry
                    .path()
                    .strip_prefix(base)
                    .map_err(|e| Flow::msg(e.to_string()))?;
                items.push(CopyItem {
                    src: entry.path().to_path_buf(),
                    dst: dst.join(rel),
                    size: md.len(),
                });
            }
        } else if long_src.is_file() {
            let md = std::fs::metadata(&long_src).map_err(Flow::io)?;
            items.push(CopyItem {
                src: long_src,
                dst: target_for(&src, &dst),
                size: md.len(),
            });
        } else {
            return Err(Flow::msg(format!("源不存在: {}", src.display())));
        }
    }
    Ok(items)
}

fn run_copy_move(
    spec: &OpSpec,
    checkpoint: Option<Checkpoint>,
    ctl: &OpControl,
    rep: &mut Reporter,
    store_dir: &Path,
) -> Flow {
    let items = match expand_plan(spec) {
        Ok(v) => v,
        Err(e) => return e,
    };
    rep.cur.files_total = items.len() as u64;
    rep.cur.bytes_total = items.iter().map(|i| i.size).sum();

    // Move 单源快速路径：同卷 rename 瞬时完成（跨卷 rename 失败走逐项复制+删源）
    if spec.kind == OpKind::Move && spec.srcs.len() == 1 && checkpoint.is_none() {
        let src_local = local!(&spec.srcs[0]);
        let dst_local = local!(&spec.dst);
        let src = to_long_path(&src_local);
        let dst = target_for(&src_local, &dst_local);
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(to_long_path(parent));
        }
        if std::fs::rename(&src, to_long_path(&dst)).is_ok() {
            rep.cur.files_done = 1;
            rep.cur.bytes_done = rep.cur.bytes_total;
            rep.emit();
            return Flow::Done;
        }
        tracing::debug!("rename 快速路径失败，走逐项复制（跨卷）");
    }

    let start_index = checkpoint
        .map(|c| c.file_index.min(items.len()))
        .unwrap_or(0);
    let before_start: u64 = items[..start_index].iter().map(|i| i.size).sum();
    rep.cur.bytes_done = checkpoint
        .map(|c| c.bytes_done)
        .unwrap_or(0)
        .max(before_start);

    for (idx, item) in items.iter().enumerate() {
        if idx < start_index {
            rep.cur.files_done += 1;
            continue;
        }
        match gate(ctl) {
            Gate::Go => {}
            g => return g.into(),
        }
        if let Some(parent) = item.dst.parent() {
            if let Err(e) = std::fs::create_dir_all(to_long_path(parent)) {
                return Flow::io(e);
            }
        }
        // 冲突决议（T-B6-7 路线 (ii)）：Ask 到达执行臂且真出现冲突 ⇒ 真报错
        // 点名，绝不再与 Skip 同体静默跳过（承重⑩主证）
        let target = match resolve_target(&item.src, &item.dst, spec.policy) {
            Ok(Some(t)) => t,
            Ok(None) => {
                rep.cur.files_done += 1; // Skip
                rep.cur.bytes_done += item.size;
                rep.emit();
                continue;
            }
            Err(e) => return Flow::msg(e.to_string()),
        };
        rep.cur.current = item
            .src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bytes_in_file = if idx == start_index {
            checkpoint.map(|c| c.bytes_done).unwrap_or(0).min(item.size)
        } else {
            0
        };
        let res = copy_one(item, &target, bytes_in_file, ctl, rep, idx, store_dir, spec);
        if !matches!(res, Flow::Done) {
            return res;
        }
        rep.cur.files_done += 1;
        if spec.kind == OpKind::Move {
            if let Err(e) = std::fs::remove_file(to_long_path(&item.src)) {
                return Flow::io(e);
            }
        }
        rep.emit();
    }

    // Move：搬空的源目录收尾
    if spec.kind == OpKind::Move {
        for src_ep in &spec.srcs {
            let long = to_long_path(&local!(src_ep));
            if long.is_dir() {
                let _ = std::fs::remove_dir_all(&long);
            }
        }
    }
    Flow::Done
}

/// 可 seek 的读/写端（T-B6-2 落地补记：任务书写 `Box<dyn Read+Send>`，
/// 但 `copyPumped_resumeAtOffset_seeksBothEnds` 要求泵自身在 offset 处两端定位
/// ⇒ 句柄类型加 Seek 超轨；形状仍是 `Box<dyn …+Send>`，仅约束随职责收紧）
pub(crate) trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}
pub(crate) trait WriteSeek: Write + Seek + Send {}
impl<T: Write + Seek + Send> WriteSeek for T {}

/// 字节泵两端（本地 File 即其"两端皆本地"特例；远端腿自 T-B6-3 起接线）
pub(crate) struct PumpPair {
    pub reader: Box<dyn ReadSeek>,
    pub writer: Box<dyn WriteSeek>,
}

/// 泵的副作用出口：断点落盘 / 取消清理——本地臂接 pending 文件与 fs，
/// 远端臂将来接各自协议（泵本体零协议知识，禁第二份泵）
pub(crate) struct PumpHooks<'a> {
    /// 已写字节 → 断点持久化（暂停出口 + CHECKPOINT_EVERY 周期）
    pub persist: &'a mut dyn FnMut(u64),
    /// 取消出口：清理部分目标
    pub on_cancel: &'a mut dyn FnMut(),
}

/// 字节泵本体（任务书锚点 copy_one :662-780 的抽形）：从 offset 两端定位后
/// 逐块搬运。CHUNK 4MiB 与"非末块必须读满"（read_chunk）纪律逐字保留。
fn copy_pumped(
    pump: &mut PumpPair,
    offset: u64,
    ctl: &OpControl,
    rep: &mut Reporter,
    hooks: &mut PumpHooks,
) -> Flow {
    let mut written_in_file = offset;
    if offset > 0 {
        if pump.reader.seek(SeekFrom::Start(offset)).is_err() {
            return Flow::msg("续传 seek 失败（源端）");
        }
        if pump.writer.seek(SeekFrom::Start(offset)).is_err() {
            return Flow::msg("续传 seek 失败（目标端）");
        }
    }
    let mut last_ckpt = offset;
    let mut buf = vec![0u8; CHUNK];
    loop {
        match gate(ctl) {
            Gate::Pause => {
                // 断点落盘后暂停退出
                (hooks.persist)(written_in_file);
                return Flow::Paused;
            }
            Gate::Cancel => {
                (hooks.on_cancel)();
                return Flow::Canceled;
            }
            Gate::Go => {}
        }
        // 逐块读满（非末块 read_exact 语义；M4 K9 零洞教训）
        let n = match read_chunk(&mut pump.reader, &mut buf) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if n == 0 {
            break;
        }
        if let Err(e) = pump.writer.write_all(&buf[..n]) {
            return Flow::io(e);
        }
        written_in_file += n as u64;
        rep.cur.bytes_done += n as u64;
        rep.emit();
        if written_in_file - last_ckpt >= CHECKPOINT_EVERY {
            last_ckpt = written_in_file;
            (hooks.persist)(written_in_file);
        }
    }
    if let Err(e) = pump.writer.flush() {
        return Flow::io(e);
    }
    Flow::Done
}

/// 复制单个本地文件（支持从 bytes_done 续传）；尺寸终验。
/// 暂停 → 持久化断点；取消 → 清理部分文件。
/// 即 [`copy_pumped`] 的"两端皆本地"特例（任务书承重：真泵不收为特例即零收益）。
#[allow(clippy::too_many_arguments)] // 进度报告/断点续传上下文天然多参，私有 helper 不再包结构体
fn copy_one(
    item: &CopyItem,
    dst: &Path,
    bytes_done: u64,
    ctl: &OpControl,
    rep: &mut Reporter,
    file_index: usize,
    store_dir: &Path,
    spec: &OpSpec,
) -> Flow {
    let long_dst = to_long_path(dst);
    let reader = match File::open(&item.src) {
        Ok(f) => f,
        Err(e) => return Flow::io(e),
    };
    let total = item.size;

    let mut start = bytes_done;
    let writer = if start > 0
        && long_dst.exists()
        && std::fs::metadata(&long_dst).map(|m| m.len()).unwrap_or(0) == start
    {
        // 断点续传：从已写字节处追加（seek 由泵在两端执行）
        match OpenOptions::new().write(true).open(&long_dst) {
            Ok(f) => f,
            Err(e) => return Flow::io(e),
        }
    } else {
        start = 0;
        match File::create(&long_dst) {
            Ok(f) => f,
            Err(e) => return Flow::io(e),
        }
    };
    let mut pump = PumpPair {
        reader: Box::new(reader),
        writer: Box::new(writer),
    };
    // op_id 先行取快照：闭包不持 rep 的可变借用，泵才能独享 Reporter
    let op_id_snap = rep.cur.op_id.clone();
    let chain_snap = rep.cur.resumed_from.clone();
    let mut persist = |written: u64| {
        let _ = persist_pending(
            store_dir,
            &PendingOp {
                op_id: op_id_snap.clone(),
                kind: spec.kind,
                srcs: spec.srcs.clone(),
                dst: spec.dst.clone(),
                policy: spec.policy,
                recycle: spec.recycle,
                file_index,
                bytes_done: written,
                resumed_from: chain_snap.clone(),
                created_ms: now_ms(),
            },
        );
    };
    let mut on_cancel = || {
        let _ = std::fs::remove_file(&long_dst);
    };
    let mut hooks = PumpHooks {
        persist: &mut persist,
        on_cancel: &mut on_cancel,
    };
    let flow = copy_pumped(&mut pump, start, ctl, rep, &mut hooks);
    drop(pump); // 释放句柄后再读终尺寸（Windows 占用面）
    if !matches!(flow, Flow::Done) {
        return flow;
    }
    // 尺寸终验（本地臂专属：目标 metadata 是唯一事实源）
    let written = match std::fs::metadata(&long_dst) {
        Ok(m) => m.len(),
        Err(e) => return Flow::io(e),
    };
    if written != total {
        return Flow::msg(format!(
            "复制尺寸不符 src={total} dst={written}: {}",
            dst.display()
        ));
    }
    Flow::Done
}

/// 读满 buf 或到 EOF；返回字节数（0 = EOF）
fn read_chunk<R: Read + ?Sized>(reader: &mut R, buf: &mut [u8]) -> Result<usize, Flow> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Flow::io(e)),
        }
    }
    Ok(filled)
}

fn run_delete(
    spec: &OpSpec,
    ctl: &OpControl,
    rep: &mut Reporter,
    recycle: Option<&Arc<dyn RecycleBinPort>>,
) -> Flow {
    // 本臂只认本地端点（远端+recycle 已在入队口拒，远端直删腿自 T-B6-5/6 起）
    let mut srcs: Vec<PathBuf> = Vec::with_capacity(spec.srcs.len());
    for src_ep in &spec.srcs {
        srcs.push(local!(src_ep));
    }
    // 统计字节（回收站整批交接，无逐文件进度）
    let mut bytes_total = 0u64;
    let mut file_count = 0u64;
    for src in &srcs {
        let long = to_long_path(src);
        let walker = if long.is_dir() {
            walkdir::WalkDir::new(&long).into_iter()
        } else {
            walkdir::WalkDir::new(long.parent().unwrap_or(Path::new("/")))
                .max_depth(0)
                .into_iter()
        };
        for entry in walker.flatten() {
            if entry.file_type().is_file() {
                file_count += 1;
                bytes_total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    rep.cur.files_total = file_count;
    rep.cur.bytes_total = bytes_total;

    if spec.recycle {
        match recycle {
            Some(port) => match port.delete(&srcs) {
                Ok(_) => {
                    rep.cur.files_done = file_count;
                    rep.cur.bytes_done = bytes_total;
                    rep.emit();
                    return Flow::Done;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "回收站删除失败，降级直删");
                }
            },
            None => tracing::warn!("RecycleBinPort 未注册，回收站删除降级为直删"),
        }
    }
    for src in &srcs {
        match gate(ctl) {
            Gate::Go => {}
            g => return g.into(),
        }
        let long = to_long_path(src);
        rep.cur.current = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let r = if long.is_dir() {
            std::fs::remove_dir_all(&long)
        } else if long.is_file() {
            std::fs::remove_file(&long).map(|_| ())
        } else {
            Ok(())
        };
        if let Err(e) = r {
            return Flow::io(e);
        }
        rep.cur.files_done += 1;
        rep.emit();
    }
    Flow::Done
}

struct CompressItem {
    src: PathBuf,
    rel: PathBuf,
    size: u64,
}

/// 压缩计划：目录以其名称为 zip 内根（`dir/a.txt`）。
/// Compress/Extract 无可续断点（三臂不接 checkpoint），resumable 恒 `Whole`
/// ——§6.1④ 裁定 (ii)：本行只把该事实变得可表达，不改其行为。
fn expand_plan_for_compress(spec: &OpSpec) -> Result<Vec<CompressItem>, Flow> {
    let mut items = Vec::new();
    for src_ep in &spec.srcs {
        let src = expect_local(src_ep)?;
        let long = to_long_path(&src);
        let base_name = src.file_name().unwrap_or_default().to_os_string();
        if long.is_dir() {
            // rel 相对源目录本身，再前置目录名 → zip 内 `dir_name/...`
            for entry in walkdir::WalkDir::new(&long).follow_links(false) {
                let entry = entry.map_err(|e| Flow::msg(e.to_string()))?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry
                    .path()
                    .strip_prefix(&long)
                    .map_err(|e| Flow::msg(e.to_string()))?;
                let md = entry.metadata().map_err(|e| Flow::msg(e.to_string()))?;
                items.push(CompressItem {
                    src: entry.path().to_path_buf(),
                    rel: PathBuf::from(&base_name).join(rel),
                    size: md.len(),
                });
            }
        } else if long.is_file() {
            let md = std::fs::metadata(&long).map_err(Flow::io)?;
            items.push(CompressItem {
                src: long,
                rel: PathBuf::from(&base_name),
                size: md.len(),
            });
        } else {
            return Err(Flow::msg(format!("源不存在: {}", src.display())));
        }
    }
    Ok(items)
}

fn run_compress(spec: &OpSpec, ctl: &OpControl, rep: &mut Reporter) -> Flow {
    let items = match expand_plan_for_compress(spec) {
        Ok(v) => v,
        Err(e) => return e,
    };
    rep.cur.files_total = items.len() as u64;
    rep.cur.bytes_total = items.iter().map(|i| i.size).sum();
    let dst = to_long_path(&local!(&spec.dst));
    // T-B6-7 同闸：Ask 下 `File::create` 会把已存在压缩包静默截断重写——
    // 那是没决议过的冲突被自称决议过
    if spec.policy == ConflictPolicy::Ask && dst.exists() {
        return Flow::msg(format!(
            "Ask 冲突未经决议: 压缩包已存在 {} ——须决议后重新入队（本操作未计入完成）",
            dst.display()
        ));
    }
    if let Some(parent) = dst.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Flow::io(e);
        }
    }
    let file = match File::create(&dst) {
        Ok(f) => f,
        Err(e) => return Flow::io(e),
    };
    let mut zw = ZipWriter::new(std::io::BufWriter::new(file));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for item in &items {
        match gate(ctl) {
            Gate::Pause | Gate::Cancel => {
                drop(zw);
                let _ = std::fs::remove_file(&dst);
                return gate(ctl).into();
            }
            Gate::Go => {}
        }
        let arc_name = item.rel.to_string_lossy().replace('\\', "/");
        if let Err(e) = zw.start_file(arc_name, opts) {
            return Flow::Failed(FileError::Zip(e.to_string()).to_string());
        }
        let mut f = match File::open(&item.src) {
            Ok(f) => f,
            Err(e) => return Flow::io(e),
        };
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = match read_chunk(&mut f, &mut buf) {
                Ok(n) => n,
                Err(e) => return e,
            };
            if n == 0 {
                break;
            }
            if let Err(e) = zw.write_all(&buf[..n]) {
                return Flow::Failed(FileError::Zip(e.to_string()).to_string());
            }
            rep.cur.bytes_done += n as u64;
            rep.emit();
        }
        rep.cur.files_done += 1;
        rep.emit();
    }
    if let Err(e) = zw.finish() {
        return Flow::Failed(FileError::Zip(e.to_string()).to_string());
    }
    Flow::Done
}

fn zip_err(e: ZipError) -> Flow {
    Flow::Failed(FileError::Zip(e.to_string()).to_string())
}

fn run_extract(spec: &OpSpec, ctl: &OpControl, rep: &mut Reporter) -> Flow {
    let zip_path = match spec.srcs.first() {
        Some(p) => local!(p),
        None => return Flow::msg("缺少压缩包路径"),
    };
    let root = local!(&spec.dst);
    let file = match File::open(to_long_path(&zip_path)) {
        Ok(f) => f,
        Err(e) => return Flow::io(e),
    };
    let mut archive = match ZipArchive::new(file) {
        Ok(a) => a,
        Err(e) => return zip_err(e),
    };
    rep.cur.files_total = archive.len() as u64;
    let mut bytes_total = 0u64;
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            bytes_total += entry.size();
        }
    }
    rep.cur.bytes_total = bytes_total;

    if let Err(e) = std::fs::create_dir_all(to_long_path(&root)) {
        return Flow::io(e);
    }
    for i in 0..archive.len() {
        match gate(ctl) {
            Gate::Go => {}
            g => return g.into(),
        }
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(e) => return zip_err(e),
        };
        // zip-slip 防护：拒绝逃逸根目录的条目
        let name = entry.name().to_owned();
        if name.contains("..") || Path::new(&name).is_absolute() {
            tracing::warn!(name = %name, "跳过可疑 zip 条目");
            rep.cur.files_done += 1;
            continue;
        }
        let out_path = root.join(&name);
        if entry.is_dir() {
            if let Err(e) = std::fs::create_dir_all(to_long_path(&out_path)) {
                return Flow::io(e);
            }
            rep.cur.files_done += 1;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            if let Err(e) = std::fs::create_dir_all(to_long_path(parent)) {
                return Flow::io(e);
            }
        }
        // 包内冲突：Skip 之外…——T-B6-7 收口：Ask 不得被当成"已决议"，
        // 静默改名与静默覆盖同罪；Overwrite 显式放行
        let final_path = if out_path.exists() {
            match spec.policy {
                ConflictPolicy::Overwrite => out_path,
                ConflictPolicy::Skip => {
                    rep.cur.bytes_done += entry.size();
                    rep.cur.files_done += 1;
                    rep.emit();
                    continue;
                }
                ConflictPolicy::Ask => {
                    return Flow::msg(format!(
                        "Ask 冲突未经决议: 解压目标已存在 {} ——须决议后重新入队（本操作未计入完成）",
                        out_path.display()
                    ));
                }
                ConflictPolicy::Rename => unique_target(&out_path),
            }
        } else {
            out_path
        };
        rep.cur.current = name;
        let mut out = match File::create(to_long_path(&final_path)) {
            Ok(o) => o,
            Err(e) => return Flow::io(e),
        };
        if let Err(e) = std::io::copy(&mut entry, &mut out) {
            return Flow::io(e);
        }
        rep.cur.bytes_done += entry.size();
        rep.cur.files_done += 1;
        rep.emit();
    }
    Flow::Done
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_file_ops_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_tree(root: &Path) {
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        std::fs::write(root.join("a.bin"), vec![7u8; 5 * 1024 * 1024]).unwrap();
        std::fs::write(root.join("sub/b.txt"), b"hello world").unwrap();
        std::fs::write(root.join("sub/deep/c.txt"), b"x").unwrap();
    }

    fn sink() -> (ProgressFn, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let c2 = count.clone();
        (
            Arc::new(move |_| {
                c2.fetch_add(1, Ordering::SeqCst);
            }),
            count,
        )
    }

    fn wait_until(pred: impl Fn() -> bool, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if pred() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn op_state(q: &OpQueue, op: &str) -> OpState {
        q.active()
            .iter()
            .find(|p| p.op_id == op)
            .map(|p| p.state)
            .unwrap_or(OpState::Queued)
    }

    // 端点速构 helper：L=Local、R=Remote，单字母即语义
    #[allow(non_snake_case)]
    fn L(p: impl Into<PathBuf>) -> OpEndpoint {
        OpEndpoint::local(p)
    }
    #[allow(non_snake_case)]
    fn R(driver_id: &str, path: &str) -> OpEndpoint {
        OpEndpoint::Remote {
            driver_id: driver_id.to_owned(),
            path: path.to_owned(),
        }
    }
    fn direction_of_op(q: &OpQueue, op: &str) -> TransferDirection {
        q.active()
            .iter()
            .find(|p| p.op_id == op)
            .expect("快照在场")
            .direction
    }

    #[test]
    fn copy_dir_recursive_with_progress_and_done() {
        let src = tmpdir("copy_src");
        let dst = tmpdir("copy_dst");
        make_tree(&src);
        let (cb, _) = sink();
        let store = tmpdir("copy_store");
        let q = OpQueue::new(store.clone(), 2, cb).unwrap();
        let op = q
            .enqueue(OpSpec {
                kind: OpKind::Copy,
                srcs: vec![L(src.clone())],
                dst: L(dst.clone()),
                policy: ConflictPolicy::Overwrite,
                recycle: false,
            })
            .unwrap();
        // dst 为已存在目录 → 在 dst 下重建同名子树（资源管理器语义）
        let src_name = src.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            wait_until(
                || std::fs::metadata(dst.join(&src_name).join("sub/deep/c.txt")).is_ok(),
                Duration::from_secs(10)
            ),
            "递归复制应完成"
        );
        assert_eq!(
            std::fs::read(dst.join(&src_name).join("sub/b.txt")).unwrap(),
            b"hello world"
        );
        // 完成后 pending 清理 + 终态 Done + 统计正确
        assert!(wait_until(
            || op_state(&q, &op) == OpState::Done,
            Duration::from_secs(5)
        ));
        assert!(q.pending().is_empty());
        let active = q.active();
        assert_eq!(
            active.iter().find(|p| p.op_id == op).unwrap().files_total,
            3
        );
        assert_eq!(
            active.iter().find(|p| p.op_id == op).unwrap().bytes_total,
            5 * 1024 * 1024 + 11 + 1
        );
        q.close();
        let _ = std::fs::remove_dir_all(&store);
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn move_file_rename_fast_path() {
        let src = tmpdir("mv_src");
        let dst = tmpdir("mv_dst");
        make_tree(&src);
        let (cb, _) = sink();
        let store = tmpdir("mv_store");
        let q = OpQueue::new(store, 1, cb).unwrap();
        q.enqueue(OpSpec {
            kind: OpKind::Move,
            srcs: vec![L(src.join("sub/b.txt"))],
            dst: L(dst.join("b.txt")),
            policy: ConflictPolicy::Overwrite,
            recycle: false,
        })
        .unwrap();
        assert!(wait_until(
            || dst.join("b.txt").exists() && !src.join("sub/b.txt").exists(),
            Duration::from_secs(10)
        ));
        q.close();
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn move_dir_cross_volume_falls_back_to_copy_delete() {
        // C:\Users 临时目录可能同卷，但语义路径验证：目录 move 后源树消失
        let src = tmpdir("mvd_src");
        let dst = tmpdir("mvd_dst");
        make_tree(&src);
        let (cb, _) = sink();
        let store = tmpdir("mvd_store");
        let q = OpQueue::new(store, 1, cb).unwrap();
        // 目标为已存在目录 → rename 到 dst/src_name；模拟跨卷失败路径用逐项校验
        q.enqueue(OpSpec {
            kind: OpKind::Move,
            srcs: vec![L(src.clone())],
            dst: L(dst.clone()),
            policy: ConflictPolicy::Overwrite,
            recycle: false,
        })
        .unwrap();
        let src_name = src.file_name().unwrap().to_string_lossy().into_owned();
        assert!(wait_until(
            || dst.join(&src_name).join("sub/b.txt").exists() && !src.exists(),
            Duration::from_secs(15)
        ));
        q.close();
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn rename_policy_on_conflict() {
        let src = tmpdir("conf_src");
        let dst = tmpdir("conf_dst");
        std::fs::write(src.join("f.txt"), b"new").unwrap();
        std::fs::write(dst.join("f.txt"), b"old").unwrap();
        let (cb, _) = sink();
        let store = tmpdir("conf_store");
        let q = OpQueue::new(store, 1, cb).unwrap();
        q.enqueue(OpSpec {
            kind: OpKind::Copy,
            srcs: vec![L(src.join("f.txt"))],
            dst: L(dst.clone()),
            policy: ConflictPolicy::Rename,
            recycle: false,
        })
        .unwrap();
        assert!(wait_until(
            || std::fs::read(dst.join("f (2).txt")).is_ok_and(|b| b == b"new"),
            Duration::from_secs(10)
        ));
        assert_eq!(std::fs::read(dst.join("f.txt")).unwrap(), b"old");
        q.close();
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn delete_permanent_directory() {
        let src = tmpdir("del_src");
        make_tree(&src);
        let (cb, _) = sink();
        let store = tmpdir("del_store");
        let q = OpQueue::new(store, 1, cb).unwrap();
        q.enqueue(OpSpec {
            kind: OpKind::Delete,
            srcs: vec![L(src.clone())],
            dst: L(PathBuf::new()),
            policy: ConflictPolicy::default(),
            recycle: false,
        })
        .unwrap();
        assert!(wait_until(|| !src.exists(), Duration::from_secs(10)));
        q.close();
    }

    #[test]
    fn compress_and_extract_roundtrip() {
        let src = tmpdir("zip_src");
        let dst = tmpdir("zip_out");
        make_tree(&src);
        let zip_root = src.file_name().unwrap().to_string_lossy().into_owned();
        let zipfile = dst.join("pack.zip");
        let (cb, _) = sink();
        let store = tmpdir("zip_store");
        let q = OpQueue::new(store, 1, cb).unwrap();
        q.enqueue(OpSpec {
            kind: OpKind::Compress,
            srcs: vec![L(src)],
            dst: L(zipfile.clone()),
            policy: ConflictPolicy::default(),
            recycle: false,
        })
        .unwrap();
        assert!(wait_until(
            || op_state(
                &q,
                &q.active()
                    .first()
                    .map(|p| p.op_id.clone())
                    .unwrap_or_default()
            ) == OpState::Done
                && zipfile.exists(),
            Duration::from_secs(20)
        ));

        let ext_dir = dst.join("unpacked");
        let op2 = q
            .enqueue(OpSpec {
                kind: OpKind::Extract,
                srcs: vec![L(zipfile)],
                dst: L(ext_dir.clone()),
                policy: ConflictPolicy::Overwrite,
                recycle: false,
            })
            .unwrap();
        assert!(wait_until(
            || std::fs::read(ext_dir.join(&zip_root).join("sub/deep/c.txt"))
                .is_ok_and(|b| b == b"x"),
            Duration::from_secs(20)
        ));
        assert!(wait_until(
            || op_state(&q, &op2) == OpState::Done,
            Duration::from_secs(10)
        ));
        assert_eq!(
            std::fs::metadata(ext_dir.join(&zip_root).join("a.bin"))
                .unwrap()
                .len(),
            5 * 1024 * 1024
        );
        q.close();
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn pause_then_resume_completes_big_file() {
        let src = tmpdir("pause_src");
        let dst = tmpdir("pause_dst");
        std::fs::write(src.join("big.bin"), vec![9u8; 24 * 1024 * 1024]).unwrap();
        let (cb, _) = sink();
        let store = tmpdir("pause_store");
        let q = OpQueue::new(store.clone(), 1, cb).unwrap();
        let op = q
            .enqueue(OpSpec {
                kind: OpKind::Copy,
                srcs: vec![L(src.join("big.bin"))],
                dst: L(dst.join("big.bin")),
                policy: ConflictPolicy::Overwrite,
                recycle: false,
            })
            .unwrap();
        // 入队后立刻暂停：块边界生效（极快机器上可能已完成）
        q.pause(&op).unwrap();
        assert!(wait_until(
            || matches!(op_state(&q, &op), OpState::Paused | OpState::Done),
            Duration::from_secs(10)
        ));
        if op_state(&q, &op) == OpState::Paused {
            assert!(!q.pending().is_empty(), "暂停应保留 pending 断点");
            let op2 = q.resume(&op).unwrap();
            assert_ne!(op2, op);
            assert!(wait_until(
                || op_state(&q, &op2) == OpState::Done,
                Duration::from_secs(20)
            ));
        } else {
            assert!(wait_until(
                || op_state(&q, &op) == OpState::Done,
                Duration::from_secs(20)
            ));
        }
        assert_eq!(
            std::fs::metadata(dst.join("big.bin")).unwrap().len(),
            24 * 1024 * 1024
        );
        q.close();
        let _ = std::fs::remove_dir_all(&store);
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn cancel_removes_partial_dst() {
        let src = tmpdir("cancel_src");
        let dst = tmpdir("cancel_dst");
        std::fs::write(src.join("big.bin"), vec![1u8; 24 * 1024 * 1024]).unwrap();
        let (cb, _) = sink();
        let store = tmpdir("cancel_store");
        let q = OpQueue::new(store.clone(), 1, cb).unwrap();
        let op = q
            .enqueue(OpSpec {
                kind: OpKind::Copy,
                srcs: vec![L(src.join("big.bin"))],
                dst: L(dst.join("big.bin")),
                policy: ConflictPolicy::Overwrite,
                recycle: false,
            })
            .unwrap();
        q.cancel(&op).unwrap();
        assert!(wait_until(
            || matches!(op_state(&q, &op), OpState::Canceled | OpState::Done),
            Duration::from_secs(10)
        ));
        if op_state(&q, &op) == OpState::Canceled {
            assert!(!dst.join("big.bin").exists(), "取消应清理部分文件");
        }
        q.close();
        let _ = std::fs::remove_dir_all(&store);
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    // ---- T-B6-2 队列方向化（09 §6.2 八枚字面测名） ----

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-2）字面测试名优先于 rustc 命名惯例
    fn pendingLegacy_localFormatBytes_stillDeserializes() {
        // 承重⑤ 主证：旧 pending JSON（裸字符串端点，含 Windows 反斜杠与中文路径）
        // 继续落 OpEndpoint::Local——零迁移、零版本协商
        let legacy = r#"{"op_id":"op-legacy-1","kind":"copy","srcs":["C:\\用户\\me\\资料 档\\a.txt","D:/plain/b.bin"],"dst":"C:\\out\\a.txt","policy":"overwrite","recycle":false,"file_index":0,"bytes_done":0,"created_ms":1700000000000}"#;
        let p: PendingOp = serde_json::from_str(legacy).unwrap();
        assert_eq!(p.srcs.len(), 2);
        assert_eq!(
            p.srcs[0],
            OpEndpoint::Local(PathBuf::from(r"C:\用户\me\资料 档\a.txt"))
        );
        assert_eq!(
            p.srcs[1],
            OpEndpoint::Local(PathBuf::from("D:/plain/b.bin"))
        );
        assert_eq!(
            p.dst.as_local().unwrap().as_path(),
            Path::new(r"C:\out\a.txt")
        );
        // 新格式远端臂序列化为对象、反序列化回同值（untagged 第二候选）
        let rp = PendingOp {
            op_id: "op-remote-1".into(),
            kind: OpKind::Copy,
            srcs: vec![R("remote:webdav-1", "/docs/报告.docx")],
            dst: L(r"C:\download\报告.docx"),
            policy: ConflictPolicy::Overwrite,
            recycle: false,
            file_index: 0,
            bytes_done: 0,
            resumed_from: None,
            created_ms: 1,
        };
        let raw = serde_json::to_string(&rp).unwrap();
        let back: PendingOp = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.srcs[0], rp.srcs[0]);
        assert_eq!(back.dst, rp.dst);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-2）字面测试名优先于 rustc 命名惯例
    fn direction_of_uploadDownloadAndMixed() {
        let loc = L(r"C:\x\a.txt");
        let rem = R("remote:webdav-1", "/docs/a.txt");
        // 四臂：全本地 / 上传 / 下载 / 混合两端
        assert_eq!(
            direction_of(std::slice::from_ref(&loc), &loc).unwrap(),
            TransferDirection::Local
        );
        assert_eq!(
            direction_of(std::slice::from_ref(&loc), &rem).unwrap(),
            TransferDirection::Upload
        );
        assert_eq!(
            direction_of(&[rem.clone(), R("remote:sftp-1", "/b")], &loc).unwrap(),
            TransferDirection::Download
        );
        // 混合（部分源远端）与远端→远端都必 Err——禁静默"当本地复制处理"
        for (srcs, dst) in [
            (vec![rem.clone(), loc.clone()], loc.clone()),
            (vec![rem.clone(), loc.clone()], rem.clone()),
            (vec![rem.clone()], rem.clone()),
        ] {
            let e = direction_of(&srcs, &dst).unwrap_err();
            assert_eq!(e.code(), crate::error::FILE_REMOTE_MIXED);
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-2）字面测试名优先于 rustc 命名惯例
    fn direction_of_matchesPanelCopySemantics() {
        // 回归既有形状：两 src.parent() 相同 ⇒ 同桶（抽函数不改 target_for 语义）
        let a = L(r"C:\dst\one.txt");
        let b = L(r"C:\dst\two.txt");
        assert_eq!(parent_key(&a), parent_key(&b));
        assert_eq!(parent_key(&a), r"C:\dst");
        let c = R("remote:webdav-1", "/data/x.txt");
        let d = R("remote:webdav-1", "/data/y.txt");
        assert_eq!(parent_key(&c), parent_key(&d));
        assert_eq!(parent_key(&c), "/data");
        // 同桶本地复制到同目录另一文件：方向仍是 Local，面板零新文案
        let (cb, _) = sink();
        let store = tmpdir("dirsem_store");
        let q = OpQueue::new(store.clone(), 1, cb).unwrap();
        let op = q
            .enqueue(OpSpec {
                kind: OpKind::Copy,
                srcs: vec![a.clone()],
                dst: L(r"C:\dst\copy_one.txt"),
                policy: ConflictPolicy::Overwrite,
                recycle: false,
            })
            .unwrap();
        assert_eq!(direction_of_op(&q, &op), TransferDirection::Local);
        q.close();
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-2）字面测试名优先于 rustc 命名惯例
    fn parentKey_pureNoFilesystemCall() {
        // 纯函数判据=对不存在的路径照样给值、无 Err 分支、零 fs 参与
        assert_eq!(parent_key(&L(r"Z:\不存在\深\f.txt")), r"Z:\不存在\深");
        assert_eq!(parent_key(&R("webdav", "/no/such/远程.txt")), "/no/such");
        assert_eq!(parent_key(&R("webdav", "/f.txt")), "/");
        assert_eq!(parent_key(&R("webdav", "f.txt")), "");
        assert_eq!(parent_key(&L(PathBuf::new())), "");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-2）字面测试名优先于 rustc 命名惯例
    fn copyPumped_resumeAtOffset_seeksBothEnds() {
        let d = tmpdir("pump");
        let src_path = d.join("src.bin");
        let dst_path = d.join("dst.bin");
        let total = CHUNK as u64 + 1024;
        let payload: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src_path, &payload).unwrap();
        // 镜像 copy_one 的 `metadata(dst).len()==start` 前提：前 CHUNK 字节已在目标
        std::fs::write(&dst_path, &payload[..CHUNK]).unwrap();
        let (cb, _) = sink();
        let mut rep = Reporter {
            cb: &cb,
            cur: OpProgress {
                op_id: "pump".into(),
                kind: OpKind::Copy,
                state: OpState::Running,
                current: "src.bin".into(),
                files_done: 0,
                files_total: 1,
                bytes_done: 0,
                bytes_total: total,
                error: None,
                direction: TransferDirection::Local,
                resumable: None,
                resumed_from: None,
            },
        };
        let ctl = OpControl::new();
        let mut pump = PumpPair {
            reader: Box::new(File::open(&src_path).unwrap()),
            writer: Box::new(OpenOptions::new().write(true).open(&dst_path).unwrap()),
        };
        let mut persisted: Vec<u64> = Vec::new();
        let mut persist = |w: u64| persisted.push(w);
        let mut on_cancel = || {};
        let mut hooks = PumpHooks {
            persist: &mut persist,
            on_cancel: &mut on_cancel,
        };
        let flow = copy_pumped(&mut pump, CHUNK as u64, &ctl, &mut rep, &mut hooks);
        assert!(matches!(flow, Flow::Done));
        drop(pump);
        // 目标不被截断（offset 前字节保真）且只补了尾部
        assert_eq!(std::fs::metadata(&dst_path).unwrap().len(), total);
        assert_eq!(std::fs::read(&dst_path).unwrap(), payload);
        // 源确被 seek：进度只计新搬运的 1024 字节（未 seek 则会计满 CHUNK+1024）
        assert_eq!(rep.cur.bytes_done, 1024);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-2）字面测试名优先于 rustc 命名惯例
    fn remoteDelete_recycleTrue_rejectsAndNamesNoRecycleBin() {
        // 承重⑥：修前该臂静默直删（回收站端口收下 PathBuf 列表根本不含远端腿）
        let (cb, _) = sink();
        let store = tmpdir("rdel_store");
        let mut q = OpQueue::new(store.clone(), 1, cb).unwrap();
        let e = q
            .enqueue(OpSpec {
                kind: OpKind::Delete,
                srcs: vec![R("remote:webdav-1", "/docs/报告.docx")],
                dst: L(PathBuf::new()),
                policy: ConflictPolicy::default(),
                recycle: true,
            })
            .unwrap_err();
        assert_eq!(e.code(), "FILE_OPS_003");
        assert!(e.to_string().contains("无回收站"), "{e}");
        // 正对照防空洞：本地源 + recycle=true 不被这道闸拦（缺端口降级直删是既有语义）
        let loc = tmpdir("rdel_loc");
        std::fs::write(loc.join("f.txt"), b"x").unwrap();
        q.set_recycle_port(Arc::new(FakeRecycleNever));
        assert!(q
            .enqueue(OpSpec {
                kind: OpKind::Delete,
                srcs: vec![L(loc.join("f.txt"))],
                dst: L(PathBuf::new()),
                policy: ConflictPolicy::default(),
                recycle: true,
            })
            .is_ok());
        q.close();
        let _ = std::fs::remove_dir_all(&store);
        let _ = std::fs::remove_dir_all(&loc);
    }

    /// 永远"失败"的回收站端口：证明正对照走的是入队闸而非端口感情
    struct FakeRecycleNever;
    impl RecycleBinPort for FakeRecycleNever {
        fn delete(&self, _paths: &[PathBuf]) -> Result<u32, host_core::error::AppError> {
            Err(host_core::error::AppError::module(
                "TEST",
                "fake recycle never",
                None,
            ))
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-2）字面测试名优先于 rustc 命名惯例
    fn queue_resumeAcrossReopen_remoteDirectionSurvives() {
        // 落 PendingOp（远端源+本地目标+断点，绕过 worker 竞态）→ 重开 OpQueue →
        // resume 后 active() 的 direction 与 checkpoint 不丢
        let store = tmpdir("dirsurv_store");
        let op_id = "op-remote-legacy";
        persist_pending(
            &store,
            &PendingOp {
                op_id: op_id.to_owned(),
                kind: OpKind::Copy,
                srcs: vec![R("remote:webdav-1", "/docs/a.bin")],
                dst: L(r"C:\download\a.bin"),
                policy: ConflictPolicy::Overwrite,
                recycle: false,
                file_index: 2,
                bytes_done: 4 * 1024 * 1024,
                resumed_from: None,
                created_ms: 1,
            },
        )
        .unwrap();
        let (cb, _) = sink();
        let q = OpQueue::new(store.clone(), 1, cb).unwrap();
        let pend = q.pending();
        assert_eq!(pend.len(), 1);
        assert!(
            matches!(&pend[0].srcs[0], OpEndpoint::Remote { driver_id, path }
                if driver_id == "remote:webdav-1" && path == "/docs/a.bin"),
            "远端端点须在重开后原样可读"
        );
        assert_eq!(
            (pend[0].file_index, pend[0].bytes_done),
            (2, 4 * 1024 * 1024)
        );
        let new_op = q.resume(op_id).unwrap();
        assert_ne!(new_op, op_id);
        // direction 由 direction_of 在入队口唯一派生：远端源 + 本地目标 ⇒ Download
        assert_eq!(direction_of_op(&q, &new_op), TransferDirection::Download);
        // 诚实拒绝：远端腿 T-B6-3 起接线，worker 判 Failed 点名该行（不假称 Done）
        assert!(wait_until(
            || op_state(&q, &new_op) == OpState::Failed,
            Duration::from_secs(10)
        ));
        let err = q
            .active()
            .iter()
            .find(|p| p.op_id == new_op)
            .unwrap()
            .error
            .clone()
            .unwrap();
        assert!(err.contains("T-B6-3"), "{err}");
        assert!(q.pending().is_empty());
        q.close();
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-7）字面测试名优先于 rustc 命名惯例
    fn xferStatus_shapeKeysExactlyMatchStruct() {
        // 序列化键集恰等声明字段集（防"偷偷加键/漏键"）；三枚传输新键在场。
        let p = OpProgress {
            op_id: "op-1".into(),
            kind: OpKind::Copy,
            state: OpState::Running,
            current: "a.bin".into(),
            files_done: 1,
            files_total: 2,
            bytes_done: 10,
            bytes_total: 20,
            error: None,
            direction: TransferDirection::Download,
            resumable: Some(crate::remote::Resumable::Range),
            resumed_from: Some("op-0".into()),
        };
        let v = serde_json::to_value(XferStatusDto::from(&p)).unwrap();
        let got: Vec<&str> = {
            let mut k: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
            k.sort_unstable();
            k
        };
        let mut want = vec![
            "bytes_done",
            "bytes_total",
            "current",
            "direction",
            "error",
            "files_done",
            "files_total",
            "kind",
            "op_id",
            "resumable",
            "resumed_from",
            "state",
        ];
        want.sort_unstable();
        assert_eq!(got, want, "XferStatusDto 序列化键集必须恰等声明字段集");
        // OpProgress（事件 payload 同源）三新键在场——方向/续传档/断点链
        let pv = serde_json::to_value(&p).unwrap();
        for k in ["direction", "resumable", "resumed_from"] {
            assert!(pv.get(k).is_some(), "OpProgress 事件面须带 {k}");
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-7）字面测试名优先于 rustc 命名惯例
    fn xferStatus_afterResume_pointsAtNewOpId() {
        // 承重③：resume 后**新** id 的状态可查且 resumed_from 指回旧 id——
        // 修前该测判红（旧形状只回裸 String，链在两行之间无从分辨）。
        // 夹具用手工 pending（与"暂停出口落盘"同一形状），零时序竞态。
        let src = tmpdir("xres_src");
        let dst = tmpdir("xres_dst");
        let store = tmpdir("xres_store");
        std::fs::write(src.join("f.bin"), vec![3u8; 64 * 1024]).unwrap();
        persist_pending(
            &store,
            &PendingOp {
                op_id: "A".into(),
                kind: OpKind::Copy,
                srcs: vec![L(src.join("f.bin"))],
                dst: L(dst.clone()),
                policy: ConflictPolicy::Overwrite,
                recycle: false,
                file_index: 0,
                bytes_done: 0,
                created_ms: 0,
                resumed_from: None,
            },
        )
        .unwrap();
        let (cb, _) = sink();
        let q = OpQueue::new(store.clone(), 1, cb).unwrap();
        let b = q.resume("A").unwrap();
        assert_ne!(b, "A");
        assert!(wait_until(
            || op_state(&q, &b) == OpState::Done,
            Duration::from_secs(10)
        ));
        let sb = q.status(&b).expect("新 id 状态必须可查");
        assert_eq!(sb.resumed_from.as_deref(), Some("A"), "断点链必须明写来源");
        let dto = XferStatusDto::from(&sb);
        assert_eq!(dto.op_id, b);
        assert_eq!(dto.resumed_from.as_deref(), Some("A"));
        // 正对照：直入队的新 op 没有链（不谎称"从谁续来"）
        std::fs::write(src.join("g.bin"), vec![4u8; 1024]).unwrap();
        let c = q
            .enqueue(OpSpec {
                kind: OpKind::Copy,
                srcs: vec![L(src.join("g.bin"))],
                dst: L(dst.clone()),
                policy: ConflictPolicy::Overwrite,
                recycle: false,
            })
            .unwrap();
        assert!(wait_until(
            || op_state(&q, &c) == OpState::Done,
            Duration::from_secs(10)
        ));
        assert_eq!(q.status(&c).unwrap().resumed_from, None);
        q.close();
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-7）字面测试名优先于 rustc 命名惯例
    fn xferStatus_resumableMatchesPeerClaim_notAssumed() {
        // "对端声明才承诺"：入队侧无任何对端事实源 ⇒ resumable 恒 None——
        // 本地复制不得被编出"断点续传"；有声明（Range）才如实带出。
        let store = tmpdir("claim_store");
        let src = tmpdir("claim_src");
        let dst = tmpdir("claim_dst");
        std::fs::write(src.join("s.bin"), b"x").unwrap();
        let (cb, _) = sink();
        let q = OpQueue::new(store.clone(), 1, cb).unwrap();
        let op = q
            .enqueue(OpSpec {
                kind: OpKind::Copy,
                srcs: vec![L(src.join("s.bin"))],
                dst: L(dst.clone()),
                policy: ConflictPolicy::Overwrite,
                recycle: false,
            })
            .unwrap();
        assert!(wait_until(
            || op_state(&q, &op) == OpState::Done,
            Duration::from_secs(10)
        ));
        let p = q.status(&op).unwrap();
        assert_eq!(p.direction, TransferDirection::Local);
        assert!(
            p.resumable.is_none(),
            "本地臂无对端声明，resumable 必须为 None（无事实源即 null）"
        );
        // 正对照：事实源在场（远端下载腿声明 Range）时逐字带出，不降级不夸大
        let mut claimed = p.clone();
        claimed.resumable = Some(crate::remote::Resumable::Range);
        let v = serde_json::to_value(&claimed).unwrap();
        assert_eq!(v["resumable"], serde_json::json!("range"));
        // Whole 档（只能整取）不得序列化出"range/append"字样——面板据此
        // 显示"断点续传"与否的判据源头在此
        let mut whole = claimed.clone();
        whole.resumable = Some(crate::remote::Resumable::Whole);
        let wv = serde_json::to_value(&whole).unwrap();
        assert_eq!(wv["resumable"], serde_json::json!("whole"));
        q.close();
        let _ = std::fs::remove_dir_all(&store);
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-7）字面测试名优先于 rustc 命名惯例
    fn askPolicy_conflictAfterPreScan_neverSilentlySkips() {
        // 承重⑩主证：队列口不做预扫描（预扫描在 service 门面），Ask 到达执行臂
        // 时目标已被占 ⇒ 真报错点名，该文件**不得**被当成"已完成"。
        let src = tmpdir("askfix_src");
        let dst = tmpdir("askfix_dst");
        let store = tmpdir("askfix_store");
        std::fs::write(src.join("a.txt"), b"new").unwrap();
        std::fs::write(dst.join("a.txt"), b"old").unwrap();
        let (cb, _) = sink();
        let q = OpQueue::new(store.clone(), 1, cb).unwrap();
        let op = q
            .enqueue(OpSpec {
                kind: OpKind::Copy,
                srcs: vec![L(src.join("a.txt"))],
                dst: L(dst.clone()),
                policy: ConflictPolicy::Ask,
                recycle: false,
            })
            .unwrap();
        assert!(wait_until(
            || op_state(&q, &op) == OpState::Failed,
            Duration::from_secs(10)
        ));
        let p = q.status(&op).unwrap();
        let err = p.error.clone().expect("Ask 冲突必须留下点名的 error");
        assert!(err.contains("a.txt"), "错误须点名冲突文件，实得 {err}");
        assert!(err.contains("未经决议"), "错误须自陈未决议，实得 {err}");
        assert_eq!(p.files_done, 0, "冲突文件不得被计入完成");
        assert_eq!(
            std::fs::read(dst.join("a.txt")).unwrap(),
            b"old",
            "旧内容不得被偷换"
        );
        // 正对照：Skip 档仍静默跳＝语义不变（防误伤既有测）
        let op2 = q
            .enqueue(OpSpec {
                kind: OpKind::Copy,
                srcs: vec![L(src.join("a.txt"))],
                dst: L(dst.clone()),
                policy: ConflictPolicy::Skip,
                recycle: false,
            })
            .unwrap();
        assert!(wait_until(
            || op_state(&q, &op2) == OpState::Done,
            Duration::from_secs(10)
        ));
        let p2 = q.status(&op2).unwrap();
        assert_eq!(
            (p2.files_done, p2.error.clone()),
            (1, None),
            "Skip 语义逐字不变"
        );
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"old");
        q.close();
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-7）字面测试名优先于 rustc 命名惯例
    fn askPolicy_deleteCompressExtract_reachTheSameGate() {
        // 三非 Copy/Move 类的 Ask 裁决现状按实收口（T-B6-6 前只 Copy/Move 有闸）：
        // Compress/Extract 的执行臂冲突走同一"未经决议即报错"闸；
        // Delete 无目标概念——免扫是**事实**，如实登记而非造一个冲突。
        let root = tmpdir("gate3");
        let store = tmpdir("gate3_store");
        std::fs::create_dir_all(root.join("in")).unwrap();
        std::fs::write(root.join("in/a.txt"), b"x").unwrap();
        let zip_path = root.join("out.zip");
        std::fs::write(&zip_path, b"existing-archive").unwrap();
        let (cb, _) = sink();
        let q = OpQueue::new(store.clone(), 1, cb).unwrap();
        // ① Compress：Ask + 已存在压缩包 ⇒ Failed 点名（修前 File::create 静默截断重写）
        let cmp_op = q
            .enqueue(OpSpec {
                kind: OpKind::Compress,
                srcs: vec![L(root.join("in"))],
                dst: L(zip_path.clone()),
                policy: ConflictPolicy::Ask,
                recycle: false,
            })
            .unwrap();
        assert!(
            wait_until(
                || op_state(&q, &cmp_op) == OpState::Failed,
                Duration::from_secs(10)
            ),
            "Ask+已有压缩包必须判红"
        );
        let err = q.status(&cmp_op).unwrap().error.unwrap();
        assert!(
            err.contains("out.zip") && err.contains("未经决议"),
            "须点名压缩包，实得 {err}"
        );
        assert_eq!(
            std::fs::read(&zip_path).unwrap(),
            b"existing-archive",
            "旧档不得被截断"
        );
        // ② Extract：Ask + 包内条目已在场 ⇒ Failed 点名（修前静默改名自陈已决议）
        let ext_dir = root.join("ext");
        std::fs::create_dir_all(&ext_dir).unwrap();
        {
            use std::io::Write;
            let zf = std::fs::File::create(root.join("real.zip")).unwrap();
            let mut zw = zip::ZipWriter::new(zf);
            let opts = SimpleFileOptions::default();
            zw.start_file("a.txt", opts).unwrap();
            zw.write_all(b"from-zip").unwrap();
            zw.finish().unwrap();
        }
        std::fs::write(ext_dir.join("a.txt"), b"on-disk").unwrap();
        let ext_op = q
            .enqueue(OpSpec {
                kind: OpKind::Extract,
                srcs: vec![L(root.join("real.zip"))],
                dst: L(ext_dir.clone()),
                policy: ConflictPolicy::Ask,
                recycle: false,
            })
            .unwrap();
        assert!(
            wait_until(
                || op_state(&q, &ext_op) == OpState::Failed,
                Duration::from_secs(10)
            ),
            "Ask+解压冲突必须判红（不得静默改名冒充决议）"
        );
        let err = q.status(&ext_op).unwrap().error.unwrap();
        assert!(
            err.contains("a.txt") && err.contains("未经决议"),
            "实得 {err}"
        );
        assert_eq!(std::fs::read(ext_dir.join("a.txt")).unwrap(), b"on-disk");
        // ③ Delete：Ask 无目标冲突概念——现状如实登记，正常执行（免扫是事实非漏洞）
        std::fs::write(root.join("gone.txt"), b"x").unwrap();
        let del_op = q
            .enqueue(OpSpec {
                kind: OpKind::Delete,
                srcs: vec![L(root.join("gone.txt"))],
                dst: L(root.clone()),
                policy: ConflictPolicy::Ask,
                recycle: false,
            })
            .unwrap();
        assert!(wait_until(
            || op_state(&q, &del_op) == OpState::Done,
            Duration::from_secs(10)
        ));
        assert!(!root.join("gone.txt").exists());
        q.close();
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-7）字面测试名优先于 rustc 命名惯例
    fn resumeDto_reopenService_preservesChain() {
        // 承重⑤ 的传输面镜像：链与断点都在 pending 文件里，重开服务读回后
        // 再 resume 链只续不断；**旧 pending 文件（无 resumed_from 键）零迁移**。
        let store = tmpdir("chain_store");
        let legacy = store.join("L.json");
        std::fs::write(
            &legacy,
            r#"{"op_id":"L","kind":"copy","srcs":["C:\\nf\\legacy 中文\\a.txt"],"dst":"D:\\b","policy":"overwrite","recycle":false,"file_index":2,"bytes_done":4096,"created_ms":7}"#,
        )
        .unwrap();
        let (cb, _) = sink();
        let q = OpQueue::new(store.clone(), 1, cb).unwrap();
        let ls = q.pending().into_iter().find(|p| p.op_id == "L").unwrap();
        assert_eq!(ls.resumed_from, None, "旧盘无键 ⇒ None（加键向后兼容）");
        assert_eq!(
            (ls.file_index, ls.bytes_done),
            (2, 4096),
            "断点真值逐字保真"
        );
        // 新形状（resume 产物）重开可读且链在场：手工落一枚 B（模拟崩溃前由 A 续来）
        persist_pending(
            &store,
            &PendingOp {
                op_id: "B".into(),
                kind: OpKind::Copy,
                srcs: vec![],
                dst: L(std::env::temp_dir()),
                policy: ConflictPolicy::Overwrite,
                recycle: false,
                file_index: 1,
                bytes_done: 512,
                created_ms: 0,
                resumed_from: Some("A".into()),
            },
        )
        .unwrap();
        let b = q.pending().into_iter().find(|p| p.op_id == "B").unwrap();
        assert_eq!(b.resumed_from.as_deref(), Some("A"), "重开后链必须原样读回");
        q.close();
        let _ = std::fs::remove_dir_all(&store);
    }
}
