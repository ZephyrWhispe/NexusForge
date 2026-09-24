//! 会话清单（T-B7-20）：标签"行"落盘（`{app_data}/editor/session_list.json`）。
//!
//! 恢复的是**行**不是内容——load 只做 stat 校验与占位会话入表，
//! 文件内容在用户点开（`content()` 首拉）时才读，防启动扫大盘。
//! 裁决族承 T-B7-14（可观测数据谱，非 T-B7-1 信任面谱）：坏档 = warn + 改名
//! `.corrupt` 留证 + 空清单继续，**不锁死启动**；盘上已删的行 = load 时弃行 +
//! warn 点名（不显示死标签）。写盘 = tmp + rename 原子替换。

use serde::{Deserialize, Serialize};

use crate::session::{EncodingKind, Eol};

/// 清单文件名（store_dir 下）
pub const SESSION_LIST_FILE: &str = "session_list.json";

/// 单条标签行元信息（`opened_ms` 升序 = 页签序）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub path: String,
    /// 光标行（1 起；随 autosave 顺带更新，重启后首载定位）
    pub cursor_line: u32,
    pub eol: Eol,
    pub preferred_encoding: Option<EncodingKind>,
    /// 打开时刻（Unix 毫秒），兼作页签排序锚
    pub opened_ms: i64,
}

/// 清单文件路径
pub fn manifest_path(store_dir: &std::path::Path) -> std::path::PathBuf {
    store_dir.join(SESSION_LIST_FILE)
}

/// `load_manifest` 结果报告（warn 的可断言面：弃行不静默=谎报）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManifestReport {
    /// 恢复入表的行数
    pub restored: usize,
    /// 盘上已删（stat 失败）被弃行的路径
    pub dropped: Vec<String>,
    /// 清单损坏（已改名 `.corrupt` 留证并按空清单继续）
    pub corrupt: bool,
}

/// 原子写（tmp + rename；承 T-B7-14 runs.json 形制）
pub fn write_manifest(store_dir: &std::path::Path, rows: &[SessionMeta]) -> std::io::Result<()> {
    let final_path = manifest_path(store_dir);
    let tmp_path = final_path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(rows)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, &final_path)
}

/// 读清单。返回 `(rows, corrupt)`：文件不存在 = 空且非损坏（静默首启）；
/// 解析失败 = 改名 `.corrupt` 留证 + 空清单继续（warn 在册）。
/// 改名失败不升级为主流程失败——留证尽力而为。
pub fn read_manifest(store_dir: &std::path::Path) -> (Vec<SessionMeta>, bool) {
    let path = manifest_path(store_dir);
    let Ok(raw) = std::fs::read(&path) else {
        return (Vec::new(), false);
    };
    match serde_json::from_slice::<Vec<SessionMeta>>(&raw) {
        Ok(rows) => (rows, false),
        Err(e) => {
            let corrupt_path = path.with_extension("json.corrupt");
            if let Err(ren) = std::fs::rename(&path, &corrupt_path) {
                tracing::warn!(error = %ren, "会话清单损坏后留证改名失败（按空清单继续）");
            }
            tracing::warn!(error = %e, "会话清单损坏，改名 .corrupt 留证并按空清单继续");
            (Vec::new(), true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nf_editor_store_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn manifest_atomic_write_roundtrip() {
        let dir = tmpdir("io");
        let rows = vec![SessionMeta {
            path: "C:\\notes\\a.txt".into(),
            cursor_line: 3,
            eol: Eol::Crlf,
            preferred_encoding: Some(EncodingKind::Gbk),
            opened_ms: 1_700_000_000_000,
        }];
        write_manifest(&dir, &rows).unwrap();
        assert!(
            !manifest_path(&dir).with_extension("json.tmp").exists(),
            "tmp 须被 rename 收走"
        );
        let (got, corrupt) = read_manifest(&dir);
        assert_eq!(got, rows);
        assert!(!corrupt);
    }

    #[test]
    fn manifest_corrupt_keeps_evidence() {
        let dir = tmpdir("corrupt");
        std::fs::write(manifest_path(&dir), b"{ not json".as_slice()).unwrap();
        let (rows, corrupt) = read_manifest(&dir);
        assert!(rows.is_empty());
        assert!(corrupt, "损坏须上报（弃行不静默=谎报）");
        assert!(
            dir.join("session_list.json.corrupt").exists(),
            "损坏档须留证"
        );
        assert!(!manifest_path(&dir).exists(), "损坏原件不得留在原位");
    }
}
