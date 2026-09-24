//! T-B6-5（09 §6.2）：SFTP 驱动 + TOFU fail-closed——协议无关红线的 seam 面。
//!
//! 红线（本文件是判据落点，改码前先读）：
//! - SSH 发起连接的字面量在本文件**恰出现一处**（批次判据 grep；探测与操作
//!   会话共用同一入口，TOFU 判定因此没有第二处可绕）。
//! - known_hosts 解析失败必须 `Err`——本文件禁绝"解析失败回落空表"式反序列化
//!   兜底（批次判据 grep 该三字常见形式对 `remote/` 目录恒零命中）。term 侧
//!   `KnownHosts::open` 的清空缺陷（09 §6.1 ⑬ 点名的反面教材）在此结构性不可
//!   复制：坏文件 ⇒ 启动即报 ⇒ 一条 SSH 连接都建立不了（fail-closed）。
//! - 首见主机键**不自动接受**：`Unknown` 臂唯一出路是指纹核对后的显式确认命令；
//!   变更臂把录制与实际两枚指纹都点名。错误文案禁"白名单"类含糊措辞——
//!   接受的是**这一枚指纹**，不是"这台主机"。
//! - 口令只进不出：本文件的凭据类型不 derive `Serialize`，`Debug` 只报在场；
//!   探测会话（`Connect` 臂）在 KEX 拿到主机键后即中止，凭据字节永不出网——
//!   未受信对端连"认证尝试"这一事实都不该拥有。
//! - 操作会话（List/Mkdir/…）连接时以 known_hosts 录制值过 host-core 唯一
//!   整键口径 `whole_key_agrees`，与守卫臂同一比较口径；守卫在分派口
//!   （`FileService::connect`）先行，驱动实例只有已受信档案才可达。
//!
//! 码表分工（T-B6-1 固定的 001..005，本行不扩码，理由登记于批次提交说明）：
//! 首见无记录 → `FILE_REMOTE_001`（记录缺失的字面语义）；键变更/凭据形状不合 →
//! `FILE_REMOTE_005`（字段级不符）；坏 known_hosts → 005（文件内容字段不可解析）。

#[cfg(test)]
use std::collections::HashMap;
use std::fmt;
use std::io::Seek as StdSeek;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use russh::client::{self, Handle};
use russh::ChannelMsg;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use zeroize::{Zeroize, Zeroizing};

use crate::error::{FileError, FILE_REMOTE_FIELD, FILE_REMOTE_MISSING};
use crate::profile::{AuthKind, RemoteProfile};
use crate::remote::webdav::join_remote_url;
use crate::remote::{range_plan, remote_block_on, remote_enter, AuthSecret, RemoteEntry};
use host_core::ports::Port as PortsPort;
use host_core::ports::Ports;

/// 探测/握手超时（与 term 同参；操作会话内各 RPC 依 inactivity 超时兜底）
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// 凭据形状（只进不出）
// ---------------------------------------------------------------------------

/// SSH 认证凭据（口令/口令短语进 [`Zeroizing`]，Display 面只报在场）
#[derive(Clone)]
pub(crate) enum SshAuthRpc {
    Password {
        user: String,
        password: Zeroizing<String>,
    },
    KeyFile {
        user: String,
        key_path: String,
        passphrase: Option<Zeroizing<String>>,
    },
}

impl fmt::Debug for SshAuthRpc {
    /// 只报在场与用户/路径指针，值零出现（口令长度都不报——那是泄露面）
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SshAuthRpc::Password { user, .. } => f
                .debug_struct("SshAuthRpc::Password")
                .field("user", user)
                .field("password", &"<present, redacted>")
                .finish(),
            SshAuthRpc::KeyFile {
                user,
                key_path,
                passphrase,
            } => f
                .debug_struct("SshAuthRpc::KeyFile")
                .field("user", user)
                .field("key_path", key_path)
                .field(
                    "passphrase",
                    &match passphrase {
                        Some(_) => "<present, redacted>",
                        None => "<absent>",
                    },
                )
                .finish(),
        }
    }
}

/// 由档案的 auth 指针 + 逐次送入的凭据组装 SSH 认证形状（唯一装配口）。
/// `AuthSecret::header`（Authorization 头形状）是 WebDAV 腿的凭据方言，
/// 喂到 SSH 面即拒——两型凭据各走各的口，禁"先收下再说"。
pub(crate) fn sftp_auth_for(
    profile: &RemoteProfile,
    secret: Option<AuthSecret>,
) -> Result<SshAuthRpc, FileError> {
    if let Some(s) = &secret {
        if s.header.is_some() {
            return Err(FileError::Remote {
                code: FILE_REMOTE_FIELD,
                msg: format!(
                    "档案 {} 是 SSH 面：不接受 Authorization 头形状的凭据（那是 WebDAV 腿的方言）",
                    profile.id
                ),
            });
        }
    }
    let password = secret.and_then(|s| s.password);
    match (&profile.auth, password) {
        (AuthKind::SshKey { key_path }, passphrase) => Ok(SshAuthRpc::KeyFile {
            user: profile.user.clone(),
            key_path: key_path.clone(),
            passphrase,
        }),
        (AuthKind::PromptEachTime | AuthKind::SessionPassword, Some(pw)) => {
            Ok(SshAuthRpc::Password {
                user: profile.user.clone(),
                password: pw,
            })
        }
        (AuthKind::VaultEntry { entry_id }, Some(pw)) => {
            // vault 条目只是指针（承重⑪）：解出的口令由宿主层经端口送入（T-B6-8
            // 装配），本函数只认"已解出的值"，不感知 vault 的任何状态
            let _ = entry_id; // 指针不出本函数：值到驱动即弃条目 id
            Ok(SshAuthRpc::Password {
                user: profile.user.clone(),
                password: pw,
            })
        }
        (AuthKind::VaultEntry { entry_id }, None) => Err(FileError::Remote {
            code: FILE_REMOTE_FIELD,
            msg: format!(
                "档案 {} 的 VaultEntry 指针（条目 {entry_id}）未随连接送入已解出的凭据：\
                 缺端口/未解锁/条目不存在三态由宿主层端口解析逐一点名（T-B6-8），\
                 本处**不回落**为交互式索取口令",
                profile.id
            ),
        }),
        (AuthKind::PromptEachTime | AuthKind::SessionPassword, None) => Err(FileError::Remote {
            code: FILE_REMOTE_FIELD,
            msg: format!(
                "档案 {} 需要逐次/会话口令，本次连接未送入凭据（不回落提示输口令——那会把决定权悄悄交给任意一端）",
                profile.id
            ),
        }),
        (AuthKind::Anonymous, _) => Err(FileError::Remote {
            code: FILE_REMOTE_FIELD,
            msg: format!("档案 {} 选了匿名档：SSH 面没有无凭据登录这一档（禁假兼容）", profile.id),
        }),
    }
}

// ---------------------------------------------------------------------------
// RPC seam（同步签名：worker 线程 block_on；async 只藏在 RusshBackend 内）
// ---------------------------------------------------------------------------

/// 对 seam 的一次请求。每调用一次独立建连（v1 无连接池，与 term 同形制，
/// 成本登记于 09 §6.2 本行）；`Download/Upload` 带断点偏移，由 checkpoint 算出。
#[derive(Clone)]
#[allow(dead_code)] // 预布线登记：`Connect` 的 user/auth 由测试替身分派消费（真腿
                    // 字段在 bound backend 自持）；`Stat/Download` 的构造腿属 T-B6-7 传输面（Fake 侧
                    // 与 ssh.rs 测试已走通）——禁为消警裁掉任务书签名形状的字面臂。
