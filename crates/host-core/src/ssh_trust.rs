//! T-B7-1（09 §7.2）SSH 主机信任**单一事实源**：term 与 file 两域共用的
//! known_hosts 共享存储 + 三态裁决 + 两旧表首见合并。
//!
//! - 落盘形状唯一：[`shared_path`]（`{app_data}/ssh/` 下的信任文件）；
//!   两域源码内该文件名的字面出现各恰一（纯文档位），构造只在本模块
//! - **fail-closed**：文件存在而解析失败 = [`TrustError::Corrupt`]（消息点名
//!   路径 + "以空表启动即被拒绝"）——"坏文件 ⇒ 信任任意主机"正是本模块
//!   存在的反面；term 旧版 `unwrap_or_default` 静默清空缺陷在此结构性缺席
//! - 三态裁决 [`HostKeyDecision`]（两域共读一型，B6 `Resumable` 上移同谱）：
//!   首见 = `Unknown`（**拒连零写**，不再有"首次自动记录"臂）；变更 =
//!   `Changed`（两指纹逐字点名）；整键逐字相等 = `Trusted`
//! - 裸指纹旧记录（term 遗留 `"SHA256:xxx"` 值形制）：指纹段相等即
//!   `Trusted` 并**升级落盘为整键**（[`whole_key_agrees`]）；两枚都是整键
//!   描述符时禁止放宽 base64 段——算法名不同而 base64 巧合相等是两把钥匙
//! - 写盘唯一落点 [`KnownHostsStore::mutate`]：读盘-改-原子写（tmp+rename）
//!   + 进程内写锁——term/file 两个视图实例不互相覆盖对方的新增记录

use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 共享信任文件相对 appData 根的位置（文件名全仓唯一定义处）
const SHARED_DIR: &str = "ssh";
const SHARED_FILE: &str = "known_hosts.json";

/// 共享信任文件路径：`{app_data}/ssh/known_hosts.json`
pub fn shared_path(app_data: &Path) -> PathBuf {
    app_data.join(SHARED_DIR).join(SHARED_FILE)
}

/// 规范键：端口 22 省略为裸 host，其余 `[host]:port`。
/// 两域现行规则本就同谱（T-B7-1 核账修正：任务书"废除 22 省略"以
/// 两域实况为准维持原形制——废除会让全部既有记录与 UI 显示无谓失效）。
pub fn canonical_key(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    }
}

/// 整键描述符是否带算法名前缀（`"algo SHA256:base64"` 为整键，
/// `"SHA256:base64"` 为 term 遗留裸指纹）
fn has_algo_prefix(whole_key: &str) -> bool {
    whole_key.split_once(' ').is_some()
}

/// SHA256 指纹段（整键/裸形的最后一段空白分隔 token）
fn fingerprint_part(whole_key: &str) -> &str {
    whole_key.rsplit(' ').next().unwrap_or(whole_key)
}

/// 整键口径比对（两域两道门的同一比较口径）：
/// - 整串逐字相等：放行
/// - 记录侧是裸旧形：放宽到指纹段相等（升级裁决由 [`KnownHostsStore::decide`] 执行）
/// - 记录侧已是整键而实收不同：**不放行**（含"base64 同而算法名不同"）
pub fn whole_key_agrees(recorded: &str, actual: &str) -> bool {
    if recorded == actual {
        return true;
    }
    !recorded.is_empty()
        && !has_algo_prefix(recorded)
        && fingerprint_part(recorded) == fingerprint_part(actual)
}

/// TOFU 三态裁决（纯数据，零 IO；两域共读一型）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyDecision {
    Trusted { fingerprint: String },
    Unknown { fingerprint: String },
    Changed { recorded: String, actual: String },
}

/// 共享存储面错误
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("信任文件落盘失败: {0}")]
    Io(String),
    #[error(
        "known_hosts 不可解析（路径 {path}）：{reason}。损坏文件启动即报、不静默清空——\
             以空表启动即被拒绝：清空等于接受任意主机。请人工核对后从信得过的副本恢复该文件"
    )]
    Corrupt { path: String, reason: String },
}

