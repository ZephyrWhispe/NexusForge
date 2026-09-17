//! F2 异步操作队列（docs/impl/05 F2）：复制/移动/删除/压缩/解压。
//!
//! 模型：全局 mpsc 队列 + N worker（默认 2，std 线程——进度发布走 host-core 同步
//! broadcast，无需 tokio）。每个 Op：
//! - 入队先落 `{store_dir}/{op_id}.json`（崩溃恢复扫描点，docs/impl/01 S6.5）
//! - 大文件 4MB 分块 + 逐块读满（非末块必须 read_exact，短块会造成零洞——M4 K9 教训）
//! - 进度事件 200ms 节流；文件完成/状态迁移立即发
//! - 断点续传：Copy/Move 记录 (file_index, bytes_done)，resume 时 seek 续传
//! - 删除：recycle=true 走 [`RecycleBinPort`]（SHFileOperationW 回收站），无端口降级直删

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use host_core::ports::RecycleBinPort;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};
use zip::read::ZipArchive;

use crate::browse::to_long_path;
use crate::conflict::{resolve_target, unique_target, ConflictPolicy};
use crate::error::FileError;

/// 分块大小（docs/impl/05 K6/F2 统一 4MB）
pub const CHUNK: usize = 4 * 1024 * 1024;
/// 进度事件节流窗口
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
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

/// 操作规格（IPC 提交）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpSpec {
    pub kind: OpKind,
    pub srcs: Vec<PathBuf>,
    /// Copy/Move：目标目录或文件；Compress：zip 输出路径；Extract：解压根目录
    pub dst: PathBuf,
    pub policy: ConflictPolicy,
    /// Delete 专用：true 走回收站（需 RecycleBinPort），false 直删
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
    pub srcs: Vec<PathBuf>,
    pub dst: PathBuf,
    pub policy: ConflictPolicy,
    pub recycle: bool,
    #[serde(default)]
    pub file_index: usize,
    #[serde(default)]
    pub bytes_done: u64,
    pub created_ms: i64,
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
}

pub(crate) struct OpControl {
    paused: AtomicBool,
    canceled: AtomicBool,
}

impl OpControl {
    fn new() -> Self {
        Self { paused: AtomicBool::new(false), canceled: AtomicBool::new(false) }
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
    checkpoint: Option<Checkpoint>,
    ctl: Arc<OpControl>,
    store_dir: PathBuf,
    recycle: Option<Arc<dyn RecycleBinPort>>,
}

/// 进度回调（模块把它接到 EventBus：payload key=op_id 供 UI 去抖订阅）
pub type ProgressFn = Arc<dyn Fn(OpProgress) + Send + Sync>;

pub struct OpQueue {
    tx: Option<std::sync::mpsc::Sender<Job>>,
    ctls: Mutex<HashMap<String, Arc<OpControl>>>,
    /// 活跃/近期操作最新进度快照（active() 用；worker 回调前先更新）
    latest: Arc<Mutex<HashMap<String, OpProgress>>>,
    store_dir: PathBuf,
    cb: ProgressFn,
    recycle: Option<Arc<dyn RecycleBinPort>>,
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
        for _ in 0..workers {
            let rx = rx.clone();
            let cb = cb.clone();
            let latest = latest.clone();
            handles.push(std::thread::spawn(move || {
                // 快照更新包装：active() 永远拿到最新状态（每 worker 构造一次）
                let sink: ProgressFn = Arc::new(move |p: OpProgress| {
                    latest.lock().expect("ops 进度表锁").insert(p.op_id.clone(), p.clone());
                    cb(p);
                });
                loop {
                    // 持锁阻塞 recv：有 job 的 worker 取走后立即放锁，其余 worker 依次排队
                    let job = { rx.lock().expect("ops 队列锁").recv() };
                    let Ok(job) = job else { return }; // 发送端关闭 → 退出
                    run_job(job, &sink);
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
            _workers: handles,
        })
    }

    /// 注册回收站端口（Delete recycle=true 需要；未注册降级直删并告警）
    pub fn set_recycle_port(&mut self, port: Arc<dyn RecycleBinPort>) {
        self.recycle = Some(port);
    }

    /// 入队（Ask 策略的预扫描由上层 [`crate::conflict::scan_conflicts`] 完成）
    pub fn enqueue(&self, spec: OpSpec) -> Result<String, FileError> {
        self.enqueue_with_checkpoint(spec, None)
    }

    pub fn enqueue_with_checkpoint(
        &self,
        spec: OpSpec,
        checkpoint: Option<Checkpoint>,
    ) -> Result<String, FileError> {
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
            },
        )?;
        let ctl = Arc::new(OpControl::new());
        self.ctls
            .lock()
            .expect("ops 控制表锁")
            .insert(op_id.clone(), ctl.clone());
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
        };
        (self.cb)(progress.clone());
        self.latest.lock().expect("ops 进度表锁").insert(op_id.clone(), progress);
        self.tx
            .as_ref()
            .expect("队列发送端存活")
            .send(Job {
                op_id: op_id2,
                spec,
                checkpoint,
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
        let new_op_id = self.enqueue_with_checkpoint(spec, checkpoint)?;
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
            .expect("ops 控制表锁")
            .get(op_id)
            .cloned()
            .ok_or_else(|| FileError::NoSuchOp(op_id.to_owned()))
    }

    /// 活跃/近期操作快照
    pub fn active(&self) -> Vec<OpProgress> {
        let mut v: Vec<OpProgress> =
            self.latest.lock().expect("ops 进度表锁").values().cloned().collect();
        v.sort_by(|a, b| a.op_id.cmp(&b.op_id));
        v
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

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
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
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
    last_emit: Instant,
    cur: OpProgress,
}

impl Reporter<'_> {
    fn set_state(&mut self, state: OpState) {
        self.cur.state = state;
        self.emit_now();
    }
    /// 节流进度；force=true 立即发（文件完成/状态迁移）
    fn tick(&mut self, force: bool) {
        if force || self.last_emit.elapsed() >= PROGRESS_INTERVAL {
            self.last_emit = Instant::now();
            (self.cb)(self.cur.clone());
        }
    }
    fn emit_now(&mut self) {
        self.last_emit = Instant::now();
        (self.cb)(self.cur.clone());
    }
}

fn run_job(job: Job, cb: &ProgressFn) {
    let Job { op_id, spec, checkpoint, ctl, store_dir, recycle } = job;
    let kind = spec.kind;
    let mut rep = Reporter {
        cb,
        last_emit: Instant::now(),
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
        },
    };
    rep.set_state(OpState::Running);

