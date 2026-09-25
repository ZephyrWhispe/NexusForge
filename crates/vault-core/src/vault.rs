//! V3 解锁/锁定流程（docs/impl/05 V3）：内存密钥生命周期。
//!
//! - 未初始化 → create（Argon2id 默认参数，一次成库）→ Locked
//! - unlock：连续错误 `MAX_ATTEMPTS` 次 → `LOCKOUT` 冷却（正确密码也被拒）
//! - lock：DEK 立即 wipe（Drop 兜底）；此后全部数据操作拒绝
//! - Argon2 慢操作**不持锁**执行（锁内只读快照/写结果）
//! - V4 Hello 免密（D-24）：`hello_unlock` = HelloPort 校验门 → CryptoPort
//!   （DPAPI）解包 DEK 副本 → DEK 自证 verifier（aad=vault_id）三重绑定；
//!   校验失败 ≥`MAX_ATTEMPTS` 次本进程内禁用 Hello 路径，密码解锁成功后恢复
//! - V5 自动锁活动足迹（D-24）：`touch`/`set_blur` 记录秒级时间戳，
//!   [`VaultService::autolock_action`] 交给 [`crate::autolock`] 纯函数评估

use parking_lot::{Mutex, RwLock};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use host_core::error::AppError;
use host_core::ports::{CryptoPort, HelloPort, Ports};

use crate::autolock::{self, AutolockAction, AutolockPolicy};
use crate::crypto::{self, HelloEnvelope, KdfParams, SecretKey, VaultHeader};
use crate::model::{Entry, EntryField, EntryRow, Folder, VaultStore};

/// 连续错误上限（阶段二验收：错误密码 5 次锁定）
pub const MAX_ATTEMPTS: u32 = 5;
/// 触发上限后的冷却窗口
pub const LOCKOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultState {
    Uninitialized,
    Locked,
    Unlocked,
}

enum Inner {
    Uninitialized,
    Locked,
    Unlocked {
        dek: SecretKey,
    },
    /// 冷却中：arg 保存冷却结束时刻
    Cooling {
        until: Instant,
    },
}

use host_core::util::app_err as err;

/// hello_ports 返回的两端口元组（clippy type_complexity 收敛）
type HelloPorts = (Arc<dyn HelloPort>, Arc<dyn CryptoPort>);

pub struct VaultService {
    meta_path: PathBuf,
    store: VaultStore,
    header: RwLock<Option<VaultHeader>>,
    inner: Mutex<Inner>,
    /// 连续错误计数（独立小锁：Argon2 慢操作不持 inner 锁）
    attempts: Mutex<u32>,
    /// V4/V5 端口（CryptoPort=DPAPI 包裹、HelloPort=校验门）；未绑定 = Hello 不可用
    ports: RwLock<Option<Arc<Ports>>>,
    /// V5 秒级时基（open 时刻；autolock 纯函数用相对秒）
    epoch: Instant,
    last_touch: Mutex<Instant>,
    blur_since: Mutex<Option<Instant>>,
    /// Hello 校验连续失败计数与进程级熔断（≥MAX_ATTEMPTS → 强制密码解锁）
    hello_failures: Mutex<u32>,
    hello_forced: Mutex<bool>,
}

impl VaultService {
    /// app_data_dir：meta 存 `{dir}/vault/vault.meta.json`，库存 `{dir}/db/vault.db`
    pub fn open(app_data_dir: &Path) -> Result<Self, AppError> {
        let meta_path = app_data_dir.join("vault").join("vault.meta.json");
        let store = VaultStore::open(&app_data_dir.join("db").join("vault.db"))?;
        let header = if meta_path.exists() {
            let raw = std::fs::read_to_string(&meta_path)
                .map_err(|e| err("VAULT_META_001", format!("读取 vault.meta 失败: {e}")))?;
            Some(
                serde_json::from_str::<VaultHeader>(&raw)
                    .map_err(|e| err("VAULT_META_002", format!("vault.meta 解析失败: {e}")))?,
            )
        } else {
            None
        };
        let inner = if header.is_some() {
            Inner::Locked
        } else {
            Inner::Uninitialized
        };
        let now = Instant::now();
        Ok(Self {
            meta_path,
            store,
            header: RwLock::new(header),
            inner: Mutex::new(inner),
            attempts: Mutex::new(0),
            ports: RwLock::new(None),
            epoch: now,
            last_touch: Mutex::new(now),
            blur_since: Mutex::new(None),
            hello_failures: Mutex::new(0),
            hello_forced: Mutex::new(false),
        })
    }