/// 进程内写锁：term/file 两视图同写一文件时读-改-写不交错（丢记录=静默
/// 削信任，与 fail-closed 立论冲突）。std sync Mutex 足够——写面本就低频。
static WRITE_GUARD: Mutex<()> = Mutex::new(());

/// 共享 known_hosts 存储（进程内视图 + 每次变更同步原子落盘）
pub struct KnownHostsStore {
    path: PathBuf,
    map: RwLock<HashMap<String, String>>,
}

impl std::fmt::Debug for KnownHostsStore {
    /// 表内容只有公开主机键描述符（无凭据），但 Debug 面仍只报规模——
    /// 少一张可被日志整版抄走的表
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KnownHostsStore")
            .field("path", &self.path)
            .field("records", &self.map.read().len())
            .finish()
    }
}

impl KnownHostsStore {
    /// 按确切路径打开（**fail-closed**）：不存在 = 尚无记录（空表）；
    /// 存在而解析失败 = `Err` 点名路径，绝无解析兜底口。
    pub fn open_at(path: &Path) -> Result<Self, TrustError> {
        let map = match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice::<HashMap<String, String>>(&bytes).map_err(|e| {
                    TrustError::Corrupt {
                        path: path.display().to_string(),
                        reason: e.to_string(),
                    }
                })?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                return Err(TrustError::Io(format!(
                    "信任文件不可读（路径 {}）: {e}",
                    path.display()
                )))
            }
        };
        Ok(Self {
            path: path.to_path_buf(),
            map: RwLock::new(map),
        })
    }

    /// 以 appData 根打开共享文件；共享文件**不在场**且任一旧表在场时执行
    /// 一次性 [`merge_legacy`]。**顺序即裁决**：新表原子落盘成功在前，
    /// 旧表证据改名在后——中途崩溃则下次启动重跑合并（纯读幂等），
    /// 不存在"旧表已退役而新表未成"的无信任面窗口。
    pub fn shared(app_data: &Path) -> Result<Self, TrustError> {
        let path = shared_path(app_data);
        if path.exists() {
            return Self::open_at(&path);
        }
        let term_legacy = app_data.join("term").join(SHARED_FILE);
        let file_legacy = app_data.join("file").join(SHARED_FILE);
        match merge_legacy(&term_legacy, &file_legacy) {
            Some(merged) => {
                for w in &merged.warnings {
                    tracing::warn!("[ssh_trust] 旧表合并: {w}");
                }
                let store = Self {
                    path,
                    map: RwLock::new(merged.map),
                };
                store.flush_locked()?;
                apply_evidence_renames(&merged.evidence_renames);
                Ok(store)
            }
            None => Self::open_at(&path),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get(&self, host: &str, port: u16) -> Option<String> {
        self.map.read().get(&canonical_key(host, port)).cloned()
    }

    /// 三态裁决（首见零写；裸旧记录命中 Trusted 时升级落盘为整键）
    pub fn decide(&self, host: &str, port: u16, actual: &str) -> HostKeyDecision {
        let key = canonical_key(host, port);
        let recorded = self.map.read().get(&key).cloned();
        match recorded {
            None => HostKeyDecision::Unknown {
                fingerprint: actual.to_owned(),
            },
            Some(rec) if whole_key_agrees(&rec, actual) => {
                if !has_algo_prefix(&rec) && has_algo_prefix(actual) {
                    // 升级写失败不改裁决（该记录本就是用户明示接受的）——
                    // 但必须 warn：下道门的整键严格口径依赖这次收紧
                    if let Err(e) = self.mutate(|m| {
                        m.insert(key.clone(), actual.to_owned());
                    }) {
                        tracing::warn!("[ssh_trust] 裸旧记录升级落盘失败（{key}）: {e}");
                    }
                }
                HostKeyDecision::Trusted {
                    fingerprint: actual.to_owned(),
                }
            }
            Some(rec) => HostKeyDecision::Changed {
                recorded: rec,
                actual: actual.to_owned(),
            },
        }
    }

    /// 用户明示接受后记录**整键描述符**（唯一写入口；TOFU 首见绝不自动流经）
    pub fn accept(&self, host: &str, port: u16, whole_key: &str) -> Result<(), TrustError> {
        let key = canonical_key(host, port);
        let whole = whole_key.to_owned();
        self.mutate(move |m| {
            m.insert(key, whole);
        })
    }

    /// 删除记录（用户确认主机重建后允许重连）
    pub fn remove(&self, host: &str, port: u16) -> Result<bool, TrustError> {
        let key = canonical_key(host, port);
        let removed = std::sync::atomic::AtomicBool::new(false);
        self.mutate(|m| {
            if m.remove(&key).is_some() {
                removed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        })?;
        Ok(removed.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// 全部记录（规范键 → 整键描述符；管理页消费面）
    pub fn entries(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .map
            .read()
            .iter()
            .map(|(k, d)| (k.clone(), d.clone()))
            .collect();
        v.sort();
        v
    }

    /// 唯一变更出口：持进程写锁 → 以**盘态**为基线改内存（两视图不互相
    /// 覆盖）→ tmp+rename 原子落盘。盘态损坏时 `Err`——不写穿坏文件。
    fn mutate(&self, f: impl FnOnce(&mut HashMap<String, String>)) -> Result<(), TrustError> {
        let _guard = lock_write();
        let mut map = match std::fs::read(&self.path) {
            Ok(bytes) => {
                serde_json::from_slice::<HashMap<String, String>>(&bytes).map_err(|e| {
                    TrustError::Corrupt {
                        path: self.path.display().to_string(),
                        reason: e.to_string(),
                    }
                })?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => self.map.read().clone(),
            Err(e) => return Err(TrustError::Io(format!("{e}"))),
        };
        f(&mut map);
        *self.map.write() = map.clone();
        self.flush_map(&map)
    }

    fn flush_locked(&self) -> Result<(), TrustError> {
        let _guard = lock_write();
        let map = self.map.read().clone();
        self.flush_map(&map)
    }

    fn flush_map(&self, map: &HashMap<String, String>) -> Result<(), TrustError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                TrustError::Io(format!("目录创建失败（{}）: {e}", parent.display()))
            })?;
        }
        let data = serde_json::to_vec_pretty(map)
            .map_err(|e| TrustError::Io(format!("known_hosts 序列化失败: {e}")))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &data)
            .map_err(|e| TrustError::Io(format!("临时文件写入失败: {e}")))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| TrustError::Io(format!("原子改名失败（{}）: {e}", self.path.display())))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.read().len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn lock_write() -> parking_lot::MutexGuard<'static, ()> {
    // 中毒恢复：临界区只做文件 IO/serde，panic 不留下半程状态语义
    WRITE_GUARD.lock()
}