pub(crate) enum SshRpc {
    Connect {
        host: String,
        port: u16,
        user: String,
        auth: SshAuthRpc,
    },
    Exec {
        cmd: String,
    },
    List {
        path: String,
    },
    Stat {
        path: String,
    },
    Mkdir {
        path: String,
    },
    Remove {
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
    Download {
        path: String,
        dst: PathBuf,
        offset: u64,
    },
    Upload {
        path: String,
        src: PathBuf,
        offset: u64,
    },
    Close,
}

/// seam 的回执
pub(crate) enum SshReply {
    /// 探测臂的回执：主机键的算法名与指纹（整键描述符的原料，见
    /// [`server_key_descriptor`]）
    ServerKey {
        algo: String,
        fingerprint: String,
    },
    Entries(Vec<RemoteEntry>),
    Size(u64),
    Ok,
}

/// 协议腿的唯一出口：真实实现 [`RusshBackend`]，测试替身 `FakeSsh`。
/// 同步 trait——调用方在 worker 线程，russh 的 async 由内部专用 runtime 桥接。
pub(crate) trait SshBackend: Send + Sync {
    fn open(&self, req: &SshRpc) -> Result<SshReply, FileError>;
}

/// 整键描述符（唯一组形口）：`"{algo} {SHA256:base64}"`。**任何一侧都不得
/// 只比 base64 段**——算法名不同而 base64 巧合相等就是两把不同的钥匙
/// （`tofu_trusted_matchesWholeKeyNotJustBase64` 立此存照）。
pub(crate) fn server_key_descriptor(algo: &str, fingerprint: &str) -> String {
    format!("{algo} {fingerprint}")
}

// ---------------------------------------------------------------------------
// known_hosts（T-B7-1 单源：term 与 file 共用一份信任文件，存储体与三态
// 裁决上移 host-core `ssh_trust`——B6 登记的"两张表互不共享"缺口在此清偿；
// 两旧表的首见合并也在 host-core，本文件只剩 file 侧门面与错误映射）
// ---------------------------------------------------------------------------

use host_core::ssh_trust::{
    shared_path as shared_trust_path, whole_key_agrees, KnownHostsStore, TrustError,
};

/// file 域对共享信任文件的进程内视图（存储体在 host-core）。
pub(crate) struct KnownHostsFile {
    store: KnownHostsStore,
}

impl std::fmt::Debug for KnownHostsFile {
    /// 表内容只有公开主机键描述符（无凭据），但 Debug 面仍只报规模——
    /// 少一张可被日志整版抄走的表
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KnownHostsFile")
            .field("path", &self.store.path())
            .field("records", &self.store.entries().len())
            .finish()
    }
}

/// 共享存储面错误 → FileError：损坏=005 字段级不符（B6 既有口径），
/// IO=001 传输失败；消息体逐字取自 host-core（点名路径 + fail-closed 语义）
fn trust_err(e: TrustError) -> FileError {
    match e {
        TrustError::Corrupt { .. } => FileError::Remote {
            code: FILE_REMOTE_FIELD,
            msg: e.to_string(),
        },
        TrustError::Io(_) => FileError::Remote {
            code: FILE_REMOTE_MISSING,
            msg: e.to_string(),
        },
    }
}

impl KnownHostsFile {
    pub(crate) fn get(&self, host: &str, port: u16) -> Option<String> {
        self.store.get(host, port)
    }

    /// 用户明示接受后才调本口（`file_remote_fingerprint_ack` 的唯一落点），
    /// 整键逐字写入共享文件（读盘-改-原子 tmp+rename 在 host-core mutate）
    pub(crate) fn accept(&self, host: &str, port: u16, descriptor: &str) -> Result<(), FileError> {
        self.store.accept(host, port, descriptor).map_err(trust_err)
    }

    /// 全部记录（规范键 → 描述符；管理页属 B7 §7，本行只保证自救口存在）
    #[allow(dead_code)] // B7 管理页消费；本行由 ssh.rs 测试臂走通（禁裁）
    pub(crate) fn entries(&self) -> Vec<(String, String)> {
        self.store.entries()
    }
}

/// 按确切路径加载视图（**fail-closed**，测试夹具位）：文件不存在 = 尚无记录
/// （Ok 空表）；存在而解析失败 = `Err` 且消息点名文件——"坏文件 ⇒ 接受任意
/// 主机"是 fail-closed 立论的反面，解析兜底口在本函数结构性缺席。
/// 生产路径走 [`load_shared_known_hosts`]（单源）。
#[cfg(test)]
pub(crate) fn load_known_hosts(path: &Path) -> Result<Arc<KnownHostsFile>, FileError> {
    Ok(Arc::new(KnownHostsFile {
        store: KnownHostsStore::open_at(path).map_err(trust_err)?,
    }))
}

/// T-B7-1 共享信任文件装载（**单一事实源**）：appData 根下 `ssh/` 目录的
/// known_hosts.json——路径构造与两旧表合并全在 host-core `ssh_trust`，
/// 本行是 file 域源码里该文件名的唯一字面出现（单源字面判据的落点）。
pub(crate) fn load_shared_known_hosts(
    app_data_root: &Path,
) -> Result<Arc<KnownHostsFile>, FileError> {
    Ok(Arc::new(KnownHostsFile {
        store: KnownHostsStore::shared(app_data_root).map_err(trust_err)?,
    }))
}

/// 单源路径（与 term 域恒一等值断言面，`fileAndTerm_shareSingleStore` 消费）
pub fn shared_known_hosts_path(app_data_root: &Path) -> PathBuf {
    shared_trust_path(app_data_root)
}

/// file 域对共享信任文件的三态裁决（单源端到端正证面；每次独立读盘态，
/// 不依赖运行中的 service 实例）
pub fn check_shared_host_key(
    app_data_root: &Path,
    host: &str,
    port: u16,
    actual: &str,
) -> Result<HostKeyDecision, FileError> {
    let store = load_shared_known_hosts(app_data_root)?;
    Ok(store.store.decide(host, port, actual))
}

/// TOFU 三态裁决（上移 host-core，两域共读一型——B6 `Resumable` 上移同谱）
pub use host_core::ssh_trust::HostKeyDecision;

/// 三臂裁决（host-core `decide` 的 file 侧保形包装）：无记录 `Unknown`；
/// 整键口径放行者是 host-core 唯一比较口 `whole_key_agrees`——两枚整键描述符
/// 逐字相等才 `Trusted`（裸旧记录命中即 Trusted 并升级落盘），不等即
/// `Changed`（两枚指纹都进决策体，点名义务由 [`tofu_guard`] 执行）。返回
/// `Result` 是为任务书签名形状保真——本函数自身无失败路径，坏文件的失败在加载臂。
pub(crate) fn check_server_key(
    store: &KnownHostsFile,
    host: &str,
    port: u16,
    got: &str,
) -> Result<HostKeyDecision, FileError> {
    Ok(store.store.decide(host, port, got))
}

/// 守卫（分派口与操作会话共用的裁决出口）：`Unknown`/`Changed` 都拒连，
/// 消息把指纹逐字点名（永不归一化——UI 展示的就是盘上要比对的那串）。
/// 文案红线：不出现"白名单/仍然连接"式含糊出路。
pub(crate) fn tofu_guard(
    backend: &dyn SshBackend,
    store: &KnownHostsFile,
    host: &str,
    port: u16,
    user: &str,
    auth: &SshAuthRpc,
) -> Result<(), FileError> {
    let probe = backend.open(&SshRpc::Connect {
        host: host.to_owned(),
        port,
        user: user.to_owned(),
        auth: auth.clone(),
    })?;
    let (algo, fingerprint) = match probe {
        SshReply::ServerKey { algo, fingerprint } => (algo, fingerprint),
        other => {
            return Err(FileError::BadState(format!(
                "协议内部矛盾：探测臂必须回 ServerKey，实得回执档位 {:?}",
                std::mem::discriminant(&other)
            )))
        }
    };
    let got = server_key_descriptor(&algo, &fingerprint);
    match check_server_key(store, host, port, &got)? {
        HostKeyDecision::Trusted { .. } => Ok(()),
        HostKeyDecision::Unknown { fingerprint } => Err(FileError::Remote {
            code: FILE_REMOTE_MISSING,
            msg: format!(
                "主机 {host}:{port} 首次连接，服务器密钥不在记录，连接已拒。指纹（逐字）：{fingerprint}。\
                 请经带外渠道与服务器侧核对这枚指纹完全一致后，经指纹确认命令明示记录它；\
                 本客户端没有隐式自纳这回事"
            ),
        }),
        HostKeyDecision::Changed { recorded, actual } => Err(FileError::Remote {
            code: FILE_REMOTE_FIELD,
            msg: format!(
                "主机 {host}:{port} 密钥已变更，连接已拒。记录（逐字）：{recorded}。实收（逐字）：{actual}。\
                 若确认主机已重建，先删除该主机的记录再发起连接；不存在带着旧记录继续连的出路"
            ),
        }),
    }
}

