//! FileService（F1–F7 门面）：IPC 层唯一入口；阻塞 IO 由调用方 spawn_blocking。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use host_core::events::{merged_window, Event, EventBus};
use host_core::ports::{Ports, RecycleBinPort, ThumbPort, UsnIndexPort};

use crate::browse::{self, DriveInfo, FileEntry, SortKey};
use crate::conflict::{scan_conflicts, ConflictItem, ConflictPolicy};
use crate::driver::{DriverInfo, DriverRegistry};
use crate::error::FileError;
use crate::ops::{OpProgress, OpQueue, OpSpec, PendingOp};
use crate::preview::{preview_file, Preview};
use crate::rename::{apply_plan, build_plan, RenamePlan, RenameRule};
use crate::search::{self, SearchOpts, SearchResult};

pub struct FileService {
    queue: OpQueue,
    drivers: Arc<DriverRegistry>,
    ports: Arc<Ports>,
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
            let dst_dir = if spec.dst.is_dir() {
                spec.dst.clone()
            } else {
                spec.dst
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."))
            };
            let conflicts = scan_conflicts(&spec.srcs, &dst_dir);
            if !conflicts.is_empty() && spec.policy == ConflictPolicy::Ask {
                return Ok((None, conflicts));
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
    use crate::ops::OpKind;
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
                srcs: vec![src.join("big.bin")],
                dst: dst.clone(),
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
}
