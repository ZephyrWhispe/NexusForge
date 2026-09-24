//! T-B7-14（09 §7.2）：规则执行历史环 + runs.json 落盘。
//!
//! - 环容量 [`HISTORY_CAP`]，溢出丢最旧；落 `{app_data}/automation/runs.json`，
//!   写形制沿 B6 profiles（tmp + sync_all + rename 原子替换）。
//! - `Partial` 是新事实非旧数据重释：旧盘无此文件 = 首载空表。
//! - **坏文件裁决（与 T-B7-1 known_hosts 相反，互点名勿"顺手统一"）**：
//!   历史 = 可观测数据非安全决定——坏文件首载清空可接受，但不静默抹除：
//!   warn + 改名 `runs.json.corrupt` 留证。对照 known_hosts（T-B7-1）：那是
//!   信任决定，坏盘 fail-closed 拒启（见 host-core ssh_trust.rs 同谱注释）。
//! - 多进程同写一文件（standalone --run-rule 与主程序并发的窄窗口）为
//!   last-writer-wins 整文件替换：历史是观测面，丢几条在宽限内，与
//!   rules.json 的既有整文件写形制同谱，不另上锁。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// 历史环容量：溢出丢最旧
pub const HISTORY_CAP: usize = 500;

/// 单规则一轮执行的结果形状（多动作后**部分成功必须可表达**，承 T-B7-13）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunOutcome {
    Success,
    Partial { failed_indices: Vec<u32> },
    Failure,
}

/// 一条执行历史（IPC DTO 直出）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRecord {
    pub rule_id: String,
    pub fired_ms: i64,
    pub outcome: RunOutcome,
    /// 本轮尝试执行的动作数（串行全试：等于规则 then 长度）
    pub actions_executed: u32,
    pub duration_ms: u64,
    /// 首个失败动作的错误文本（Success 时为 None）
    pub error: Option<String>,
}

/// 执行历史环（内存态 + runs.json 盘态同源）
pub struct History {
    path: PathBuf,
    records: Mutex<Vec<RunRecord>>,
    /// 首载遇坏文件的留证改名发生过（warn 的结构化证据，测试钉用）
    corrupt_renamed: AtomicBool,
}