// ---------------------------------------------------------------------------
// 旧表一次性合并（T-B7-1 数据变更：两表收拢单源）
// ---------------------------------------------------------------------------

/// 合并结果：并集映射 + 点名警告 + 证据改名对（from → to，由调用方
/// 在**共享文件原子落盘成功之后**执行——先落新表再退役旧表，
/// 中途崩溃下次重跑合并且结果一致，绝不出现在无信任面的窗口）
#[derive(Debug)]
pub struct MergedHosts {
    pub map: HashMap<String, String>,
    pub warnings: Vec<String>,
    pub evidence_renames: Vec<(PathBuf, PathBuf)>,
}

enum LegacyRead {
    Absent,
    /// 在场但不可读/不可解析
    Unusable(String),
    Records(HashMap<String, String>),
}

fn read_legacy(path: &Path) -> LegacyRead {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => LegacyRead::Absent,
        Err(e) => LegacyRead::Unusable(format!("不可读: {e}")),
        Ok(bytes) => match serde_json::from_slice::<HashMap<String, String>>(&bytes) {
            Ok(m) => LegacyRead::Records(m),
            Err(e) => LegacyRead::Unusable(format!("不可解析: {e}")),
        },
    }
}

/// 两旧表首见合并（**纯读函数，零副作用**）：任一旧路径在场即读并集
/// （同键冲突取 term 值并 warn 点名——term 表是既有用户已接受集，file
/// 表后建、覆盖面小）；可读数品的改名落点（`*.migrated`）与损坏数品的
/// 留证落点（`*.corrupt`）以 [`MergedHosts::evidence_renames`] 返回，
/// **实际改名由 [`KnownHostsStore::shared`] 在新表原子落盘后执行**
/// （回退证据不删）。两枚都不在场返回 `None`。
///
/// 与**活体**存储的裁决分歧（互点名，勿"顺手统一"）：
/// - 活体文件损坏 ⇒ [`TrustError::Corrupt`] 拒启（fail-closed：那是当下
///   生效的信任面）；
/// - 旧文件损坏 ⇒ 留证改名 `.corrupt` + warn + 跳过——旧文件退役中，为
///   一份已放弃的文件锁死应用启动不成比例（对照 T-B7-14 历史环同谱裁决）。
pub fn merge_legacy(term_path: &Path, file_path: &Path) -> Option<MergedHosts> {
    let term = read_legacy(term_path);
    let file = read_legacy(file_path);
    let absent = |l: &LegacyRead| matches!(l, LegacyRead::Absent);
    if absent(&term) && absent(&file) {
        return None;
    }
    let mut warnings = Vec::new();
    let mut evidence_renames = Vec::new();
    let mut map = HashMap::new();
    for (label, legacy, path) in [("term", term, term_path), ("file", file, file_path)] {
        let records = match legacy {
            LegacyRead::Absent => continue,
            LegacyRead::Unusable(why) => {
                // 退役文件损坏：留证改名交调用方执行，不锁死启动（分歧裁决见函数头）
                let corrupt = path.with_extension("json.corrupt");
                warnings.push(format!(
                    "旧 {label} 信任表（{}）{why}——改名 {} 留证跳过（不静默删除）",
                    path.display(),
                    corrupt.file_name().unwrap_or_default().to_string_lossy()
                ));
                evidence_renames.push((path.to_path_buf(), corrupt));
                continue;
            }
            LegacyRead::Records(m) => m,
        };
        for (k, v) in records {
            match map.get(&k) {
                // term 先行处理，后来者（file）遇已有键即冲突——取 term
                Some(prev) if *prev != v => {
                    warnings.push(format!(
                        "旧表同键冲突（{k}）：记录两侧指纹不一致，取 term 侧值 {prev}，弃 file 侧 {v}"
                    ));
                }
                Some(_) => {}
                None => {
                    map.insert(k, v);
                }
            }
        }
        evidence_renames.push((path.to_path_buf(), path.with_extension("json.migrated")));
    }
    // 冲突语义修正：term 先 insert 则 term 值天然在场；上面 file 臂的
    // Some(prev) 分支 prev 即 term 值——warn 文案已按此固定顺序撰写
    Some(MergedHosts {
        map,
        warnings,
        evidence_renames,
    })
}

