//! T-B7-10（09 §7.2）sys 进程页红线批：进程行两拍差值 + 保护名单 + 结束进程闸。
//!
//! - [`ProcessRow`]：`disk_bps` 为 `Option`——**无事实源即 None**（IO 计数首轮无
//!   差值 / 跨权限读不到时不编 0，同 PerfPort io 语义）。
//! - CPU% 两拍差值：本模块持上一拍快照（[`Beat`]），[`PROCESS_SAMPLE_GAP_MS`]
//!   为两拍最小间隔——不足一拍的重入复用上拍结果（不产生除零假峰）。
//! - 保护名单是**数据文件** `{appData}/sys/process_guard.json`（允许扩）：
//!   坏文件 fail-closed——整份拒载且**拒绝任何 kill**（而非放空名单）。
//!   与 T-B7-14 历史环（可观测数据=warn 留证空载）同谱相反、与 T-B7-1
//!   known_hosts（信任决定=拒启）同向：保护名单是安全决定，坏盘宁锁不误放。
//! - [`ProcessTable::kill`] 闸序：坏盘总拒 → 保护名单点名拒 → 复述名不符拒
//!   （输入确认词范式，B6 FILE_REMOTE_008/镜像档同谱）→ 端口执行。
//!   成功与被拒均落审计 `{appData}/sys/process_audit.jsonl`
//!   （复用 winops AuditStore 通道形状：JSONL 只追加，写失败仅 warn）。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use host_core::ports::{ProcPort, ProcSnap};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::error::{Result, SysError};

/// 两拍最小采样间隔（毫秒）：不足一拍的重入直接复用上拍差值结果
pub const PROCESS_SAMPLE_GAP_MS: i64 = 500;

/// 内置保护名单（数据文件只能**扩**不能削）。
/// svchost 不在名单——登记裁决：svchost 属常规可重启服务宿主，用户面可杀
/// （且杀单个 svchost 进程不等于杀服务，SCM 会按需拉起宿主）；放进名单会
/// 让用户面最常见的合法目标被一刀切禁掉，故裁决为否。
const DEFAULT_PROTECTED: &[&str] = &["system", "csrss.exe", "wininit.exe", "services.exe"];

/// 进程行（IPC DTO）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessRow {
    pub pid: u32,
    pub name: String,
    /// 两拍差值 CPU 占用（0-100；多核进程可超 100，不做归一）
    pub cpu_pct: f32,
    pub mem_bytes: u64,
    /// 磁盘吞吐（字节/秒）；**无事实源即 None**（首轮无差值/跨权限读不到）
    pub disk_bps: Option<u64>,
}

/// 单条 kill 审计（JSONL 行；形状沿 winops AuditStore，只追加不修改）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KillAudit {
    pub ts_ms: i64,
    pub pid: u32,
    /// 实际进程名（读不到时为 "?"）
    pub name: String,
    /// 调用方复述词
    pub confirm_name: String,
    /// killed | refused
    pub outcome: String,
    pub detail: String,
}

/// 保护名单盘态
enum GuardState {
    /// 数据文件在册的扩展条目（文件不存在 = 空扩展，正常态）
    Extended(Vec<String>),
    /// 坏盘 fail-closed：整份拒载，**拒绝任何 kill**
    Corrupted,
}

/// 一拍快照：进程计数 + 本拍算出的行（不足一拍的重入直接复用 rows）
struct Beat {
    at_ms: i64,
    procs: HashMap<u32, ProcSnap>,
    rows: Vec<ProcessRow>,
}

/// 进程表：端口 + 上一拍快照 + 保护名单 + kill 审计
pub struct ProcessTable {
    port: Option<Arc<dyn ProcPort>>,
    prev: Mutex<Option<Beat>>,
    guard: GuardState,
    audit_path: PathBuf,
}

/// 内置保护判定（pid 0/4 + 缺省名册，大小写不敏感）
pub fn is_protected(name: &str, pid: u32) -> bool {
    pid == 0
        || pid == 4
        || DEFAULT_PROTECTED
            .iter()
            .any(|p| p.eq_ignore_ascii_case(name))
}