impl History {
    /// 打开 `{automation_dir}/runs.json`（不存在 = 空环；坏文件 = warn+改名留证+空环）
    pub fn open(automation_dir: &std::path::Path) -> Self {
        let path = automation_dir.join("runs.json");
        let mut corrupt_renamed = false;
        let records = match std::fs::read(&path) {
            Err(_) => Vec::new(),
            Ok(bytes) => match serde_json::from_slice::<Vec<RunRecord>>(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    let corrupt = path.with_extension("json.corrupt");
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "runs.json 不可解析——改名 {} 留证后以空环启动（历史=可观测数据，\
                         与 known_hosts 信任面 fail-closed 裁决相反，见模块头互点名）",
                        corrupt.file_name().unwrap_or_default().to_string_lossy()
                    );
                    // 留证改名不静默删除；改名失败也不写穿坏文件（本进程只留内存环）
                    if std::fs::rename(&path, &corrupt).is_ok() {
                        corrupt_renamed = true;
                    }
                    Vec::new()
                }
            },
        };
        Self {
            path,
            records: Mutex::new(records),
            corrupt_renamed: AtomicBool::new(corrupt_renamed),
        }
    }

    /// 首载遇坏文件且已留证改名（warn 的结构化证据）
    pub fn corrupt_renamed(&self) -> bool {
        self.corrupt_renamed.load(Ordering::SeqCst)
    }

    /// 追加一条：环内裁剪到 [`HISTORY_CAP`]（丢最旧）后整表原子落盘
    pub fn push(&self, record: RunRecord) {
        let snapshot = {
            let mut v = self.records.lock();
            v.push(record);
            let overflow = v.len().saturating_sub(HISTORY_CAP);
            if overflow > 0 {
                v.drain(0..overflow);
            }
            v.clone()
        };
        if let Err(e) = self.persist(&snapshot) {
            tracing::warn!(path = %self.path.display(), error = %e, "执行历史落盘失败（内存环不受影响）");
        }
    }

    /// 历史列表（旧 → 新）。`limit` 有界：None=整环，Some(n) 取最近 n 条，
    /// 且恒不超过环容量（防超大 limit 当无界读口）。
    pub fn runs(&self, limit: Option<usize>) -> Vec<RunRecord> {
        let v = self.records.lock();
        let want = limit.unwrap_or(HISTORY_CAP).min(HISTORY_CAP);
        if want >= v.len() {
            return v.clone();
        }
        v[v.len() - want..].to_vec()
    }

    fn persist(&self, records: &[RunRecord]) -> crate::error::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(crate::AutomationError::Io)?;
        }
        let bytes = serde_json::to_vec_pretty(records)
            .map_err(|e| crate::AutomationError::BadRule(format!("历史序列化失败: {e}")))?;
        let tmp = self.path.with_extension("json.tmp");
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).map_err(crate::AutomationError::Io)?;
        f.write_all(&bytes).map_err(crate::AutomationError::Io)?;
        f.sync_all().map_err(crate::AutomationError::Io)?;
        std::fs::rename(&tmp, &self.path).map_err(crate::AutomationError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nf-hist-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ))
    }

    fn rec(i: i64) -> RunRecord {
        RunRecord {
            rule_id: format!("r{i}"),
            fired_ms: i,
            outcome: RunOutcome::Success,
            actions_executed: 1,
            duration_ms: 0,
            error: None,
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-14）字面测试名优先于 rustc 命名惯例
    fn history_ringCaps_dropsOldest() {
        let d = tmpdir("cap");
        let h = History::open(&d);
        for i in 0..501 {
            h.push(rec(i));
        }
        let got = h.runs(None);
        assert_eq!(got.len(), HISTORY_CAP, "501 条写入读回须恰剩环容量");
        assert_eq!(got[0].fired_ms, 1, "最旧一条（fired_ms=0）应被挤出");
        assert_eq!(got[HISTORY_CAP - 1].fired_ms, 500, "最新一条必须在场");
        // 盘态同裁定：重开读回 500 条（落盘的是裁剪后的整表）
        let reopened = History::open(&d);
        assert_eq!(reopened.runs(None).len(), HISTORY_CAP);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-14）字面测试名优先于 rustc 命名惯例
    fn runsGet_limitBounded() {
        let d = tmpdir("limit");
        let h = History::open(&d);
        for i in 0..10 {
            h.push(rec(i));
        }
        let last3 = h.runs(Some(3));
        assert_eq!(
            last3.iter().map(|r| r.fired_ms).collect::<Vec<_>>(),
            vec![7, 8, 9],
            "limit 取最近 n 条且旧→新序"
        );
        assert_eq!(h.runs(None).len(), 10, "None=整环");
        assert!(h.runs(Some(0)).is_empty(), "limit 0 显式空页而非全量");
        // 超大 limit 恒被环容量钳住（不成为无界读口）
        for i in 10..600 {
            h.push(rec(i));
        }
        assert_eq!(h.runs(Some(1_000_000)).len(), HISTORY_CAP);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-14）字面测试名优先于 rustc 命名惯例
    fn history_corruptFile_emptyWithWarn_notSilentWipe() {
        let d = tmpdir("corrupt");
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("runs.json");
        std::fs::write(&path, b"{ not json at all").unwrap();
        let h = History::open(&d);
        assert!(h.runs(None).is_empty(), "坏盘首载空表（可观测数据裁决）");
        assert!(h.corrupt_renamed(), "warn 的结构化证据：留证改名必须发生");
        assert!(!path.exists(), "原坏文件不得原地留存被下轮写穿");
        let corrupt = d.join("runs.json.corrupt");
        assert_eq!(
            std::fs::read(&corrupt).unwrap(),
            b"{ not json at all",
            "留证：坏字节原样改名保留，不静默抹除"
        );
        // 正对照防"改名即清空"式空洞：留证后新写入能正常落盘
        h.push(rec(42));
        assert_eq!(History::open(&d).runs(None).len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