    /// 绑定端口注册表（host init 时注入；重复绑定以最后一次为准）
    pub fn bind_ports(&self, ports: Arc<Ports>) {
        *self.ports.write() = Some(ports);
    }

    fn ports(&self) -> Option<Arc<Ports>> {
        self.ports.read().clone()
    }

    pub fn state(&self) -> VaultState {
        match &*self.inner.lock() {
            Inner::Uninitialized => VaultState::Uninitialized,
            Inner::Locked | Inner::Cooling { .. } => VaultState::Locked,
            Inner::Unlocked { .. } => VaultState::Unlocked,
        }
    }

    /// 头部快照（IPC 展示 KDF 参数 / vault_id；无机密）
    pub fn header(&self) -> Option<VaultHeader> {
        self.header.read().clone()
    }

    /// 当前冷却剩余秒数（Locked 且 Cooling 时 > 0）
    pub fn lockout_remaining_secs(&self) -> u64 {
        let inner = self.inner.lock();
        match &*inner {
            Inner::Cooling { until } => until.saturating_duration_since(Instant::now()).as_secs(),
            _ => 0,
        }
    }

    /// 新建保险库（未初始化态）；kdf None 用默认参数（64MiB/t3/p4）
    pub fn create(
        &self,
        master_password: &str,
        kdf: Option<KdfParams>,
    ) -> Result<VaultHeader, AppError> {
        let mut inner = self.inner.lock();
        if !matches!(&*inner, Inner::Uninitialized) {
            return Err(err("VAULT_STATE_001", "保险库已存在，不能重复创建"));
        }
        let (header, dek) = match kdf {
            Some(k) => crypto::create_vault_with(master_password, k)?,
            None => crypto::create_vault(master_password)?,
        };
        self.persist_header(&header)?;
        *self.header.write() = Some(header.clone());
        *inner = Inner::Unlocked { dek }; // 先落位
                                          // COR-03：落位后再锁页——SecretKey 已 Box 化（地址稳定），显式顺序便于审阅
        if let Inner::Unlocked { dek } = &*inner {
            dek.lock_in_memory(); // D-24：进解锁态即锁页（失败仅 warn）
        }
        self.touch();
        Ok(header)
    }

    /// 主密码解锁；成功清零尝试计数，连续 `MAX_ATTEMPTS` 次失败进入 `LOCKOUT` 冷却
    pub fn unlock(&self, master_password: &str) -> Result<(), AppError> {
        // 锁内快照，锁外慢操作（Argon2 可达数百 ms～秒级）
        let attempts_snapshot = {
            let inner = self.inner.lock();
            match &*inner {
                Inner::Uninitialized => {
                    return Err(err("VAULT_STATE_002", "保险库未创建"));
                }
                Inner::Cooling { until } if *until > Instant::now() => {
                    let remain = until.saturating_duration_since(Instant::now()).as_secs();
                    return Err(err(
                        "VAULT_LOCKED_002",
                        format!("尝试次数过多，请 {remain}s 后重试"),
                    ));
                }
                // 冷却已结束：降级为 Locked 继续本次尝试（计数已在触发时清零）
                Inner::Cooling { .. } => self.attempts_snapshot(),
                Inner::Unlocked { .. } => return Ok(()), // 幂等
                Inner::Locked => self.attempts_snapshot(),
            }
        };
        let header = self
            .header()
            .ok_or_else(|| err("VAULT_META_003", "头部缺失"))?;
        match crypto::unwrap_dek(&header, master_password) {
            Ok(dek) => {
                let mut inner = self.inner.lock();
                self.store_attempts(0);
                // 密码解锁成功 = Hello 熔断恢复（D-24 决策①：forced 只能被密码路径清除）
                *self.hello_failures.lock() = 0;
                *self.hello_forced.lock() = false;
                *inner = Inner::Unlocked { dek }; // 先落位再锁页（COR-03）
                if let Inner::Unlocked { dek } = &*inner {
                    dek.lock_in_memory();
                }
                self.touch();
                Ok(())
            }
            Err(e) => {
                let attempts = attempts_snapshot + 1;
                let mut inner = self.inner.lock();
                if attempts >= MAX_ATTEMPTS {
                    self.store_attempts(0);
                    let until = Instant::now() + LOCKOUT;
                    *inner = Inner::Cooling { until };
                    tracing::warn!("密码库连续错误 {attempts} 次，进入冷却");
                    Err(err(
                        "VAULT_LOCKED_002",
                        format!("连续错误 {MAX_ATTEMPTS} 次，已锁定 {}s", LOCKOUT.as_secs()),
                    ))
                } else {
                    self.store_attempts(attempts);
                    *inner = Inner::Locked;
                    Err(e)
                }
            }
        }
    }