impl ProcessTable {
    /// 打开 `{appData}/sys/process_guard.json`（不存在=空扩展；坏文件=Corrupted）
    pub fn open(app_data_dir: &Path, port: Option<Arc<dyn ProcPort>>) -> Self {
        let guard_path = app_data_dir.join("sys").join("process_guard.json");
        #[derive(Deserialize)]
        struct GuardFile {
            #[serde(default)]
            protected: Vec<String>,
        }
        let guard = match std::fs::read(&guard_path) {
            Ok(bytes) => match serde_json::from_slice::<GuardFile>(&bytes) {
                Ok(g) => GuardState::Extended(g.protected),
                Err(e) => {
                    tracing::warn!(
                        path = %guard_path.display(),
                        error = %e,
                        "process_guard.json 不可解析——fail-closed 拒绝一切结束进程（保护名单=安全决定，与历史环可观测数据裁决相反）"
                    );
                    GuardState::Corrupted
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => GuardState::Extended(Vec::new()),
            Err(e) => {
                tracing::warn!(
                    path = %guard_path.display(),
                    error = %e,
                    "process_guard.json 读取失败——同坏盘 fail-closed 处理"
                );
                GuardState::Corrupted
            }
        };
        Self {
            port,
            prev: Mutex::new(None),
            guard,
            audit_path: app_data_dir.join("sys").join("process_audit.jsonl"),
        }
    }

    fn extra_protected(&self, name: &str) -> bool {
        match &self.guard {
            GuardState::Extended(list) => list.iter().any(|p| p.eq_ignore_ascii_case(name)),
            GuardState::Corrupted => false, // 坏盘走 kill 总拒，不在此判定
        }
    }

    /// 进程 Top-N：两拍差值 → query 过滤（大小写不敏感）→ sort（name 升序，其余降序）→ 截 n
    pub fn top_n(&self, sort: &str, n: usize, query: &str, now_ms: i64) -> Vec<ProcessRow> {
        let Some(port) = &self.port else {
            return Vec::new();
        };
        let snaps = match port.snapshot() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "进程快照失败（本轮返回空表）");
                return Vec::new();
            }
        };
        let mut prev = self.prev.lock();
        let rows: Vec<ProcessRow> = match prev.as_ref() {
            // 不足一拍：复用上拍行（不新算差值——同拍重算只会出除零假峰）
            Some(b) if now_ms - b.at_ms < PROCESS_SAMPLE_GAP_MS => b.rows.clone(),
            _ => {
                let elapsed = prev.as_ref().map(|b| (now_ms - b.at_ms).max(1) as f64);
                let mut rows = Vec::with_capacity(snaps.len());
                for s in &snaps {
                    let old = prev.as_ref().and_then(|b| b.procs.get(&s.pid));
                    let (cpu_pct, disk_bps) = match (elapsed, old) {
                        (Some(el), Some(o)) => {
                            let dcpu = s.cpu_ms.saturating_sub(o.cpu_ms) as f64 / el * 100.0;
                            let dio = match (o.io_bytes, s.io_bytes) {
                                (Some(a), Some(b)) if b >= a => {
                                    Some(((b - a) as f64 / (el / 1000.0)).round() as u64)
                                }
                                _ => None,
                            };
                            (dcpu as f32, dio)
                        }
                        // 首轮/新进程：无差值事实 → cpu 基线 0、disk_bps=None 不编 0
                        _ => (0.0, None),
                    };
                    rows.push(ProcessRow {
                        pid: s.pid,
                        name: s.name.clone(),
                        cpu_pct,
                        mem_bytes: s.mem_bytes,
                        disk_bps,
                    });
                }
                *prev = Some(Beat {
                    at_ms: now_ms,
                    procs: snaps.into_iter().map(|s| (s.pid, s)).collect(),
                    rows: rows.clone(),
                });
                rows
            }
        };
        drop(prev);
        let q = query.trim().to_lowercase();
        let mut rows: Vec<ProcessRow> = rows
            .into_iter()
            .filter(|r| q.is_empty() || r.name.to_lowercase().contains(&q))
            .collect();
        rows.sort_by(|a, b| match sort {
            "mem" => b.mem_bytes.cmp(&a.mem_bytes),
            "disk" => b.disk_bps.unwrap_or(0).cmp(&a.disk_bps.unwrap_or(0)),
            "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            _ => b.cpu_pct.total_cmp(&a.cpu_pct),
        });
        rows.truncate(n);
        rows
    }

    /// 结束进程（红线闸序见模块头）；成功返回实际进程名
    pub fn kill(&self, pid: u32, confirm_name: &str) -> Result<String> {
        let now_ms = host_core::util::now_ms();
        let Some(port) = &self.port else {
            return Err(SysError::ProcKill("进程端口未注册".into()));
        };
        if matches!(self.guard, GuardState::Corrupted) {
            self.audit(
                now_ms,
                pid,
                "?",
                confirm_name,
                "refused",
                "process_guard.json 坏盘：拒绝结束任何进程",
            );
            return Err(SysError::ProcKill(
                "保护名单文件损坏——拒绝结束任何进程（fail-closed：名单坏=谁也不能杀）".into(),
            ));
        }
        let name = port.name_of(pid).map_err(|e| {
            self.audit(
                now_ms,
                pid,
                "?",
                confirm_name,
                "refused",
                &format!("进程不可见: {e}"),
            );
            SysError::ProcKill(format!("进程 {pid} 不存在或不可读名，拒绝结束"))
        })?;
        if is_protected(&name, pid) || self.extra_protected(&name) {
            let reason = if pid == 0 || pid == 4 {
                format!("pid {pid}（{name}）为内核系统进程")
            } else {
                format!("「{name}」在册保护名单（内置/数据文件扩展）")
            };
            self.audit(now_ms, pid, &name, confirm_name, "refused", &reason);
            return Err(SysError::ProcKill(format!(
                "拒绝结束受保护进程：{reason}。名单见 sys/process_guard.json，系统关键进程不可经本应用结束。"
            )));
        }
        if !name.eq_ignore_ascii_case(confirm_name) {
            let detail = format!("复述名「{confirm_name}」≠ 实际进程名「{name}」");
            self.audit(now_ms, pid, &name, confirm_name, "refused", &detail);
            return Err(SysError::ProcKill(format!(
                "{detail}——拒杀（结束进程=输入确认词范式，须逐字复述目标）"
            )));
        }
        match port.kill(pid) {
            Ok(()) => {
                self.audit(now_ms, pid, &name, confirm_name, "killed", "端口执行成功");
                Ok(name)
            }
            Err(e) => {
                self.audit(
                    now_ms,
                    pid,
                    &name,
                    confirm_name,
                    "refused",
                    &format!("端口拒绝/失败: {e}"),
                );
                Err(SysError::ProcKill(format!("结束 {name}({pid}) 失败：{e}")))
            }
        }
    }

    /// 审计追加（写失败仅 warn——审计不比主流程更重要，沿 winops AuditStore 裁决）
    fn audit(&self, ts_ms: i64, pid: u32, name: &str, confirm: &str, outcome: &str, detail: &str) {
        let entry = KillAudit {
            ts_ms,
            pid,
            name: name.to_string(),
            confirm_name: confirm.to_string(),
            outcome: outcome.to_string(),
            detail: detail.to_string(),
        };
        if let Some(parent) = self.audit_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let line = match serde_json::to_string(&entry) {
            Ok(l) => l,
            Err(_) => return,
        };
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.audit_path)
        {
            Ok(mut f) => {
                if let Err(e) = f
                    .write_all(line.as_bytes())
                    .and_then(|_| f.write_all(b"\n"))
                {
                    tracing::warn!(error = %e, "进程 kill 审计写入失败（不阻断主流程）");
                }
            }
            Err(e) => tracing::warn!(error = %e, "进程 kill 审计文件打开失败（不阻断主流程）"),
        }
    }

    /// 审计全量条目（展示/导出用）
    pub fn audit_entries(&self) -> Vec<KillAudit> {
        std::fs::read_to_string(&self.audit_path)
            .map(|raw| {
                raw.lines()
                    .filter_map(|l| serde_json::from_str::<KillAudit>(l).ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::error::AppError;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// 假进程端口：当前拍快照 + 名字表 + kill 记录（测试显式 set_current 驱动两拍）
    struct FakePort {
        current: Mutex<Vec<ProcSnap>>,
        names: Mutex<HashMap<u32, String>>,
        killed: Mutex<Vec<u32>>,
        fail_kill: AtomicBool,
    }
    impl FakePort {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                current: Mutex::new(vec![]),
                names: Mutex::new(HashMap::new()),
                killed: Mutex::new(vec![]),
                fail_kill: AtomicBool::new(false),
            })
        }
        fn set_names(&self, map: Vec<(u32, &str)>) {
            *self.names.lock() = map.into_iter().map(|(p, n)| (p, n.to_string())).collect();
        }
        fn set_current(&self, snaps: Vec<ProcSnap>) {
            *self.current.lock() = snaps;
        }
    }
    impl ProcPort for FakePort {
        fn snapshot(&self) -> std::result::Result<Vec<ProcSnap>, AppError> {
            Ok(self.current.lock().clone())
        }
        fn name_of(&self, pid: u32) -> std::result::Result<String, AppError> {
            self.names
                .lock()
                .get(&pid)
                .cloned()
                .ok_or_else(|| AppError::module("SYS_TEST_001", "进程不存在", None))
        }
        fn kill(&self, pid: u32) -> std::result::Result<(), AppError> {
            if self.fail_kill.load(Ordering::SeqCst) {
                return Err(AppError::module("SYS_TEST_002", "访问被拒绝", None));
            }
            self.killed.lock().push(pid);
            Ok(())
        }
    }

    fn snap(pid: u32, name: &str, cpu_ms: u64, mem: u64, io: Option<u64>) -> ProcSnap {
        ProcSnap {
            pid,
            name: name.into(),
            cpu_ms,
            mem_bytes: mem,
            io_bytes: io,
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("nf-proc-{tag}-{}", std::process::id()))
    }

    fn table(port: Arc<FakePort>, dir: &Path) -> ProcessTable {
        ProcessTable::open(dir, Some(port))
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-10）字面测试名优先于 rustc 命名惯例
    fn processTopN_sortedAndQueryCaseInsensitive() {
        let dir = tmp("topn");
        let p = FakePort::new();
        let t = table(p.clone(), &dir);
        // 第一拍：无差值事实，全 0 基线（排序无峰，只有名称/内存是事实）
        p.set_current(vec![
            snap(11, "Notepad.exe", 100, 10, Some(0)),
            snap(12, "chrome.exe", 300, 99, Some(0)),
            snap(13, "Code.exe", 200, 50, Some(0)),
        ]);
        let first = t.top_n("cpu", 3, "", 0);
        assert_eq!(first.len(), 3);
        assert!(first.iter().all(|r| r.cpu_pct == 0.0));
        // 第二拍 +500ms：Δcpu = [50, 100, 200] → [10%, 20%, 40%]
        p.set_current(vec![
            snap(11, "Notepad.exe", 150, 10, Some(0)),
            snap(12, "chrome.exe", 400, 99, Some(0)),
            snap(13, "Code.exe", 400, 50, Some(0)),
        ]);
        let rows = t.top_n("cpu", 2, "", 500);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Code.exe", "Δcpu 200ms/500ms=40% 居首");
        assert!((rows[0].cpu_pct - 40.0).abs() < 0.01);
        assert_eq!(rows[1].name, "chrome.exe");
        // query 大小写不敏感命中过滤
        let hit = t.top_n("cpu", 10, "NOTEPAD", 1100);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].name, "Notepad.exe");
        // name 升序臂：大小写归一（chrome < Code < Notepad）
        let byname = t.top_n("name", 10, "", 1700);
        assert_eq!(
            byname.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["chrome.exe", "Code.exe", "Notepad.exe"],
            "name 序大小写归一"
        );
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-10）字面测试名优先于 rustc 命名惯例
    fn kill_protectedPid4_refusesNamingReason() {
        let dir = tmp("guard4");
        let p = FakePort::new();
        p.set_names(vec![(4, "System"), (11, "notepad.exe")]);
        let t = table(p.clone(), &dir);
        // 红线：pid 4 即便复述名全对也拒杀，且拒因点名
        let e = t.kill(4, "System").unwrap_err();
        assert!(
            e.to_string().contains("受保护") && e.to_string().contains("内核系统进程"),
            "拒因必须点名：{e}"
        );
        assert!(p.killed.lock().is_empty(), "受保护 pid 不得触达端口");
        // 被拒也落审计（承重：kill 成功/被拒均落 audit）
        let log = t.audit_entries();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].outcome, "refused");
        assert_eq!(log[0].pid, 4);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-10）字面测试名优先于 rustc 命名惯例
    fn kill_confirmNameMismatch_refuses() {
        let dir = tmp("mismatch");
        let p = FakePort::new();
        p.set_names(vec![(123, "chrome.exe")]);
        let t = table(p.clone(), &dir);
        // 任务书夹具："notepad" vs "chrome"
        let e = t.kill(123, "notepad").unwrap_err();
        assert!(e.to_string().contains("复述名"), "拒因点名复述闸：{e}");
        assert!(p.killed.lock().is_empty());
        // 复述逐字一致（大小写不敏感）→ 放行且回执为实际进程名 + killed 审计
        let name = t.kill(123, "CHROME.EXE").unwrap();
        assert_eq!(name, "chrome.exe");
        assert_eq!(*p.killed.lock(), vec![123]);
        let log = t.audit_entries();
        assert_eq!(log.len(), 2);
        assert_eq!(log[1].outcome, "killed");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-10）字面测试名优先于 rustc 命名惯例
    fn kill_guardFileCorrupt_disablesAllKills() {
        let dir = tmp("corrupt");
        std::fs::create_dir_all(dir.join("sys")).unwrap();
        std::fs::write(dir.join("sys").join("process_guard.json"), b"{ nope").unwrap();
        let p = FakePort::new();
        p.set_names(vec![(11, "notepad.exe")]);
        let t = table(p.clone(), &dir);
        // 反向承重：名单坏 = 谁也不能杀（连未受保护的常规进程也拒）
        let e = t.kill(11, "notepad.exe").unwrap_err();
        assert!(e.to_string().contains("拒绝结束任何进程"), "{e}");
        assert!(p.killed.lock().is_empty());
        assert_eq!(t.audit_entries()[0].outcome, "refused");
        // 正对照：好盘（合法扩展名单）不误伤——未列入的可杀、列入的拒杀
        std::fs::write(
            dir.join("sys").join("process_guard.json"),
            br#"{"protected":["mytool.exe"]}"#,
        )
        .unwrap();
        let p2 = FakePort::new();
        p2.set_names(vec![(11, "notepad.exe"), (12, "MyTool.exe")]);
        let t2 = table(p2.clone(), &dir);
        assert!(
            t2.kill(11, "notepad.exe").is_ok(),
            "坏盘裁决不得波及好盘（含累积审计）"
        );
        assert!(
            t2.kill(12, "MyTool.exe").is_err(),
            "数据文件扩展名单必须生效"
        );
        assert_eq!(p2.killed.lock().as_slice(), &[11]);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-10）字面测试名优先于 rustc 命名惯例
    fn processRow_diskBps_noneOnFirstSample_notZero() {
        let dir = tmp("disknone");
        let p = FakePort::new();
        let t = table(p.clone(), &dir);
        p.set_current(vec![
            snap(11, "a.exe", 10, 1, Some(1000)),
            snap(12, "b.exe", 10, 1, None),
        ]);
        let first = t.top_n("disk", 10, "", 0);
        assert_eq!(first.len(), 2);
        assert!(
            first.iter().all(|r| r.disk_bps.is_none()),
            "首轮无差值：IO 可读也须 None——不编 0 即不编假基线"
        );
        p.set_current(vec![
            snap(11, "a.exe", 20, 1, Some(6000)),
            snap(12, "b.exe", 20, 1, None),
        ]);
        let second = t.top_n("disk", 10, "", 500);
        let a = second.iter().find(|r| r.pid == 11).unwrap();
        assert_eq!(a.disk_bps, Some(10_000), "5000B/0.5s=10000B/s");
        let b = second.iter().find(|r| r.pid == 12).unwrap();
        assert_eq!(b.disk_bps, None, "跨权限读不到 IO：恒 None 非 0");
    }
}