/// 执行合并的证据改名（NotFound 静默——同名旧路径已被上轮消费）
fn apply_evidence_renames(renames: &[(PathBuf, PathBuf)]) {
    for (from, to) in renames {
        match std::fs::rename(from, to) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                // 改名失败 ⇒ 下次启动重跑合并（幂等并集，无损害），只 warn
                tracing::warn!(
                    "[ssh_trust] 旧信任表证据改名失败（{} → {}）: {e}——下次启动将再次尝试合并",
                    from.display(),
                    to.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_ssh_trust_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_json(path: &Path, obj: &serde_json::Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(obj).unwrap()).unwrap();
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-1）字面测试名优先于 rustc 命名惯例
    fn merge_legacy_twoOldFiles_unionOnce_idempotent() {
        let d = tmpdir("union");
        let term = d.join("term").join("known_hosts.json");
        let file = d.join("file").join("known_hosts.json");
        write_json(
            &term,
            &serde_json::json!({"srv-a": "SHA256:aaa", "srv-b": "ssh-ed25519 SHA256:bbb"}),
        );
        write_json(
            &file,
            &serde_json::json!({"[srv-c]:2222": "ssh-rsa SHA256:ccc"}),
        );
        // 纯读面：直接调用零副作用，改名只以配对返回（落点 .migrated）
        let merged = merge_legacy(&term, &file).expect("两旧表在场必须合并");
        assert_eq!(merged.map.len(), 3, "并集三键");
        assert!(merged.warnings.is_empty());
        assert!(
            term.exists() && file.exists(),
            "merge_legacy 不得有改名副作用（新表未落盘先退役旧表 = 无信任面窗口）"
        );
        assert_eq!(
            merged
                .evidence_renames
                .iter()
                .map(|(from, to)| (from.clone(), to.clone()))
                .collect::<Vec<_>>(),
            vec![
                (term.clone(), term.with_extension("json.migrated")),
                (file.clone(), file.with_extension("json.migrated")),
            ],
            "两旧表各一枚证据改名，term 在前"
        );
        // 经生产路径走完整迁移：新表原子落盘在前，旧表退役改名在后
        let store = KnownHostsStore::shared(&d).unwrap();
        assert_eq!(store.len(), 3);
        assert!(shared_path(&d).exists());
        assert!(!term.exists() && term.with_extension("json.migrated").exists());
        assert!(!file.exists() && file.with_extension("json.migrated").exists());
        drop(store);
        let again = KnownHostsStore::shared(&d).unwrap();
        assert_eq!(again.len(), 3, "共享文件在场即直开，不再触旧表");
        // 消费后两旧路径 Absent → None（幂等语义的另一半）
        assert!(merge_legacy(&term, &file).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-1）字面测试名优先于 rustc 命名惯例
    fn merge_legacy_conflict_prefersTermAndWarns() {
        let d = tmpdir("conflict");
        let term = d.join("term").join("known_hosts.json");
        let file = d.join("file").join("known_hosts.json");
        write_json(&term, &serde_json::json!({"srv-x": "SHA256:term-wins"}));
        write_json(
            &file,
            &serde_json::json!({"srv-x": "ssh-ed25519 SHA256:lost"}),
        );
        let merged = merge_legacy(&term, &file).unwrap();
        assert_eq!(merged.map.get("srv-x").unwrap(), "SHA256:term-wins");
        assert_eq!(
            merged.warnings.len(),
            1,
            "冲突必须点名：{:?}",
            merged.warnings
        );
        assert!(merged.warnings[0].contains("srv-x"));
        assert!(merged.warnings[0].contains("term-wins") && merged.warnings[0].contains("lost"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-1）字面测试名优先于 rustc 命名惯例
    fn wholeKeyAgrees_threeArms() {
        // 整键逐字相等
        assert!(whole_key_agrees(
            "ssh-ed25519 SHA256:abc",
            "ssh-ed25519 SHA256:abc"
        ));
        // base64 巧合相等而算法名不同：两枚整键间禁止放宽
        assert!(!whole_key_agrees(
            "ssh-rsa SHA256:abc",
            "ssh-ed25519 SHA256:abc"
        ));
        // 裸旧记录：指纹段相等即同意（升级裁决另臂执行）
        assert!(whole_key_agrees("SHA256:abc", "ssh-ed25519 SHA256:abc"));
        assert!(!whole_key_agrees("SHA256:abc", "ssh-ed25519 SHA256:zzz"));
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-1）字面测试名优先于 rustc 命名惯例
    fn store_corruptFile_isErrWithNamedPath() {
        let d = tmpdir("corrupt");
        let p = shared_path(&d);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"{ torn").unwrap();
        let e = KnownHostsStore::open_at(&p).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("known_hosts"), "必须点名文件，实得 {msg}");
        assert!(msg.contains(&p.display().to_string(),), "必须点名路径");
        assert!(
            msg.contains("以空表启动即被拒绝"),
            "fail-closed 语义必须显形"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-1）字面测试名优先于 rustc 命名惯例
    fn store_decide_bareLegacyRecord_trustedAndUpgraded() {
        let d = tmpdir("upgrade");
        let p = shared_path(&d);
        write_json(&p, &serde_json::json!({"h": "SHA256:abc"}));
        let store = KnownHostsStore::open_at(&p).unwrap();
        let dec = store.decide("h", 22, "ssh-ed25519 SHA256:abc");
        assert!(matches!(dec, HostKeyDecision::Trusted { .. }));
        // 落盘已收紧为整键：下次即便算法换名也能被 Changed 臂抓到
        let on_disk: HashMap<String, String> =
            serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(on_disk.get("h").unwrap(), "ssh-ed25519 SHA256:abc");
        let _ = std::fs::remove_dir_all(&d);
    }
}