    /// 锁定：DEK 立即清零（内存密钥生命周期终点；阶段二验收项）
    pub fn lock(&self) -> Result<(), AppError> {
        let mut inner = self.inner.lock();
        match std::mem::replace(&mut *inner, Inner::Locked) {
            Inner::Unlocked { mut dek } => {
                dek.unlock_memory(); // 先解除锁页，再清零（D-24）
                dek.wipe(); // Drop 兜底，这里显式清零
                tracing::info!("密码库已锁定，内存密钥已清零");
            }
            other => *inner = other,
        }
        Ok(())
    }

    /// 改主密码（须解锁态）：DEK 重包，数据条目零改动
    pub fn change_master_password(&self, old: &str, new: &str) -> Result<VaultHeader, AppError> {
        if self.state() != VaultState::Unlocked {
            return Err(err("VAULT_LOCKED_001", "请先解锁再修改主密码"));
        }
        let header = self
            .header()
            .ok_or_else(|| err("VAULT_META_003", "头部缺失"))?;
        let header2 = crypto::change_master_password(&header, old, new)?;
        self.persist_header(&header2)?;
        *self.header.write() = Some(header2.clone());
        Ok(header2)
    }

    // ---- V4 Windows Hello 免密解锁（D-24 决策①）----

    /// Hello 路径是否已启用（头部含信封）
    pub fn hello_enabled(&self) -> bool {
        self.header
            .read()
            .as_ref()
            .and_then(|h| h.hello.as_ref())
            .is_some()
    }

    /// Hello 路径是否被进程级熔断（连续校验失败 ≥MAX_ATTEMPTS）
    pub fn hello_forced_off(&self) -> bool {
        *self.hello_forced.lock()
    }

    fn hello_ports(&self) -> Result<HelloPorts, AppError> {
        let ports = self
            .ports()
            .ok_or_else(|| err("VAULT_HELLO_001", "端口未绑定，Hello 路径不可用"))?;
        let hello = ports
            .get::<dyn HelloPort>()
            .ok_or_else(|| err("VAULT_HELLO_001", "本机无 Windows Hello 可用"))?;
        let crypto = ports
            .get::<dyn CryptoPort>()
            .ok_or_else(|| err("VAULT_HELLO_002", "加密端口未注册"))?;
        Ok((hello, crypto))
    }

    /// 启用免密路径（须解锁态 + 一次成功 Hello 校验）。
    /// DEK 副本 → CryptoPort（DPAPI，账户域绑定）包裹 + DEK 自证 verifier（本库绑定）。
    pub fn hello_enable(&self) -> Result<VaultHeader, AppError> {
        if self.state() != VaultState::Unlocked {
            return Err(err("VAULT_LOCKED_001", "请先解锁再启用 Windows Hello"));
        }
        let (hello_port, crypto_port) = self.hello_ports()?;
        hello_port.verify("启用密码库免密解锁")?;
        let header = self
            .header()
            .ok_or_else(|| err("VAULT_META_003", "头部缺失"))?;
        let dek = self.dek()?;
        let wrapped = crypto_port.protect(dek.expose())?;
        let envelope = HelloEnvelope {
            wrapped_dek_b64: host_core::util::b64_encode(&wrapped),
            verifier: crypto::seal_hello_verifier(&dek, &header.vault_id)?,
        };
        let mut header2 = header;
        header2.hello = Some(envelope);
        self.persist_header(&header2)?;
        *self.header.write() = Some(header2.clone());
        *self.hello_failures.lock() = 0;
        Ok(header2)
    }