// ---------------------------------------------------------------------------
// 驱动
// ---------------------------------------------------------------------------

/// SFTP 驱动：档案 + 已装配凭据 + 已绑定的协议腿 + known_hosts 视图。
/// 构造即假定 TOFU 已在分派口过检（本类型只有 `FileService::connect` 可达）；
/// 操作会话在 [`RusshBackend`] 内还会以录制值二次把关（两道门同一比较口径）。
pub(crate) struct SftpDriver {
    profile: RemoteProfile,
    #[allow(dead_code)] // 凭据随连接事实驻留（真腿认证字段由 bound backend 自持
    // 读取）；本枚是驱动侧的在场记录，禁为消警裁掉
    auth: SshAuthRpc,
    backend: Arc<dyn SshBackend>,
    #[allow(dead_code)] // 操作会话二次把关属 RusshBackend 字段；测试替身构造时常驻
    store: Arc<KnownHostsFile>,
}

impl SftpDriver {
    pub(crate) fn new(
        profile: RemoteProfile,
        auth: SshAuthRpc,
        backend: Arc<dyn SshBackend>,
        store: Arc<KnownHostsFile>,
    ) -> Self {
        Self {
            profile,
            auth,
            backend,
            store,
        }
    }

    pub(crate) fn profile(&self) -> &RemoteProfile {
        &self.profile
    }

    pub(crate) fn driver_label(&self) -> String {
        format!("SFTP {}@{}", self.profile.user, self.profile.host)
    }

    pub(crate) fn base_path(&self) -> String {
        self.profile.base_path.clone()
    }

    fn rpc(&self, req: SshRpc) -> Result<SshReply, FileError> {
        self.backend.open(&req)
    }

    pub(crate) fn list_entries(&self, path: &str) -> Result<Vec<RemoteEntry>, FileError> {
        match self.rpc(SshRpc::List {
            path: path.to_owned(),
        })? {
            SshReply::Entries(v) => Ok(v),
            _ => Err(FileError::BadState(
                "协议内部矛盾：List 臂必须回 Entries".into(),
            )),
        }
    }

    pub(crate) fn mkdir_remote(&self, path: &str) -> Result<(), FileError> {
        self.rpc(SshRpc::Mkdir {
            path: path.to_owned(),
        })?;
        Ok(())
    }

    pub(crate) fn remove_remote(&self, path: &str) -> Result<(), FileError> {
        self.rpc(SshRpc::Remove {
            path: path.to_owned(),
        })?;
        Ok(())
    }

    pub(crate) fn rename_remote(&self, from: &str, to: &str) -> Result<(), FileError> {
        self.rpc(SshRpc::Rename {
            from: from.to_owned(),
            to: to.to_owned(),
        })?;
        Ok(())
    }

    /// 远端文件总大小（`download_to` 的算式前置；队列消费接线归 T-B6-7）
    #[allow(dead_code)]
    pub(crate) fn stat_size(&self, path: &str) -> Result<u64, FileError> {
        match self.rpc(SshRpc::Stat {
            path: path.to_owned(),
        })? {
            SshReply::Size(n) => Ok(n),
            _ => Err(FileError::BadState(
                "协议内部矛盾：Stat 臂必须回 Size".into(),
            )),
        }
    }

    /// 断点续取（唯一算式：先 `Stat` 拿总大小，[`range_plan`] 裁还有无缺口，
    /// 缺口起点即 `Download` 的 offset——checkpoint 真值不靠猜）。
    /// 返回**累计已落盘字节**（have + 本次新增），供队列 checkpoint 直存。
    #[allow(dead_code)] // 队列执行器的 SFTP 下载腿接线归 T-B6-7；本行交付算式
                        // 并由 `sftpResume_offsetsMatchPlan` 走通（禁为消警裁掉）
    pub(crate) fn download_to(
        &self,
        remote_path: &str,
        dst: &Path,
        have: u64,
    ) -> Result<u64, FileError> {
        let total = self.stat_size(remote_path)?;
        let Some((start, _end)) = range_plan(total, have) else {
            // 无缺口：不再发起传输（防"显示已续传实际重跑"），checkpoint 原值回传
            return Ok(have);
        };
        match self.rpc(SshRpc::Download {
            path: remote_path.to_owned(),
            dst: dst.to_path_buf(),
            offset: start,
        })? {
            SshReply::Size(written) => Ok(start + written),
            _ => Err(FileError::BadState(
                "协议内部矛盾：Download 臂必须回 Size".into(),
            )),
        }
    }

    /// 上传腿（断点从 checkpoint 偏移起）：队列执行器的远端写面接线归 T-B6-7，
    /// 本行交付 seam 形状并如实登记未消费
    #[allow(dead_code)]
    pub(crate) fn upload_from(
        &self,
        remote_path: &str,
        src: &Path,
        have: u64,
    ) -> Result<u64, FileError> {
        match self.rpc(SshRpc::Upload {
            path: remote_path.to_owned(),
            src: src.to_path_buf(),
            offset: have,
        })? {
            SshReply::Size(written) => Ok(have + written),
            _ => Err(FileError::BadState(
                "协议内部矛盾：Upload 臂必须回 Size".into(),
            )),
        }
    }

    /// 连通性探测的显式口（面板"测试连接"按钮的落点归 T-B6-10；v1 语义 =
    /// 会话建立即成功，stdout 不回收）
    #[allow(dead_code)]
    pub(crate) fn probe_exec(&self, cmd: &str) -> Result<(), FileError> {
        self.rpc(SshRpc::Exec {
            cmd: cmd.to_owned(),
        })?;
        Ok(())
    }

    // ---- T-B6-11 字节腿（队列远端执行器经 trait 消费）----

    /// spool 读腿：RPC Download 整文件落盘（协议腿粒度是文件），读柄内部
    /// seek 到 `offset` 才交出——trait 的"流恰从 offset 起"契约由此兑现；
    /// offset 超盘上实长即 Err 点名（断点失真禁静默）。
    pub(crate) fn read_spool(
        &self,
        remote_path: &str,
        offset: u64,
    ) -> Result<super::SpoolReader, FileError> {
        use std::io::Seek;
        let spool_path = super::reserve_spool_path("sftp");
        let total = self.rpc(SshRpc::Download {
            path: remote_path.to_owned(),
            dst: spool_path.clone(),
            offset: 0,
        })?;
        let SshReply::Size(written) = total else {
            super::SpoolReader::discard(&spool_path);
            return Err(FileError::BadState(
                "协议内部矛盾：Download 臂必须回 Size".into(),
            ));
        };
        let reader = match super::SpoolReader::open(spool_path.clone()) {
            Ok(r) => r,
            Err(e) => {
                super::SpoolReader::discard(&spool_path);
                return Err(e);
            }
        };
        let mut reader = reader;
        let len = {
            use std::io::SeekFrom;
            let pos = reader.seek(SeekFrom::End(0)).map_err(FileError::Io)?;
            reader
                .seek(SeekFrom::Start(offset))
                .map_err(FileError::Io)?;
            pos
        };
        if offset > len {
            drop(reader);
            super::SpoolReader::discard(&spool_path);
            return Err(FileError::BadState(format!(
                "断点失真：声称 offset={offset} 但 spool 只有 {len} 字节（远端实收 {written}）"
            )));
        }
        Ok(reader)
    }

    /// spool 写腿：字节先攒盘，finish 时经 RPC Upload 整提交并核对回执
    /// （泵完字节 ≠ 传成——回执 Size 与盘上实长不等即 Err）。
    pub(crate) fn put_writer(
        &self,
        remote_path: &str,
    ) -> Result<Box<dyn host_core::storage::WriteCommit>, FileError> {
        Ok(Box::new(SftpPutWriter {
            spool: super::Spool::create("sftp-put")?,
            remote: remote_path.to_owned(),
            backend: self.backend.clone(),
        }))
    }
}