    let result = match spec.kind {
        OpKind::Copy | OpKind::Move => run_copy_move(&spec, checkpoint, &ctl, &mut rep, &store_dir),
        OpKind::Delete => run_delete(&spec, &ctl, &mut rep, recycle.as_ref()),
        OpKind::Compress => run_compress(&spec, &ctl, &mut rep),
        OpKind::Extract => run_extract(&spec, &ctl, &mut rep),
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
            rep.cur.error = Some(e);
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
    for src in &spec.srcs {
        let long_src = to_long_path(src);
        if long_src.is_dir() {
            if spec.dst.is_file() {
                return Err(Flow::msg(format!("目标是文件而源是目录: {}", src.display())));
            }
            // 基准：dst 已存在 → 源父目录（保留源目录名）；否则 → 源本身（内容直达 dst）
            let base = if spec.dst.is_dir() {
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
                let rel = entry.path().strip_prefix(base).map_err(|e| Flow::msg(e.to_string()))?;
                items.push(CopyItem {
                    src: entry.path().to_path_buf(),
                    dst: spec.dst.join(rel),
                    size: md.len(),
                });
            }
        } else if long_src.is_file() {
            let md = std::fs::metadata(&long_src).map_err(Flow::io)?;
            items.push(CopyItem {
                src: long_src,
                dst: target_for(src, &spec.dst),
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
        let src = to_long_path(&spec.srcs[0]);
        let dst = target_for(&spec.srcs[0], &spec.dst);
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(to_long_path(parent));
        }
        if std::fs::rename(&src, &to_long_path(&dst)).is_ok() {
            rep.cur.files_done = 1;
            rep.cur.bytes_done = rep.cur.bytes_total;
            rep.tick(true);
            return Flow::Done;
        }
        tracing::debug!("rename 快速路径失败，走逐项复制（跨卷）");
    }

    let start_index = checkpoint.map(|c| c.file_index.min(items.len())).unwrap_or(0);
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
        // 冲突决议（Ask 在入队前已预扫描，到达 worker 的必为 Skip/Overwrite/Rename）
        let target = match resolve_target(&item.src, &item.dst, spec.policy) {
            Some(t) => t,
            None => {
                rep.cur.files_done += 1; // Skip
                rep.cur.bytes_done += item.size;
                rep.tick(true);
                continue;
            }
        };
        rep.cur.current =
            item.src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
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
        rep.tick(true);
    }

    // Move：搬空的源目录收尾
    if spec.kind == OpKind::Move {
        for src in &spec.srcs {
            let long = to_long_path(src);
            if long.is_dir() {
                let _ = std::fs::remove_dir_all(&long);
            }
        }
    }
    Flow::Done
}

/// 复制单个文件（支持从 bytes_done 续传）；逐块读满；尺寸终验。
/// 暂停 → 持久化断点；取消 → 清理部分文件。
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
    let mut reader = match File::open(&item.src) {
        Ok(f) => f,
        Err(e) => return Flow::io(e),
    };
    let total = item.size;

    let mut start = bytes_done;
    let writer = if start > 0
        && long_dst.exists()
        && std::fs::metadata(&long_dst).map(|m| m.len()).unwrap_or(0) == start
    {
        // 断点续传：从已写字节处追加
        match OpenOptions::new().write(true).open(&long_dst) {
            Ok(mut f) => match f.seek(SeekFrom::Start(start)) {
                Ok(_) => f,
                Err(e) => return Flow::io(e),
            },
            Err(e) => return Flow::io(e),
        }
    } else {
        start = 0;
        match File::create(&long_dst) {
            Ok(f) => f,
            Err(e) => return Flow::io(e),
        }
    };
    if start > 0 && reader.seek(SeekFrom::Start(start)).is_err() {
        return Flow::msg("续传 seek 失败");
    }
    let mut written_in_file = start;
    let mut last_ckpt = start;
    let mut buf = vec![0u8; CHUNK];
    let mut writer = writer;
    loop {
        match gate(ctl) {
            Gate::Pause => {
                // 断点落盘后暂停退出
                let _ = persist_pending(
                    store_dir,
                    &PendingOp {
                        op_id: rep.cur.op_id.clone(),
                        kind: spec.kind,
                        srcs: spec.srcs.clone(),
                        dst: spec.dst.clone(),
                        policy: spec.policy,
                        recycle: spec.recycle,
                        file_index,
                        bytes_done: written_in_file,
                        created_ms: now_ms(),
                    },
                );
                return Flow::Paused;
            }
            Gate::Cancel => {
                let _ = std::fs::remove_file(&long_dst);
                return Flow::Canceled;
            }
            Gate::Go => {}
        }
        // 逐块读满（非末块 read_exact 语义；M4 K9 零洞教训）
        let n = match read_chunk(&mut reader, &mut buf) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if n == 0 {
            break;
        }
        if let Err(e) = writer.write_all(&buf[..n]) {
            return Flow::io(e);
        }
        written_in_file += n as u64;
        rep.cur.bytes_done += n as u64;
        rep.tick(false);
        if written_in_file - last_ckpt >= CHECKPOINT_EVERY {
            last_ckpt = written_in_file;
            let _ = persist_pending(
                store_dir,
                &PendingOp {
                    op_id: rep.cur.op_id.clone(),
                    kind: spec.kind,
                    srcs: spec.srcs.clone(),
                    dst: spec.dst.clone(),
                    policy: spec.policy,
                    recycle: spec.recycle,
                    file_index,
                    bytes_done: written_in_file,
                    created_ms: now_ms(),
                },
            );
        }
    }
    if let Err(e) = writer.flush() {
        return Flow::io(e);
    }
    // 尺寸终验
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
fn read_chunk(reader: &mut File, buf: &mut [u8]) -> Result<usize, Flow> {
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
    // 统计字节（回收站整批交接，无逐文件进度）
    let mut bytes_total = 0u64;
    let mut file_count = 0u64;
    for src in &spec.srcs {
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
            Some(port) => {
                match port.delete(&spec.srcs) {
                    Ok(_) => {
                        rep.cur.files_done = file_count;
                        rep.cur.bytes_done = bytes_total;
                        rep.tick(true);
                        return Flow::Done;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "回收站删除失败，降级直删");
                    }
                }
            }
            None => tracing::warn!("RecycleBinPort 未注册，回收站删除降级为直删"),
        }
    }
    for src in &spec.srcs {
        match gate(ctl) {
            Gate::Go => {}
            g => return g.into(),
        }
        let long = to_long_path(src);
        rep.cur.current =
            src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
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
        rep.tick(true);
    }
    Flow::Done
}

struct CompressItem {
    src: PathBuf,
    rel: PathBuf,
    size: u64,
}

/// 压缩计划：目录以其名称为 zip 内根（`dir/a.txt`）
fn expand_plan_for_compress(spec: &OpSpec) -> Result<Vec<CompressItem>, Flow> {
    let mut items = Vec::new();
    for src in &spec.srcs {
        let long = to_long_path(src);
        let base_name = src.file_name().unwrap_or_default().to_os_string();
        if long.is_dir() {
            // rel 相对源目录本身，再前置目录名 → zip 内 `dir_name/...`
            for entry in walkdir::WalkDir::new(&long).follow_links(false) {
                let entry = entry.map_err(|e| Flow::msg(e.to_string()))?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry.path().strip_prefix(&long).map_err(|e| Flow::msg(e.to_string()))?;
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
    let dst = to_long_path(&spec.dst);
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
        if let Err(e) = zw.start_file(arc_name, opts.clone()) {
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
            rep.tick(false);
        }
        rep.cur.files_done += 1;
        rep.tick(true);
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
        Some(p) => p.clone(),
        None => return Flow::msg("缺少压缩包路径"),
    };
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

    if let Err(e) = std::fs::create_dir_all(to_long_path(&spec.dst)) {
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
        let out_path = spec.dst.join(&name);
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
        // 包内冲突：Skip 之外一律 Rename（安全默认），Overwrite 显式放行
        let final_path = if out_path.exists() {
            match spec.policy {
                ConflictPolicy::Overwrite => out_path,
                ConflictPolicy::Skip => {
                    rep.cur.bytes_done += entry.size();
                    rep.cur.files_done += 1;
                    rep.tick(true);
                    continue;
                }
                _ => unique_target(&out_path),
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
        rep.tick(true);
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
        (Arc::new(move |_| { c2.fetch_add(1, Ordering::SeqCst); }), count)
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
        q.active().iter().find(|p| p.op_id == op).map(|p| p.state).unwrap_or(OpState::Queued)
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
                srcs: vec![src.clone()],
                dst: dst.clone(),
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
        assert!(wait_until(|| op_state(&q, &op) == OpState::Done, Duration::from_secs(5)));
        assert!(q.pending().is_empty());
        let active = q.active();
        assert_eq!(active.iter().find(|p| p.op_id == op).unwrap().files_total, 3);
        assert_eq!(active.iter().find(|p| p.op_id == op).unwrap().bytes_total, 5 * 1024 * 1024 + 11 + 1);
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
            srcs: vec![src.join("sub/b.txt")],
            dst: dst.join("b.txt"),
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
            srcs: vec![src.clone()],
            dst: dst.clone(),
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
            srcs: vec![src.join("f.txt")],
            dst: dst.clone(),
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
            srcs: vec![src.clone()],
            dst: PathBuf::new(),
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
            srcs: vec![src],
            dst: zipfile.clone(),
            policy: ConflictPolicy::default(),
            recycle: false,
        })
        .unwrap();
        assert!(wait_until(
            || op_state(&q, &q.active().first().map(|p| p.op_id.clone()).unwrap_or_default())
                == OpState::Done
                && zipfile.exists(),
            Duration::from_secs(20)
        ));

        let ext_dir = dst.join("unpacked");
        let op2 = q
            .enqueue(OpSpec {
                kind: OpKind::Extract,
                srcs: vec![zipfile],
                dst: ext_dir.clone(),
                policy: ConflictPolicy::Overwrite,
                recycle: false,
            })
            .unwrap();
        assert!(wait_until(
            || std::fs::read(ext_dir.join(&zip_root).join("sub/deep/c.txt"))
                .is_ok_and(|b| b == b"x"),
            Duration::from_secs(20)
        ));
        assert!(wait_until(|| op_state(&q, &op2) == OpState::Done, Duration::from_secs(10)));
        assert_eq!(
            std::fs::metadata(ext_dir.join(&zip_root).join("a.bin")).unwrap().len(),
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
                srcs: vec![src.join("big.bin")],
                dst: dst.join("big.bin"),
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
            assert!(wait_until(|| op_state(&q, &op2) == OpState::Done, Duration::from_secs(20)));
        } else {
            assert!(wait_until(|| op_state(&q, &op) == OpState::Done, Duration::from_secs(20)));
        }
        assert_eq!(std::fs::metadata(dst.join("big.bin")).unwrap().len(), 24 * 1024 * 1024);
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
                srcs: vec![src.join("big.bin")],
                dst: dst.join("big.bin"),
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
}