    /// 关闭免密路径（须解锁态；头部 hello 置 None 落盘）
    pub fn hello_disable(&self) -> Result<VaultHeader, AppError> {
        if self.state() != VaultState::Unlocked {
            return Err(err("VAULT_LOCKED_001", "请先解锁再关闭 Windows Hello"));
        }
        let mut header = self
            .header()
            .ok_or_else(|| err("VAULT_META_003", "头部缺失"))?;
        if header.hello.take().is_none() {
            return Ok(header); // 幂等
        }
        self.persist_header(&header)?;
        header.hello = None;
        *self.header.write() = Some(header.clone());
        Ok(header)
    }

    /// 免密解锁：Cooling 同样拒绝；三重绑定任一失败计数，≥MAX_ATTEMPTS 熔断
    pub fn hello_unlock(&self) -> Result<(), AppError> {
        {
            let inner = self.inner.lock();
            match &*inner {
                Inner::Uninitialized => return Err(err("VAULT_STATE_002", "保险库未创建")),
                Inner::Cooling { until } if *until > Instant::now() => {
                    let remain = until.saturating_duration_since(Instant::now()).as_secs();
                    return Err(err(
                        "VAULT_LOCKED_002",
                        format!("尝试次数过多，请 {remain}s 后重试"),
                    ));
                }
                Inner::Unlocked { .. } => return Ok(()), // 幂等
                Inner::Locked | Inner::Cooling { .. } => {}
            }
        }
        if self.hello_forced_off() {
            return Err(err(
                "VAULT_HELLO_003",
                "免密校验连续失败过多，本轮次仅允许主密码解锁",
            ));
        }
        let envelope = self
            .header()
            .and_then(|h| h.hello.clone())
            .ok_or_else(|| err("VAULT_HELLO_004", "未启用免密解锁"))?;
        let (hello_port, crypto_port) = self.hello_ports()?;
        if let Err(e) = hello_port.verify("解锁密码库") {
            self.count_hello_failure();
            return Err(e);
        }
        let fail = |s: AppError| {
            self.count_hello_failure();
            s
        };
        let wrapped = host_core::util::b64_decode_lenient(&envelope.wrapped_dek_b64)
            .ok_or_else(|| err("VAULT_HELLO_005", "免密信封损坏"))?;
        // COR-12：DPAPI 解出的明文 DEK 中间缓冲用 Zeroizing 承载，释放即清零，
        // 不残留明文密钥于已释放堆内存
        let dek_bytes: zeroize::Zeroizing<Vec<u8>> = zeroize::Zeroizing::new(
            crypto_port
                .unprotect(&wrapped)
                .map_err(|e| fail(err("VAULT_HELLO_006", e.to_string())))?,
        );
        if dek_bytes.len() != crypto::KEY_LEN {
            return Err(fail(err("VAULT_HELLO_006", "免密密钥长度非法")));
        }
        let mut dek = SecretKey::new(
            dek_bytes
                .as_slice()
                .try_into()
                .map_err(|_| err("VAULT_HELLO_006", "免密密钥长度非法"))?,
        );
        let vault_id = self
            .header()
            .ok_or_else(|| err("VAULT_META_003", "头部缺失"))?
            .vault_id;
        if let Err(e) = crypto::open_hello_verifier(&dek, &vault_id, &envelope.verifier) {
            dek.wipe();
            return Err(fail(e)); // 篡改/跨库重放 → 拒绝并留在 Locked（D-24 验收②）
        }
        let mut inner = self.inner.lock();
        *self.hello_failures.lock() = 0;
        *inner = Inner::Unlocked { dek }; // 先落位再锁页（COR-03）
        if let Inner::Unlocked { dek } = &*inner {
            dek.lock_in_memory();
        }
        self.touch();
        Ok(())
    }