struct SftpPutWriter {
    spool: super::Spool,
    remote: String,
    backend: Arc<dyn SshBackend>,
}

impl std::io::Write for SftpPutWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(self.spool.file_mut(), buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(self.spool.file_mut())
    }
}

impl host_core::storage::WriteCommit for SftpPutWriter {
    fn finish(mut self: Box<Self>) -> Result<(), host_core::error::AppError> {
        let want = self.spool.disk_len()?;
        let path = self.spool.publish()?;
        let rpc = self.backend.open(&SshRpc::Upload {
            path: self.remote.clone(),
            src: path.clone(),
            offset: 0,
        });
        // 提交后 spool 由本臂终删（成功失败都不留残）
        let cleanup = || {
            let _ = std::fs::remove_file(&path);
        };
        match rpc {
            Ok(SshReply::Size(written)) => {
                cleanup();
                if written != want {
                    return Err(host_core::error::AppError::from(FileError::BadState(
                        format!("SFTP 上传回执失真：盘上 {want} 字节而对端收报 {written}"),
                    )));
                }
                Ok(())
            }
            Ok(_) => {
                cleanup();
                Err(host_core::error::AppError::from(FileError::BadState(
                    "协议内部矛盾：Upload 臂必须回 Size".into(),
                )))
            }
            Err(e) => {
                cleanup();
                Err(e.into())
            }
        }
    }
}

impl Drop for SftpDriver {
    fn drop(&mut self) {
        // 每调用独立建连（无池） ⇒ Close 对 RusshBackend 是名义动作；
        // 仍显式走一遍，让 seam 的退役臂有真实消费者而非纸面枚举
        let _ = self.rpc(SshRpc::Close);
    }
}

// ---------------------------------------------------------------------------
// 真协议腿：russh 实现（0.46，与 term-core 同锁内版本；两份 SSH 栈的重复
// 成本按承重⑫ 登记——为共享一份栈而跨 crate 抽层会牵 term 的会话面，属 §6.3）
// ---------------------------------------------------------------------------

/// russh Handler：探测臂 `expect=None` 只捕获密钥并中止 KEX（凭据不出网）；
/// 操作臂以录制值为预期，过 host-core 唯一整键口径 `whole_key_agrees`
/// （两道门同一比较口径，裸旧记录臂与守卫侧裁决一致）。
struct FileSshHandler {
    expect: Option<String>,
    captured: Arc<parking_lot::Mutex<Option<(String, String)>>>,
}

#[async_trait]
impl client::Handler for FileSshHandler {
    type Error = FileError;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::key::PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        let algo = server_public_key.name().to_owned();
        let fp = server_public_key.fingerprint();
        *self.captured.lock() = Some((algo.clone(), fp.clone()));
        let desc = server_key_descriptor(&algo, &fp);
        Ok(self
            .expect
            .as_deref()
            .is_some_and(|want| whole_key_agrees(want, &desc)))
    }
}

impl From<russh::Error> for FileError {
    fn from(e: russh::Error) -> Self {
        FileError::Remote {
            code: FILE_REMOTE_MISSING,
            msg: format!("SSH 连接失败: {e}"),
        }
    }
}

fn sftp_err(what: &str, e: impl std::fmt::Display) -> FileError {
    FileError::Remote {
        code: FILE_REMOTE_MISSING,
        msg: format!("SFTP {what}失败: {e}"),
    }
}

/// 唯一发起连接口（批次判据 grep：connect 字面量本文件恰一处的落点）。
/// 探测臂与操作臂共用——超时、Config、Handler 形状因此不存在第二套。
async fn connect_russh(
    host: &str,
    port: u16,
    expect: Option<String>,
    captured: Arc<parking_lot::Mutex<Option<(String, String)>>>,
) -> Result<Handle<FileSshHandler>, FileError> {
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(30)),
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let handler = FileSshHandler { expect, captured };
    let connect = tokio::time::timeout(
        CONNECT_TIMEOUT,
        russh::client::connect(config, (host, port), handler),
    )
    .await
    .map_err(|_| FileError::Remote {
        code: FILE_REMOTE_MISSING,
        msg: format!("连接 {host}:{port} 超时（{}s）", CONNECT_TIMEOUT.as_secs()),
    })?;
    connect
}

async fn auth_russh(
    handle: &mut Handle<FileSshHandler>,
    user: &str,
    auth: &SshAuthRpc,
) -> Result<(), FileError> {
    let ok = match auth {
        SshAuthRpc::Password { password, .. } => {
            handle
                .authenticate_password(user, password.as_str())
                .await?
        }
        SshAuthRpc::KeyFile {
            key_path,
            passphrase,
            ..
        } => {
            let mut buf = std::fs::read(key_path)?;
            let key = match russh::keys::decode_openssh(
                buf.as_slice(),
                passphrase.as_deref().map(|s| s.to_string()).as_deref(),
            ) {
                Ok(k) => k,
                Err(e) => {
                    buf.zeroize();
                    return Err(FileError::Remote {
                        code: FILE_REMOTE_MISSING,
                        msg: format!("私钥解析失败（口令错误？）: {e}"),
                    });
                }
            };
            buf.zeroize();
            handle.authenticate_publickey(user, Arc::new(key)).await?
        }
    };
    if ok {
        Ok(())
    } else {
        Err(FileError::Remote {
            code: FILE_REMOTE_MISSING,
            msg: "服务器拒绝认证凭据".into(),
        })
    }
}

async fn open_sftp_session(
    handle: &mut Handle<FileSshHandler>,
) -> Result<russh_sftp::client::SftpSession, FileError> {
    let channel = handle.channel_open_session().await?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| sftp_err("subsystem 请求", e))?;
    russh_sftp::client::SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| sftp_err("会话建立", e))
}

/// 真协议腿：构造时绑定档案与凭据（操作臂每次调用独立建连；`Connect` 臂
/// 只做探测——见 [`tofu_guard`] 的调用序）。
pub(crate) struct RusshBackend {
    host: String,
    port: u16,
    user: String,
    auth: SshAuthRpc,
    store: Arc<KnownHostsFile>,
}

impl RusshBackend {
    pub(crate) fn bound(
        profile: &RemoteProfile,
        auth: SshAuthRpc,
        store: Arc<KnownHostsFile>,
    ) -> Self {
        Self {
            host: profile.host.clone(),
            port: profile.port,
            user: profile.user.clone(),
            auth,
            store,
        }
    }

    /// 操作会话的统一前置：先以录制值为预期建 KEX（未过检即 russh 层断连），
    /// 再认证。守卫在分派口第一道，这里是第二道——中间若有人换了盘上记录，
    /// 操作会话立刻拒，而非沿用旧信任。
    async fn ops_handle(&self) -> Result<Handle<FileSshHandler>, FileError> {
        let want = self.store.get(&self.host, self.port).ok_or_else(|| {
            FileError::Remote {
                code: FILE_REMOTE_MISSING,
                msg: format!(
                    "主机 {}:{} 无密钥记录，操作会话拒绝（先完成首次连接的指纹确认流程，不存在隐式自纳）",
                    self.host, self.port
                ),
            }
        })?;
        let captured = Arc::new(parking_lot::Mutex::new(None));
        let mut handle = connect_russh(&self.host, self.port, Some(want), captured).await?;
        auth_russh(&mut handle, &self.user, &self.auth).await?;
        Ok(handle)
    }

    async fn probe(&self, host: &str, port: u16) -> Result<SshReply, FileError> {
        let captured: Arc<parking_lot::Mutex<Option<(String, String)>>> =
            Arc::new(parking_lot::Mutex::new(None));
        // expect=None ⇒ check_server_key 恒放行=false ⇒ KEX 在拿到密钥后即中止
        let err = connect_russh(host, port, None, captured.clone())
            .await
            .err();
        if let Some((algo, fingerprint)) = captured.lock().clone() {
            return Ok(SshReply::ServerKey { algo, fingerprint });
        }
        Err(err.unwrap_or_else(|| {
            FileError::BadState("探测会话未取到服务器密钥却报告成功（协议层矛盾）".into())
        }))
    }

    async fn dispatch(&self, req: SshRpc) -> Result<SshReply, FileError> {
        match req {
            SshRpc::Connect { host, port, .. } => self.probe(&host, port).await,
            SshRpc::Close => {
                // 每调用独立建连 ⇒ 此处本无长连接可关；名义回执，退役路径真实走过
                Ok(SshReply::Ok)
            }
            SshRpc::Exec { cmd } => {
                let handle = self.ops_handle().await?;
                let channel = handle.channel_open_session().await?;
                channel
                    .exec(true, cmd.as_str())
                    .await
                    .map_err(|e| FileError::Remote {
                        code: FILE_REMOTE_MISSING,
                        msg: format!("Exec 请求失败: {e}"),
                    })?;
                let mut channel = channel;
                while let Some(msg) = channel.wait().await {
                    match msg {
                        ChannelMsg::ExitStatus { .. } | ChannelMsg::Eof | ChannelMsg::Close => {
                            break
                        }
                        _ => {}
                    }
                }
                Ok(SshReply::Ok)
            }
            SshRpc::List { path } => {
                let mut handle = self.ops_handle().await?;
                let sftp = open_sftp_session(&mut handle).await?;
                let dir = sftp
                    .read_dir(&path)
                    .await
                    .map_err(|e| sftp_err("列目录", e))?;
                let mut out: Vec<RemoteEntry> = Vec::new();
                for e in dir {
                    let name = e.file_name();
                    if name == "." || name == ".." {
                        continue;
                    }
                    let m = e.metadata();
                    out.push(RemoteEntry {
                        name: name.clone(),
                        path: join_remote_url(&path, &name),
                        is_dir: m.is_dir(),
                        size: m.size.unwrap_or(0),
                        // 无事实源就无时间：mtime 缺失回 0，绝不回当前时刻
                        modified_ms: m.mtime.map_or(0, |s| s as i64 * 1000),
                    });
                }
                Ok(SshReply::Entries(out))
            }
            SshRpc::Stat { path } => {
                let mut handle = self.ops_handle().await?;
                let sftp = open_sftp_session(&mut handle).await?;
                let md = sftp
                    .metadata(&path)
                    .await
                    .map_err(|e| sftp_err("取元数据", e))?;
                Ok(SshReply::Size(md.size.unwrap_or(0)))
            }
            SshRpc::Mkdir { path } => {
                let mut handle = self.ops_handle().await?;
                let sftp = open_sftp_session(&mut handle).await?;
                sftp.create_dir(&path)
                    .await
                    .map_err(|e| sftp_err("建目录", e))?;
                Ok(SshReply::Ok)
            }
            SshRpc::Remove { path } => {
                let mut handle = self.ops_handle().await?;
                let sftp = open_sftp_session(&mut handle).await?;
                match sftp.remove_file(&path).await {
                    Ok(()) => Ok(SshReply::Ok),
                    Err(file_err) => match sftp.remove_dir(&path).await {
                        Ok(()) => Ok(SshReply::Ok),
                        Err(dir_err) => Err(sftp_err(
                            "删除",
                            format!("文件路 {file_err}；目录路 {dir_err}"),
                        )),
                    },
                }
            }
            SshRpc::Rename { from, to } => {
                let mut handle = self.ops_handle().await?;
                let sftp = open_sftp_session(&mut handle).await?;
                sftp.rename(&from, &to)
                    .await
                    .map_err(|e| sftp_err("重命名", e))?;
                Ok(SshReply::Ok)
            }
            SshRpc::Download { path, dst, offset } => {
                let mut handle = self.ops_handle().await?;
                let sftp = open_sftp_session(&mut handle).await?;
                let mut rf = sftp
                    .open(&path)
                    .await
                    .map_err(|e| sftp_err("打开远程文件", e))?;
                if offset > 0 {
                    rf.seek(tokio::io::SeekFrom::Start(offset))
                        .await
                        .map_err(|e| sftp_err("定位远程文件", e))?;
                }
                let mut lf = if offset == 0 {
                    std::fs::File::create(&dst)?
                } else {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(&dst)
                        .map_err(|e| FileError::Remote {
                            code: FILE_REMOTE_MISSING,
                            msg: format!(
                                "断点失真：目标 {} 不可续写（声称已有 {offset} 字节）: {e}",
                                dst.display()
                            ),
                        })?
                };
                use std::io::SeekFrom;
                lf.seek(SeekFrom::Start(offset))?;
                let mut buf = vec![0u8; 64 * 1024];
                let mut written: u64 = 0;
                loop {
                    let n = rf.read(&mut buf).await.map_err(|e| {
                        sftp_err(
                            "读取",
                            format!("{e}（已续写 {written} 字节，checkpoint 可续）"),
                        )
                    })?;
                    if n == 0 {
                        break;
                    }
                    std::io::Write::write_all(&mut lf, &buf[..n])?;
                    written += n as u64;
                }
                std::io::Write::flush(&mut lf)?;
                Ok(SshReply::Size(written))
            }
            SshRpc::Upload { path, src, offset } => {
                let mut handle = self.ops_handle().await?;
                let sftp = open_sftp_session(&mut handle).await?;
                let mut lf = std::fs::File::open(&src)?;
                use std::io::SeekFrom;
                lf.seek(SeekFrom::Start(offset))?;
                let mut wf = if offset == 0 {
                    sftp.create(&path)
                        .await
                        .map_err(|e| sftp_err("创建远程文件", e))?
                } else {
                    let f = sftp
                        .open(&path)
                        .await
                        .map_err(|e| sftp_err("打开远程文件", e))?;
                    let mut f = f;
                    f.seek(tokio::io::SeekFrom::Start(offset))
                        .await
                        .map_err(|e| sftp_err("定位远程文件", e))?;
                    f
                };
                let mut buf = vec![0u8; 64 * 1024];
                let mut sent: u64 = 0;
                loop {
                    let n = std::io::Read::read(&mut lf, &mut buf)?;
                    if n == 0 {
                        break;
                    }
                    wf.write_all(&buf[..n])
                        .await
                        .map_err(|e| sftp_err("写入", format!("{e}（已送 {sent} 字节）")))?;
                    sent += n as u64;
                }
                wf.flush().await.map_err(|e| sftp_err("冲刷", e))?;
                Ok(SshReply::Size(sent))
            }
        }
    }
}

impl SshBackend for RusshBackend {
    fn open(&self, req: &SshRpc) -> Result<SshReply, FileError> {
        let this = self.clone_shallow();
        let req = req.clone();
        remote_block_on(async move { this.dispatch(req).await })
    }
}

impl RusshBackend {
    fn clone_shallow(&self) -> Self {
        Self {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            auth: self.auth.clone(),
            store: self.store.clone(),
        }
    }
}

/// 用 `remote_enter` 保证 Client/runtime 上下文构建的入口在本 crate 的专用
/// runtime 上（与 WebDAV 腿同一桥）——本函数存在只为让 remote_enter 的
/// 既有约束（"供驱动在 runtime 上下文内构建"）对 SSH 腿同样成文
#[allow(dead_code)] // 预留给 T-B6-7 队列执行器接线时的上下文自检
fn assert_runtime_enterable() {
    let guard = remote_enter();
    drop(guard);
}

// ---------------------------------------------------------------------------
// vault 指针的端口面（承重⑪：file 域只有条目 id，密文永不过这条边）
// ---------------------------------------------------------------------------

/// vault 口令读取端口：src-tauri 侧以真 `VaultService` 实现并注册进 [`Ports`]
/// （file-core **不 import vault-core**——这条 trait 边是零新依赖的缝合口，
/// 装配与三态归因归 T-B6-8，本行交付端口形状与缺端口态）。
pub trait VaultSecretPort: PortsPort {
    /// 按条目 id 取 `kind==password` 字段的值（用完即 Zeroizing 退役；
    /// 实现侧的失败必须把"未解锁/条目不存在"两态逐一点名，禁回落提示输口令）
    fn read_secret(&self, entry_id: &str) -> Result<Zeroizing<String>, FileError>;
}