    fn count_hello_failure(&self) {
        let mut f = self.hello_failures.lock();
        *f += 1;
        if *f >= MAX_ATTEMPTS {
            *f = 0;
            *self.hello_forced.lock() = true;
            tracing::warn!("免密校验连续失败 {MAX_ATTEMPTS} 次，本进程内禁用 Hello 路径");
        }
    }

    // ---- V5 自动锁定足迹（D-24 决策②）----

    /// 任一 vault 数据/解锁操作刷新活动时间
    pub fn touch(&self) {
        *self.last_touch.lock() = Instant::now();
    }

    /// 前端窗口失焦/聚焦上报（blur=true 记录失焦起点；false 清除）
    pub fn set_blur(&self, blurred: bool) {
        let mut b = self.blur_since.lock();
        if blurred {
            if b.is_none() {
                *b = Some(Instant::now());
            }
        } else {
            *b = None;
        }
    }

    /// 当前是否解锁态供看门狗评估（Locked 时无需评估，直接 None）
    pub fn autolock_action(&self, policy: &AutolockPolicy) -> Option<AutolockAction> {
        if self.state() != VaultState::Unlocked {
            return None;
        }
        let now = Instant::now();
        let secs = |t: Instant| now.duration_since(self.epoch.max(t)).as_secs();
        let last_touch = secs(*self.last_touch.lock());
        let blur = self.blur_since.lock().map(secs);
        autolock::evaluate(
            now.duration_since(self.epoch).as_secs(),
            last_touch,
            blur,
            policy,
        )
    }

    // ---- 尝试计数（内存态；重启即清零——冷却窗口是进程级防护）----

    fn attempts_snapshot(&self) -> u32 {
        // 由 unlock 在 inner 锁外调用：独立小锁避免与 inner 互相嵌套
        self.attempts.lock().to_owned()
    }

    fn store_attempts(&self, v: u32) {
        *self.attempts.lock() = v;
    }