/// 经端口解析 VaultEntry 指针（三态归因的第一态：缺端口在此点名；
/// 未解锁/条目不存在由端口实现点名——三态消息两两不同串的裁决面在
/// `vaultEntryPointer_resolvesViaPortsAndErrsNamedByState`，src-tauri 侧真
/// `VaultService` 上跑）。
pub fn vault_secret_via_ports(
    ports: &Ports,
    entry_id: &str,
) -> Result<Zeroizing<String>, FileError> {
    let port = ports.get::<dyn VaultSecretPort>().ok_or_else(|| {
        FileError::Remote {
            code: FILE_REMOTE_FIELD,
            msg: format!(
                "VaultSecretPort 未注册：档案体系引用了 vault 条目 {entry_id}，但本进程没有装配 vault 读取端口（缺端口态——与未解锁/条目不存在两态分列，禁混因）"
            ),
        }
    })?;
    port.read_secret(entry_id)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

/// 测试替身：记录每次 RPC 名并返回成功臂（协议层红线全部在这层验证，
/// 真实 SSH 冒烟属人工——批次尾登记，不假称绿）
#[cfg(test)]
pub(crate) struct FakeSsh {
    pub calls: Arc<parking_lot::Mutex<Vec<String>>>,
    /// 服务器"实际"密钥描述符（"algo SHA256:base64" 两字段，探测臂拆开回）
    pub descriptor: String,
    pub listing: Vec<RemoteEntry>,
    /// 远端文件夹具（Stat/Download 消费）
    pub files: HashMap<String, Vec<u8>>,
}

#[cfg(test)]
impl FakeSsh {
    pub(crate) fn new(descriptor: &str) -> Self {
        Self {
            calls: Arc::new(parking_lot::Mutex::new(Vec::new())),
            descriptor: descriptor.to_owned(),
            listing: Vec::new(),
            files: HashMap::new(),
        }
    }

    pub(crate) fn with_listing(mut self, entries: Vec<RemoteEntry>) -> Self {
        self.listing = entries;
        self
    }

    pub(crate) fn with_file(mut self, path: &str, bytes: &[u8]) -> Self {
        self.files.insert(path.to_owned(), bytes.to_vec());
        self
    }

    fn record(&self, name: &str) {
        self.calls.lock().push(name.to_owned());
    }
}

#[cfg(test)]
impl SshBackend for FakeSsh {
    fn open(&self, req: &SshRpc) -> Result<SshReply, FileError> {
        match req {
            SshRpc::Connect { .. } => {
                self.record("Connect");
                let (algo, fp) = self
                    .descriptor
                    .split_once(' ')
                    .unwrap_or((self.descriptor.as_str(), ""));
                Ok(SshReply::ServerKey {
                    algo: algo.to_owned(),
                    fingerprint: fp.to_owned(),
                })
            }
            SshRpc::Exec { .. } => {
                self.record("Exec");
                Ok(SshReply::Ok)
            }
            SshRpc::List { .. } => {
                self.record("List");
                Ok(SshReply::Entries(self.listing.clone()))
            }
            SshRpc::Stat { path } => {
                self.record("Stat");
                let f = self.files.get(path).ok_or_else(|| FileError::Remote {
                    code: FILE_REMOTE_MISSING,
                    msg: format!("桩夹具无此文件: {path}"),
                })?;
                Ok(SshReply::Size(f.len() as u64))
            }
            SshRpc::Mkdir { .. } => {
                self.record("Mkdir");
                Ok(SshReply::Ok)
            }
            SshRpc::Remove { .. } => {
                self.record("Remove");
                Ok(SshReply::Ok)
            }
            SshRpc::Rename { .. } => {
                self.record("Rename");
                Ok(SshReply::Ok)
            }
            SshRpc::Download { path, dst, offset } => {
                self.record(&format!("Download@{offset}"));
                let f = self.files.get(path).ok_or_else(|| FileError::Remote {
                    code: FILE_REMOTE_MISSING,
                    msg: format!("桩夹具无此文件: {path}"),
                })?;
                if *offset as usize > f.len() {
                    return Err(FileError::BadState(format!(
                        "桩夹具越界：offset {} > len {}",
                        offset,
                        f.len()
                    )));
                }
                let rest = &f[*offset as usize..];
                let mut lf = if *offset == 0 {
                    std::fs::File::create(dst)?
                } else {
                    std::fs::OpenOptions::new().write(true).open(dst)?
                };
                use std::io::{Seek, SeekFrom, Write};
                lf.seek(SeekFrom::Start(*offset))?;
                lf.write_all(rest)?;
                Ok(SshReply::Size(rest.len() as u64))
            }
            SshRpc::Upload { path, src, offset } => {
                self.record(&format!("Upload@{offset}"));
                let data = std::fs::read(src)?;
                let skip = *offset as usize;
                if skip > data.len() {
                    return Err(FileError::BadState("桩夹具越界：src 比 offset 短".into()));
                }
                let _ = path;
                Ok(SshReply::Size((data.len() - skip) as u64))
            }
            SshRpc::Close => {
                self.record("Close");
                Ok(SshReply::Ok)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{profile_id_of, RemoteProtocol};
    use crate::service::FileService;
    use host_core::error::AppError;
    use host_core::events::EventBus;
    use host_core::storage::StorageDriver;
    use std::sync::Arc;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_file_ssh_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sftp_profile(id: &str) -> RemoteProfile {
        RemoteProfile {
            id: id.into(),
            label: "sftp 站".into(),
            protocol: RemoteProtocol::Sftp,
            host: "sftp.example.invalid".into(),
            port: 2222,
            user: "me".into(),
            base_path: "/srv".into(),
            auth: AuthKind::PromptEachTime,
            preset_id: None,
            last_used_ms: 0,
        }
    }

    fn fake_auth() -> SshAuthRpc {
        SshAuthRpc::Password {
            user: "me".into(),
            password: Zeroizing::new("sesame-open-secret".into()),
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-5）字面测试名优先于 rustc 命名惯例
    fn knownHosts_corruptFile_isErrorNotSilentlyEmpty() {
        // 承重⑬(a)：坏文件必须炸，静默清空 = 接受任意主机。
        // 修前（term 式兜底）该臂等价于"表空→首见→自动自纳"三连，判红即达标。
        let d = tmpdir("corrupt");
        let p = d.join("kh-view.json");
        std::fs::write(&p, b"{ not json at all").unwrap();
        let e = load_known_hosts(&p).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("known_hosts"), "须点名文件，实得 {msg}");
        assert!(
            matches!(&e, FileError::Remote { code, .. } if *code == FILE_REMOTE_FIELD),
            "须报 005 字段级不符，实得 {e}"
        );
        // 半截 JSON（撕裂写形状）同样炸
        std::fs::write(&p, b"{\"a\": \"b\"").unwrap();
        assert!(load_known_hosts(&p).is_err(), "撕裂文件不得可读");
        // 类型不符（数组冒充映射表）也炸——serde 形状校验即门禁
        std::fs::write(&p, b"[1,2,3]").unwrap();
        assert!(load_known_hosts(&p).is_err());
        // 正对照防空洞：文件不存在 = 尚无记录，Ok 空表而非 Err
        let fresh = d.join("fresh.json");
        let store = load_known_hosts(&fresh).unwrap();
        assert!(store.get("any.example", 22).is_none());
        // 正对照二：合法文件可读回
        std::fs::write(&fresh, br#"{"h.example": "ssh-ed25519 SHA256:xYz"}"#).unwrap();
        let store = load_known_hosts(&fresh).unwrap();
        assert_eq!(
            store.get("h.example", 22).as_deref(),
            Some("ssh-ed25519 SHA256:xYz")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-5）字面测试名优先于 rustc 命名惯例
    fn tofu_firstSight_refusesConnectionAndNamesFingerprint() {
        // 承重⑬(b)：首见 → Unknown，且**真的拒了**——FakeSsh 断 calls 无后续
        // RPC（探测之后再发一条操作 RPC 就是"拒完继续连"）。
        let d = tmpdir("first");
        let store = load_known_hosts(&d.join("kh-view.json")).unwrap();
        let fp = "ssh-ed25519 SHA256:FIRSTSIGHTabc";
        let fake = Arc::new(FakeSsh::new(fp));
        let calls = fake.calls.clone();
        // 纯函数臂：三态裁决取 Unknown，指纹逐字
        let dec = check_server_key(&store, "sftp.example.invalid", 2222, fp).unwrap();
        assert!(
            matches!(&dec, HostKeyDecision::Unknown { fingerprint } if fingerprint == fp),
            "首见须裁 Unknown，实得 {dec:?}"
        );
        // 守卫臂：Err 且消息逐字点名指纹
        let e = tofu_guard(
            &*fake,
            &store,
            "sftp.example.invalid",
            2222,
            "me",
            &fake_auth(),
        )
        .unwrap_err();
        let msg = e.to_string();
        assert!(
            msg.contains(fp),
            "消息须逐字含 {fp}（永不归一化），实得 {msg}"
        );
        assert!(
            matches!(&e, FileError::Remote { code, .. } if *code == FILE_REMOTE_MISSING),
            "无记录即 001 的字面语义，实得 {e}"
        );
        // 探测即止步：Connect 之后不得有任何后续 RPC
        assert_eq!(calls.lock().as_slice(), ["Connect"], "拒后仍连即假拒");
        // 端到端臂（分派口真走一遍）：service.connect 拒、连接表空；
        // 显式确认指纹后才放行——Unknown 的唯一出路
        let svc = FileService::open(
            &d.join("store"),
            Arc::new(EventBus::new()),
            Arc::new(Ports::new()),
        )
        .unwrap();
        svc.profiles().save(sftp_profile("remote:first")).unwrap();
        svc.set_ssh_backend_for_test(fake.clone());
        // AuthSecret 故意不 Clone（凭据不复制的口径）——两次连接各现造一份
        let mk_secret = || AuthSecret {
            header: None,
            password: Some(Zeroizing::new("sesame".into())),
        };
        let e = svc
            .connect("remote:first", Some(mk_secret()), false)
            .unwrap_err();
        assert!(e.to_string().contains(fp), "分派口须点名指纹，实得 {e}");
        assert!(svc.remote_drivers().is_empty(), "被拒的连接不得入账");
        assert_eq!(
            calls.lock().as_slice(),
            ["Connect", "Connect"],
            "分派口也止步于探测"
        );
        svc.fingerprint_ack("remote:first", fp).unwrap();
        let info = svc
            .connect("remote:first", Some(mk_secret()), false)
            .unwrap();
        assert_eq!(info.protocol, "sftp");
        assert_eq!(svc.remote_drivers().len(), 1);
        // 确认写盘：单源共享文件（FileService 的域目录上跳一级即 appData 根），
        // 盘上记录逐字等于核对过的描述符
        let on_disk = std::fs::read_to_string(shared_known_hosts_path(&d)).unwrap();
        assert!(on_disk.contains(fp), "落盘须逐字，实得 {on_disk}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-5）字面测试名优先于 rustc 命名惯例
    fn tofu_changedKey_errsNamingBothFingerprints() {
        // term 变更臂零测的债在此还：recorded 与 actual 双双点名。
        let d = tmpdir("changed");
        let store = load_known_hosts(&d.join("kh-view.json")).unwrap();
        let recorded = "ssh-ed25519 SHA256:RECORDEDoldkey0ld";
        let actual = "ssh-ed25519 SHA256:ACTUALnewkeyac7";
        store
            .accept("sftp.example.invalid", 2222, recorded)
            .unwrap();
        let dec = check_server_key(&store, "sftp.example.invalid", 2222, actual).unwrap();
        assert!(
            matches!(&dec, HostKeyDecision::Changed { recorded: r, actual: a } if r == recorded && a == actual),
            "须裁 Changed 且两指纹逐字入体，实得 {dec:?}"
        );
        let fake = FakeSsh::new(actual);
        let e = tofu_guard(
            &fake,
            &store,
            "sftp.example.invalid",
            2222,
            "me",
            &fake_auth(),
        )
        .unwrap_err();
        let msg = e.to_string();
        assert!(
            msg.contains(recorded) && msg.contains(actual),
            "两枚指纹都须逐字点名，实得 {msg}"
        );
        assert!(
            !msg.contains("仍然连接"),
            "变更臂不得含糊出出路，实得 {msg}"
        );
        // 变更臂同样止步于探测
        assert_eq!(fake.calls.lock().as_slice(), ["Connect"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-5）字面测试名优先于 rustc 命名惯例
    fn tofu_trusted_matchesWholeKeyNotJustBase64() {
        // 整键相等才 Trusted：算法名不同而 base64 巧合相等 = 两把钥匙。
        let d = tmpdir("whole");
        let store = load_known_hosts(&d.join("kh-view.json")).unwrap();
        let base64 = "SAMEB64value==";
        let rsa = server_key_descriptor("ssh-rsa", base64);
        let ed = server_key_descriptor("ssh-ed25519", base64);
        store.accept("mismatch.example", 2222, &rsa).unwrap();
        let dec = check_server_key(&store, "mismatch.example", 2222, &ed).unwrap();
        assert!(
            matches!(&dec, HostKeyDecision::Changed { .. }),
            "同 base64 异算法必须落 Changed，绝不能 Trusted——{dec:?}"
        );
        // 正对照：整串相等 → Trusted；非 22 端口的键格式 `[host]:port`（term
        // ssh.rs:69 同谱，`[host]:2222` 形状）
        let exact = server_key_descriptor("ssh-ed25519", "abcDEF012==");
        store.accept("port2222.example", 2222, &exact).unwrap();
        let entries = store.entries();
        assert!(
            entries.iter().any(|(k, _)| k == "[port2222.example]:2222"),
            "非 22 端口键须为 [host]:port 形状，实得 {entries:?}"
        );
        let dec = check_server_key(&store, "port2222.example", 2222, &exact).unwrap();
        assert!(
            matches!(&dec, HostKeyDecision::Trusted { fingerprint } if fingerprint == &exact),
            "整串相等须 Trusted，实得 {dec:?}"
        );
        let fake = FakeSsh::new(&exact);
        tofu_guard(&fake, &store, "port2222.example", 2222, "me", &fake_auth()).unwrap();
        assert_eq!(fake.calls.lock().as_slice(), ["Connect"]);
        // 22 端口省略格式（同谱正对照）
        store
            .accept("plain.example", 22, "ssh-ed25519 SHA256:x")
            .unwrap();
        assert!(entries.iter().all(|(k, _)| k != "plain.example")); // entries 是改前快照——现取现断
        let entries2 = store.entries();
        assert!(
            entries2.iter().any(|(k, _)| k == "plain.example"),
            "22 端口键不得带 [host]:22 装饰，实得 {entries2:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-5）字面测试名优先于 rustc 命名惯例
    fn sshPassword_neverHitsDiskOrDisplay() {
        // ① Display 面：SshAuthRpc 的 Debug 对口令值零泄露（两臂都验）
        let pw = fake_auth();
        let dbg = format!("{pw:?}");
        assert!(!dbg.contains("sesame"), "Debug 泄露口令: {dbg}");
        assert!(dbg.contains("redacted"), "正对照：Debug 报在场");
        let kf = SshAuthRpc::KeyFile {
            user: "me".into(),
            key_path: "C:/keys/id_ed25519".into(),
            passphrase: Some(Zeroizing::new("passphrase-VALUE-9".into())),
        };
        let dbg = format!("{kf:?}");
        assert!(!dbg.contains("passphrase-VALUE-9"), "短语泄露: {dbg}");
        assert!(
            dbg.contains("C:/keys/id_ed25519"),
            "key_path 是允许展示的路径指针"
        );
        // ② 落盘面：档案 + known_hosts 全目录扫明文零命中。
        let d = tmpdir("nodisk");
        let store_dir = d.join("store");
        std::fs::create_dir_all(&store_dir).unwrap();
        let mut p = sftp_profile(&profile_id_of("vaulty"));
        p.auth = AuthKind::VaultEntry {
            entry_id: "entry-77".into(),
        };
        let profiles = crate::profile::ProfileStore::open(&store_dir.join("profiles")).unwrap();
        profiles.save(p.clone()).unwrap();
        for entry in walkdir::WalkDir::new(&store_dir) {
            let f = entry.unwrap();
            if f.file_type().is_file() {
                let bytes = std::fs::read(f.path()).unwrap();
                let text = String::from_utf8_lossy(&bytes);
                assert!(
                    !text.contains("sesame") && !text.contains("passphrase-VALUE-9"),
                    "盘上文件 {} 扫出口令明文",
                    f.path().display()
                );
            }
        }
        // known_hosts 落盘同样只有描述符
        let store = load_known_hosts(&store_dir.join("kh-view.json")).unwrap();
        store
            .accept(&p.host, p.port, "ssh-ed25519 SHA256:visible-only")
            .unwrap();
        let kh = std::fs::read_to_string(store_dir.join("kh-view.json")).unwrap();
        assert!(!kh.contains("sesame") && !kh.contains("passphrase"), "{kh}");
        // ③ 未送入凭据臂：VaultEntry 指针无值 → Err 点名三态归因结构，
        //    且**不回落**成"提示输口令"（口令永远不向协议腿自己伸手要）
        let e = sftp_auth_for(&p, None).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("entry-77"), "须点名条目 id，实得 {msg}");
        assert!(!msg.contains("请输入"), "禁回落交互式索取: {msg}");
        // 正对照：PromptEachTime 无凭据同样拒但消息不同串（三态各自点名，不共文案）
        let mut q = sftp_profile(&profile_id_of("prompty"));
        q.auth = AuthKind::PromptEachTime;
        let e2 = sftp_auth_for(&q, None).unwrap_err();
        assert_ne!(e.to_string(), e2.to_string(), "不同态必须不同消息");
        // ④ header 方言拒收（那是 WebDAV 腿的形状）
        let bad = AuthSecret {
            header: Some(Zeroizing::new("Basic c2VzYW1l".into())),
            password: None,
        };
        let e3 = sftp_auth_for(&q, Some(bad)).unwrap_err();
        assert!(
            e3.to_string().contains("Authorization"),
            "须点名头方言，实得 {e3}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-5）字面测试名优先于 rustc 命名惯例
    fn sshDriver_implementsStorageDriverForRemoteBrowse() {
        // trait 形状不塌的机检面：编译期泛型断言 + 端到端 Fake 走浏览三动词
        fn assert_driver<T: StorageDriver + Send + Sync>() {}
        assert_driver::<SftpDriver>();
        let d = tmpdir("driver");
        let store = load_known_hosts(&d.join("kh-view.json")).unwrap();
        let fake = Arc::new(FakeSsh::new("ssh-ed25519 SHA256:d").with_listing(vec![
            RemoteEntry {
                name: "报告.txt".into(),
                path: "/srv/报告.txt".into(),
                is_dir: false,
                size: 3,
                modified_ms: 0,
            },
            RemoteEntry {
                name: "sub".into(),
                path: "/srv/sub".into(),
                is_dir: true,
                size: 0,
                modified_ms: 0,
            },
        ]));
        let calls = fake.calls.clone();
        let drv: Arc<dyn StorageDriver> = Arc::new(SftpDriver::new(
            sftp_profile("remote:drv"),
            fake_auth(),
            fake.clone(),
            Arc::clone(&store),
        ));
        assert_eq!(
            drv.id(),
            "sftp",
            "自报协议名（动态键归注册口，与 WebDAV 同谱）"
        );
        let listed = drv.list(Path::new("/srv")).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].name, "报告.txt");
        assert!(!drv.roots().is_empty());
        drv.mkdir(Path::new("/srv/newdir")).unwrap();
        drv.remove(Path::new("/srv/gone.txt"), false).unwrap();
        assert_eq!(
            calls.lock().len(),
            3,
            "驱动操作直连协议腿，守卫在分派口已毕（每调用独立建连的 v1 形制）"
        );
        // 承重⑥第二道（驱动侧）：远端删除永不进回收站
        let e = match drv.remove(Path::new("/srv/gone.txt"), true) {
            Err(e) => e,
            Ok(()) => panic!("远端回收站臂必须拒"),
        };
        assert!(
            matches!(&e, AppError::Module { code, message, .. } if code == "FILE_OPS_005" && message.contains("回收站")),
            "须报 Unsupported→FILE_OPS_005 且点名回收站，实得 {e}"
        );
        // rename 经 trait 走通（同驱动内，形状不塌）
        drv.rename(Path::new("/srv/a"), Path::new("/srv/b"))
            .unwrap();
        assert_eq!(
            calls.lock().as_slice(),
            ["List", "Mkdir", "Remove", "Rename"]
        );
        // rename 跨驱动不可能：SftpDriver 的 from/to 都是同一协议腿的斜杠路径
        drop(drv);
        assert!(
            calls.lock().contains(&"Close".to_owned()),
            "Drop 走退役臂：Close 不得是纸面枚举"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-5）字面测试名优先于 rustc 命名惯例
    fn sftpResume_offsetsMatchPlan() {
        // Download{offset} 由 checkpoint 算出（Stat→range_plan→offset 三步
        // 全走 T-B6-4 立的唯一算式），续传后 bytes_done 单调 + 总大小正确
        let d = tmpdir("resume");
        let store = load_known_hosts(&d.join("kh-view.json")).unwrap();
        let src = vec![7u8; 10];
        let fake = Arc::new(FakeSsh::new("ssh-ed25519 SHA256:r").with_file("/srv/big.bin", &src));
        let calls = fake.calls.clone();
        let drv = SftpDriver::new(
            sftp_profile("remote:res"),
            fake_auth(),
            fake.clone(),
            Arc::clone(&store),
        );
        let dst = d.join("dst.bin");
        // 已有前 4 字节（模拟崩溃留下的半截文件）
        std::fs::write(&dst, &src[..4]).unwrap();
        let done = drv.download_to("/srv/big.bin", &dst, 4).unwrap();
        assert_eq!(done, 10, "bytes_done = 4 起点 + 6 新增");
        assert_eq!(std::fs::read(&dst).unwrap(), src, "续写后的文件逐字等于源");
        assert_eq!(
            calls.lock().as_slice(),
            ["Stat", "Download@4"],
            "offset 必须逐字取自 range_plan 的缺口起点"
        );
        // have==total：无缺口即不重跑（checkpoint 不写脏、显示即事实）
        let done2 = drv.download_to("/srv/big.bin", &dst, 10).unwrap();
        assert_eq!(done2, 10);
        assert_eq!(
            calls.lock().as_slice(),
            ["Stat", "Download@4", "Stat"],
            "不得发第二条 Download"
        );
        // have 超过 total（盘比声称的还长——range_plan 的 have>=total 臂）同样不重跑
        let done3 = drv.download_to("/srv/big.bin", &dst, 12).unwrap();
        assert_eq!(done3, 12, "原值回传，不动盘");
        assert_eq!(std::fs::read(&dst).unwrap().len(), 10);
        // 全新一腿：have=0 ⇒ offset=0 ⇒ 整取
        let dst2 = d.join("dst2.bin");
        let done4 = drv.download_to("/srv/big.bin", &dst2, 0).unwrap();
        assert_eq!(done4, 10);
        assert_eq!(std::fs::read(&dst2).unwrap(), src);
        assert!(calls.lock().contains(&"Download@0".to_owned()));
        // 上传腿的 offset 直通 seam（消费方接线归 T-B6-7，形状先钉死）
        let up = drv.upload_from("/srv/up.bin", &dst2, 3).unwrap();
        assert_eq!(up, 10, "have=3 + 送 7");
        assert!(calls.lock().contains(&"Upload@3".to_owned()));
        let _ = std::fs::remove_dir_all(&d);
    }
}