    fn persist_header(&self, header: &VaultHeader) -> Result<(), AppError> {
        if let Some(parent) = self.meta_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| err("VAULT_META_004", e.to_string()))?;
        }
        let raw = serde_json::to_vec_pretty(header)
            .map_err(|e| err("VAULT_META_005", format!("meta 序列化失败: {e}")))?;
        let tmp = self.meta_path.with_extension("json.tmp");
        std::fs::write(&tmp, raw).map_err(|e| err("VAULT_META_004", e.to_string()))?;
        // COR-11：Windows 的 rename 走 MOVEFILE_REPLACE_EXISTING 会覆盖目标——
        // 先 remove 再 rename 会制造"删旧→写新"窗口，此间崩溃 = vault.meta.json
        // 丢失 = 保险库永久不可解锁。原子替换，绝不先删。
        std::fs::rename(&tmp, &self.meta_path).map_err(|e| err("VAULT_META_004", e.to_string()))?;
        Ok(())
    }

    fn dek(&self) -> Result<SecretKey, AppError> {
        // DEK 拷贝一份给调用方（SecretKey 是 32B 值语义；原件仍由服务持有）
        let inner = self.inner.lock();
        match &*inner {
            Inner::Unlocked { dek } => Ok(SecretKey::new(*dek.expose())),
            _ => Err(err("VAULT_LOCKED_001", "密码库处于锁定状态")),
        }
    }

    // ---- 数据 CRUD（全部要求解锁态）----

    pub fn add_entry(
        &self,
        folder_id: Option<String>,
        title: &str,
        favorite: bool,
        fields: Vec<EntryField>,
        totp_secret: Option<String>,
    ) -> Result<Entry, AppError> {
        let dek = self.dek()?;
        let id = uuid::Uuid::now_v7().to_string();
        let now = crate::model::now_ms();
        let fields_json = serde_json::to_vec(&fields)
            .map_err(|e| err("VAULT_DB_013", format!("fields 序列化失败: {e}")))?;
        let row = EntryRow {
            id: id.clone(),
            folder_id,
            title: title.into(),
            favorite,
            fields_ct: crypto::seal_field(&dek, &id, &fields_json)?,
            totp_ct: totp_secret
                .as_deref()
                .map(|s| crypto::seal_field(&dek, &id, s.as_bytes()))
                .transpose()?,
            created_at: now,
            updated_at: now,
        };
        self.store.insert_entry(&row)?;
        Ok(self.decrypt_row(row, &dek))
    }

    pub fn update_entry(&self, entry: Entry) -> Result<Entry, AppError> {
        let dek = self.dek()?;
        let fields_json = serde_json::to_vec(&entry.fields)
            .map_err(|e| err("VAULT_DB_013", format!("fields 序列化失败: {e}")))?;
        let row = EntryRow {
            id: entry.id.clone(),
            folder_id: entry.folder_id.clone(),
            title: entry.title.clone(),
            favorite: entry.favorite,
            fields_ct: crypto::seal_field(&dek, &entry.id, &fields_json)?,
            totp_ct: entry
                .totp_secret
                .as_deref()
                .map(|s| crypto::seal_field(&dek, &entry.id, s.as_bytes()))
                .transpose()?,
            created_at: entry.created_at,
            updated_at: crate::model::now_ms(),
        };
        if !self.store.update_entry(&row)? {
            return Err(err("VAULT_DB_014", format!("条目 {} 不存在", entry.id)));
        }
        Ok(self.decrypt_row(row, &dek))
    }

    pub fn delete_entry(&self, id: &str) -> Result<bool, AppError> {
        self.dek()?;
        self.store.delete_entry(id)
    }

    pub fn list_entries(
        &self,
        folder_id: Option<&str>,
        search: Option<&str>,
    ) -> Result<Vec<Entry>, AppError> {
        let dek = self.dek()?;
        self.store
            .list_entries(folder_id, search)?
            .into_iter()
            .map(|row| Ok(self.decrypt_row(row, &dek)))
            .collect()
    }

    pub fn get_entry(&self, id: &str) -> Result<Option<Entry>, AppError> {
        let dek = self.dek()?;
        self.store
            .get_entry(id)?
            .map(|row| Ok(self.decrypt_row(row, &dek)))
            .transpose()
    }

    // ---- 文件夹透传（无敏感数据）----

    pub fn create_folder(&self, name: &str) -> Result<Folder, AppError> {
        self.store.create_folder(name)
    }

    pub fn list_folders(&self) -> Result<Vec<Folder>, AppError> {
        self.store.list_folders()
    }

    pub fn rename_folder(&self, id: &str, name: &str) -> Result<bool, AppError> {
        self.store.rename_folder(id, name)
    }

    pub fn delete_folder(&self, id: &str) -> Result<bool, AppError> {
        self.store.delete_folder(id)
    }

    // ---- 内部 ----

    fn decrypt_row(&self, row: EntryRow, dek: &SecretKey) -> Entry {
        let fields: Vec<EntryField> = crypto::open_field(dek, &row.id, &row.fields_ct)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let totp_secret = row
            .totp_ct
            .as_deref()
            .and_then(|ct| crypto::open_field(dek, &row.id, ct).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok());
        Entry {
            id: row.id,
            folder_id: row.folder_id,
            title: row.title,
            favorite: row.favorite,
            fields,
            totp_secret,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::test_kdf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vault-svc-{tag}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn service(tag: &str) -> VaultService {
        VaultService::open(&temp_dir(tag)).unwrap()
    }

    fn sample_fields() -> Vec<EntryField> {
        vec![
            EntryField {
                key: "password".into(),
                kind: crate::model::FieldKind::Password,
                value: "s3cret-VALUE-xyz".into(),
            },
            EntryField {
                key: "url".into(),
                kind: crate::model::FieldKind::Url,
                value: "https://github.com".into(),
            },
        ]
    }

    #[test]
    fn create_unlock_lock_cycle() {
        let v = service("cycle");
        assert_eq!(v.state(), VaultState::Uninitialized);
        v.create("master-pw", Some(test_kdf())).unwrap();
        assert_eq!(v.state(), VaultState::Unlocked);
        assert_eq!(v.header().unwrap().kdf.algo, "argon2id");

        v.lock().unwrap();
        assert_eq!(v.state(), VaultState::Locked);
        // 锁定后数据操作拒绝
        assert!(v.list_entries(None, None).is_err());

        v.unlock("master-pw").unwrap();
        assert_eq!(v.state(), VaultState::Unlocked);
        assert_eq!(v.lockout_remaining_secs(), 0);
    }

    #[test]
    fn five_wrong_attempts_trigger_cooldown() {
        let v = service("lockout");
        v.create("master-pw", Some(test_kdf())).unwrap();
        v.lock().unwrap();

        for i in 0..MAX_ATTEMPTS {
            // 每次尝试前不处于冷却
            assert_eq!(
                v.lockout_remaining_secs(),
                0,
                "第 {} 次尝试前不应处于冷却",
                i + 1
            );
            let _ = v.unlock("bad").unwrap_err();
        }
        // 第 5 次错误后进入冷却：正确密码也被拒
        assert!(v.lockout_remaining_secs() > 0, "冷却必须生效");
        assert!(v.unlock("master-pw").is_err());
        assert_eq!(v.state(), VaultState::Locked);
    }

    #[test]
    fn entry_crud_and_db_never_stores_plaintext() {
        let dir = temp_dir("plaintext");
        let v = VaultService::open(&dir).unwrap();
        v.create("master-pw", Some(test_kdf())).unwrap();

        let entry = v
            .add_entry(
                None,
                "GitHub",
                true,
                sample_fields(),
                Some("GEZDGNBVGY3TQOJQ".into()),
            )
            .unwrap();
        assert_eq!(entry.fields[0].value, "s3cret-VALUE-xyz");

        let listed = v.list_entries(None, Some("git")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].fields[1].value, "https://github.com");
        assert_eq!(listed[0].totp_secret.as_deref(), Some("GEZDGNBVGY3TQOJQ"));

        // 修改
        let mut e2 = listed[0].clone();
        e2.title = "GitHub 改".into();
        e2.fields[0].value = "new-pass-99".into();
        let updated = v.update_entry(e2).unwrap();
        assert_eq!(updated.fields[0].value, "new-pass-99");
        assert_eq!(
            v.get_entry(&updated.id).unwrap().unwrap().title,
            "GitHub 改"
        );

        // 删除
        assert!(v.delete_entry(&updated.id).unwrap());
        assert!(v.list_entries(None, None).unwrap().is_empty());

        // 加密落盘断言：库文件与 WAL 字节均不得出现明文密码 / TOTP 密钥
        let db = dir.join("db").join("vault.db");
        let mut blob = std::fs::read(&db).unwrap_or_default();
        blob.extend(std::fs::read(db.with_extension("db-wal")).unwrap_or_default());
        assert!(
            !blob.windows(15).any(|w| w == b"s3cret-VALUE-xyz"),
            "库文件不得含明文密码"
        );
        assert!(
            !blob.windows(8).any(|w| w == b"new-pass-99"),
            "库文件不得含明文密码"
        );
        assert!(
            !blob.windows(16).any(|w| w == b"GEZDGNBVGY3TQOJQ"),
            "库文件不得含明文 TOTP 密钥"
        );
    }

    #[test]
    fn change_password_via_service() {
        let v = service("chpw");
        v.create("old-pw", Some(test_kdf())).unwrap();
        v.change_master_password("old-pw", "new-pw").unwrap();
        v.lock().unwrap();
        assert!(v.unlock("old-pw").is_err(), "旧密码必须失效");
        v.unlock("new-pw").unwrap();
    }

    #[test]
    fn reopen_persists_locked_state() {
        let dir = temp_dir("reopen");
        {
            let v = VaultService::open(&dir).unwrap();
            v.create("master-pw", Some(test_kdf())).unwrap();
            v.add_entry(None, "Site", false, sample_fields(), None)
                .unwrap();
            v.lock().unwrap();
        }
        // 重新打开：meta 仍在，锁定态，解锁后数据完好
        let v2 = VaultService::open(&dir).unwrap();
        assert_eq!(v2.state(), VaultState::Locked);
        v2.unlock("master-pw").unwrap();
        assert_eq!(v2.list_entries(None, None).unwrap().len(), 1);
    }
}
