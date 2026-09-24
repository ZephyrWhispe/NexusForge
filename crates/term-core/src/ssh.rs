//! T3 SSH/SFTP（docs/impl/06 T3）：russh 客户端 + TOFU known_hosts。
//!
//! - T-B7-1 治本：首见**拒连**（`TERM_SSH_004` + 指纹进 hint），只有用户经
//!   `term_ssh_fingerprint_ack` 明示核对才落信任；指纹变更拒连点名两枚（TERM_SSH_002）
//! - known_hosts 单一事实源 `{app_data}/ssh/known_hosts.json`（term 与 file
//!   两域共读一表，构造与旧表迁移在 host-core `ssh_trust`）；坏文件 fail-closed
//! - 终端会话：request_pty + request_shell → 输出泵入统一 SessionState
//! - SFTP：每次操作独立 channel + sftp subsystem（v1 简化，不复用连接池）
//! - 私钥走路径引用（不复制内容进 vault）；密码由 UI 现场输入不入库

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use russh::client::{self, Handle};
use russh::keys::key::PublicKey;
use russh::ChannelMsg;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use crate::error::{Result, TermError};
use crate::session::{SessionInfo, TermKind, TermSessions};
pub use host_core::ssh_trust::HostKeyDecision;

/// 连接超时
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// 认证方式（IPC 入参；密码现场输入，密钥走路径）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshAuth {
    Password {
        password: String,
    },
    Key {
        key_path: String,
        passphrase: Option<String>,
    },
}

/// 连接目标
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
}

/// known_hosts 单源视图（host-core `ssh_trust` 共享存储；路径构造与旧表
/// 迁移全在 host-core——本型只是 term 侧的薄门面）
pub struct KnownHosts {
    store: host_core::ssh_trust::KnownHostsStore,
}

impl std::fmt::Debug for KnownHosts {
    /// 与 host-core 存储同谱：只报规模，不整版抄表
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.store, f)
    }
}

impl KnownHosts {
    /// 以 appData 根打开共享信任文件。**坏文件 Err 不自愈**——
    /// 拒绝以空表启动（错误消息点名路径，见 `TrustError::Corrupt`）。
    pub fn open(app_data_dir: &Path) -> Result<Self> {
        Ok(Self {
            store: host_core::ssh_trust::KnownHostsStore::shared(app_data_dir)
                .map_err(TermError::from)?,
        })
    }

    /// 单一事实源路径（与 file 域同一构造函数——`fileAndTerm_shareSingleStore`
    /// 恒一断言的 term 半边）
    pub fn shared_path(app_data_dir: &Path) -> PathBuf {
        host_core::ssh_trust::shared_path(app_data_dir)
    }

    /// 查询已记录整键描述符
    pub fn get(&self, host: &str, port: u16) -> Option<String> {
        self.store.get(host, port)
    }

    /// 记录/更新**整键逐字**（`"algo SHA256:base64"`）——只在用户经
    /// `term_ssh_fingerprint_ack` 明示核对后调用；首见绝不自动流经此口（T-B7-1 治本）
    pub fn accept(&self, host: &str, port: u16, whole_key: &str) -> Result<()> {
        self.store
            .accept(host, port, whole_key)
            .map_err(TermError::from)
    }

    /// 删除记录（用户确认主机重建后允许重连）
    pub fn remove(&self, host: &str, port: u16) -> Result<bool> {
        self.store.remove(host, port).map_err(TermError::from)
    }

    /// 全部记录（UI 管理：规范键 → 整键描述符）
    pub fn entries(&self) -> Vec<(String, String)> {
        self.store.entries()
    }

    /// 三态裁决（首见 = Unknown 零写；裸旧记录命中即 Trusted 并升级落盘）
    pub fn decide(&self, host: &str, port: u16, actual: &str) -> HostKeyDecision {
        self.store.decide(host, port, actual)
    }
}

/// 整键描述符组形口：russh 公钥 → `"{algo} {SHA256:base64}"`（盘上比对与
/// UI 展示的是同一串，永不只取 base64 段）
fn whole_key_of(key: &PublicKey) -> String {
    format!("{} {}", key.name(), key.fingerprint())
}

/// TOFU 三态 → TermError 的统一裁决出口（Handler 臂与测试臂共用一枚——
/// 点名牌只有这一份措辞，错误消息构造不存在第二套）
fn tofu_verdict(known: &KnownHosts, host: &str, port: u16, whole_key: &str) -> Result<()> {
    match known.decide(host, port, whole_key) {
        HostKeyDecision::Trusted { .. } => Ok(()),
        HostKeyDecision::Unknown { fingerprint } => Err(TermError::HostKeyUnknown {
            host: host.to_owned(),
            port,
            descriptor: fingerprint,
        }),
        HostKeyDecision::Changed { recorded, actual } => Err(TermError::HostKey(format!(
            "主机 {host}:{port} 密钥已变更！记录（逐字）{recorded}，实收（逐字）{actual}。\
             若确认主机重建，请删除已知主机记录后重连；不存在带着旧记录继续连的出路。",
        ))),
    }
}

/// russh Handler：TOFU 校验（首见拒 + 变更拒，都不允许静默接受——docs/impl/06 风险标注）
struct TofuHandler {
    known: Arc<KnownHosts>,
    host: String,
    port: u16,
}

#[async_trait]
impl client::Handler for TofuHandler {
    type Error = TermError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        tofu_verdict(
            &self.known,
            &self.host,
            self.port,
            &whole_key_of(server_public_key),
        )?;
        Ok(true)
    }
}

/// 认证（密码 / 私钥路径）；0.46 返回 Result<bool>
async fn authenticate(handle: &mut Handle<TofuHandler>, user: &str, auth: &SshAuth) -> Result<()> {
    let ok = match auth {
        SshAuth::Password { password } => handle
            .authenticate_password(user, password.as_str())
            .await
            .map_err(|e| TermError::Auth(format!("密码认证失败: {e}")))?,
        SshAuth::Key {
            key_path,
            passphrase,
        } => {
            let mut buf = std::fs::read(key_path).map_err(TermError::Io)?;
            let key = match russh::keys::decode_openssh(
                &buf,
                passphrase.as_deref().map(|s| s.to_string()).as_deref(),
            ) {
                Ok(k) => k,
                Err(e) => {
                    use zeroize::Zeroize;
                    buf.zeroize();
                    return Err(TermError::Auth(format!("私钥解析失败（口令错误？）: {e}")));
                }
            };
            use zeroize::Zeroize;
            buf.zeroize();
            handle
                .authenticate_publickey(user, Arc::new(key))
                .await
                .map_err(|e| TermError::Auth(format!("公钥认证失败: {e}")))?
        }
    };
    if ok {
        Ok(())
    } else {
        Err(TermError::Auth("服务器拒绝认证凭据".into()))
    }
}

/// 建立连接（TOFU + 认证）
async fn connect_ssh(target: &SshTarget, known: Arc<KnownHosts>) -> Result<Handle<TofuHandler>> {
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(600)),
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let addr = (target.host.as_str(), target.port);
    let handler = TofuHandler {
        known,
        host: target.host.clone(),
        port: target.port,
    };
    let mut handle = tokio::time::timeout(
        CONNECT_TIMEOUT,
        russh::client::connect(config, addr, handler),
    )
    .await
    .map_err(|_| TermError::Ssh(format!("连接超时（{}s）", CONNECT_TIMEOUT.as_secs())))?
    // connect 返回 H::Error = TermError（check_server_key 拒绝即 HostKey/HostKeyUnknown 详情，
    // 两枚信任臂都原样过网——不许被兜底臂裹成 TERM_SSH_001）
    .map_err(|e| match &e {
        TermError::HostKey(_) | TermError::HostKeyUnknown { .. } => e,
        other => TermError::Ssh(format!(
            "连接 {host}:{port} 失败: {other}",
            host = target.host,
            port = target.port
        )),
    })?;
    authenticate(&mut handle, &target.user, &target.auth).await?;
    Ok(handle)
}

/// TermSessions 的 SSH 扩展（避免循环依赖，SSH 会话注册复用统一 register 路径）
pub struct SshService {
    known: Arc<KnownHosts>,
}

impl SshService {
    /// 以 appData 根构造（信任面走 T-B7-1 单一事实源；坏文件即 Err）
    pub fn new(app_data_dir: &Path) -> Result<Self> {
        Ok(Self {
            known: Arc::new(KnownHosts::open(app_data_dir)?),
        })
    }

    pub fn known_hosts(&self) -> &Arc<KnownHosts> {
        &self.known
    }

    /// SSH 终端会话：连接 + PTY + shell → 输出泵进统一 reader（TermSessions.register）
    pub async fn open_shell(
        &self,
        target: SshTarget,
        cols: u16,
        rows: u16,
        sessions: &TermSessions,
    ) -> Result<SessionInfo> {
        let handle = connect_ssh(&target, self.known.clone()).await?;
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| TermError::Ssh(format!("打开会话通道失败: {e}")))?;
        channel
            .request_pty(
                false,
                "xterm-256color",
                cols.max(1) as u32,
                rows.max(1) as u32,
                0,
                0,
                &[],
            )
            .await
            .map_err(|e| TermError::Ssh(format!("请求 PTY 失败: {e}")))?;
        channel
            .request_shell(false)
            .await
            .map_err(|e| TermError::Ssh(format!("请求 shell 失败: {e}")))?;

        // Channel 无法 clone：wait 需要 &mut、data/window_change 需要 &self——
        // 用 AsyncMutex 共享（读任务持锁等待 msg，写任务短暂持锁发数据）
        let channel = Arc::new(AsyncMutex::new(channel));

        let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(256);
        let (resize_tx, mut resize_rx) = mpsc::channel::<(u16, u16)>(16);
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(1024);

        // 输入泵
        let ch_in = channel.clone();
        tokio::spawn(async move {
            while let Some(data) = input_rx.recv().await {
                let ch = ch_in.lock().await;
                if ch.data(&data[..]).await.is_err() {
                    break;
                }
            }
        });
        // resize 泵
        let ch_r = channel.clone();
        tokio::spawn(async move {
            while let Some((c, r)) = resize_rx.recv().await {
                let ch = ch_r.lock().await;
                if ch
                    .window_change(c.max(1) as u32, r.max(1) as u32, 0, 0)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        // 输出泵：wait() 独占读（持锁直到 msg 到达）
        let ch_out = channel.clone();
        tokio::spawn(async move {
            let mut ch = ch_out.lock().await;
            while let Some(msg) = ch.wait().await {
                match msg {
                    ChannelMsg::Data { data } => {
                        if out_tx.send(data.to_vec()).await.is_err() {
                            break;
                        }
                    }
                    ChannelMsg::ExtendedData { data, .. } => {
                        if out_tx.send(data.to_vec()).await.is_err() {
                            break;
                        }
                    }
                    ChannelMsg::ExitStatus { .. } | ChannelMsg::Eof | ChannelMsg::Close => {
                        break;
                    }
                    _ => {}
                }
            }
            // drop out_tx：reader 收到 EOF（会话结束信号）
        });

        let kind = TermKind::Ssh {
            host: target.host.clone(),
            port: target.port,
            user: target.user.clone(),
        };
        let title = format!("{}@{}", target.user, target.host);
        // 统一注册入口：SSH 输出/输入/resize 全部经由 PtyHandle 四件套
        let ssh_handle = host_core::ports::PtyHandle::new(
            input_tx,
            out_rx,
            resize_tx,
            Box::new(move || {
                // kill：关 channel + 断开连接（FnOnce 同步上下文内 spawn 异步清理）
                let ch = channel.clone();
                tokio::spawn(async move {
                    let c = ch.lock().await;
                    let _ = c.close().await;
                });
                tokio::spawn(async move {
                    let _ = handle
                        .disconnect(russh::Disconnect::ByApplication, "user quit", "en")
                        .await;
                });
            }),
        );
        sessions.register(kind, title, cols, rows, ssh_handle).await
    }

    /// SFTP 目录列表
    pub async fn sftp_list(&self, target: &SshTarget, path: &str) -> Result<Vec<SftpEntry>> {
        let mut handle = connect_ssh(target, self.known.clone()).await?;
        let sftp = open_sftp(&mut handle).await?;
        let dir = sftp
            .read_dir(path)
            .await
            .map_err(|e| TermError::Sftp(format!("读取目录失败: {e}")))?;
        Ok(dir
            .into_iter()
            .map(|e| {
                let m = e.metadata();
                SftpEntry {
                    name: e.file_name(),
                    is_dir: m.is_dir(),
                    size: m.size.unwrap_or(0),
                }
            })
            .collect())
    }

    /// SFTP 下载（远程 → 本地）
    pub async fn sftp_download(
        &self,
        target: &SshTarget,
        remote: &str,
        local: &Path,
    ) -> Result<u64> {
        let mut handle = connect_ssh(target, self.known.clone()).await?;
        let sftp = open_sftp(&mut handle).await?;
        let mut remote_file = sftp
            .open(remote)
            .await
            .map_err(|e| TermError::Sftp(format!("打开远程文件失败: {e}")))?;
        let mut local_file = tokio::fs::File::create(local)
            .await
            .map_err(TermError::Io)?;
        let n = tokio::io::copy(&mut remote_file, &mut local_file)
            .await
            .map_err(TermError::Io)?;
        local_file.flush().await.map_err(TermError::Io)?;
        Ok(n)
    }

    /// SFTP 上传（本地 → 远程）
    pub async fn sftp_upload(&self, target: &SshTarget, local: &Path, remote: &str) -> Result<u64> {
        let mut handle = connect_ssh(target, self.known.clone()).await?;
        let sftp = open_sftp(&mut handle).await?;
        let mut local_file = tokio::fs::File::open(local).await.map_err(TermError::Io)?;
        let mut remote_file = sftp
            .create(remote)
            .await
            .map_err(|e| TermError::Sftp(format!("创建远程文件失败: {e}")))?;
        let n = tokio::io::copy(&mut local_file, &mut remote_file)
            .await
            .map_err(TermError::Io)?;
        remote_file
            .flush()
            .await
            .map_err(|e| TermError::Sftp(e.to_string()))?;
        Ok(n)
    }
}

/// 打开 sftp subsystem 通道
async fn open_sftp(handle: &mut Handle<TofuHandler>) -> Result<russh_sftp::client::SftpSession> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| TermError::Ssh(format!("打开通道失败: {e}")))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| TermError::Sftp(format!("请求 sftp subsystem 失败: {e}")))?;
    russh_sftp::client::SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| TermError::Sftp(format!("SFTP 会话建立失败: {e}")))
}

/// SFTP 条目（IPC DTO）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SftpEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_term_ssh_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn seed_shared(app: &Path, obj: &serde_json::Value) -> PathBuf {
        let p = KnownHosts::shared_path(app);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_vec(obj).unwrap()).unwrap();
        p
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-1）字面测试名优先于 rustc 命名惯例
    fn knownHosts_corruptFile_isErrNotSilentlyEmpty() {
        // 修前判红即达标（旧臂解析失败静默回落空表）——镜像 file 域同名测
        let d = tmpdir("corrupt");
        let p = seed_shared(&d, &serde_json::json!({"ok": "x"}));
        std::fs::write(&p, b"{ torn json").unwrap();
        let e = KnownHosts::open(&d).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("known_hosts"), "必须点名文件，实得 {msg}");
        assert!(
            msg.contains("以空表启动即被拒绝"),
            "fail-closed 语义必须显形"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名
    fn tofu_firstSight_returnsUnknownAndZeroWrites() {
        let d = tmpdir("firstsight");
        let kh = KnownHosts::open(&d).unwrap();
        let e = tofu_verdict(&kh, "srv.example", 22, "ssh-ed25519 SHA256:aaa").unwrap_err();
        match &e {
            TermError::HostKeyUnknown { descriptor, .. } => {
                assert_eq!(
                    descriptor, "ssh-ed25519 SHA256:aaa",
                    "指纹必须逐字进错误体（hint 通道）"
                );
            }
            other => panic!("首见必须是 HostKeyUnknown（TERM_SSH_004），实得 {other:?}"),
        }
        assert_eq!(e.code(), "TERM_SSH_004");
        assert!(
            !KnownHosts::shared_path(&d).exists(),
            "首见零写——不存在隐式自纳这回事"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名
    fn tofu_changed_errsNamingBothFingerprints() {
        let d = tmpdir("changed");
        seed_shared(
            &d,
            &serde_json::json!({"srv.example": "ssh-ed25519 SHA256:old"}),
        );
        let kh = KnownHosts::open(&d).unwrap();
        let e = tofu_verdict(&kh, "srv.example", 22, "ssh-ed25519 SHA256:new").unwrap_err();
        let msg = e.to_string();
        assert!(
            matches!(e, TermError::HostKey(_)),
            "变更臂维持 TERM_SSH_002"
        );
        assert!(
            msg.contains("ssh-ed25519 SHA256:old") && msg.contains("ssh-ed25519 SHA256:new"),
            "两枚指纹都必须逐字点名，实得 {msg}"
        );
        // 盘态未被裁决改动
        let on_disk = std::fs::read_to_string(KnownHosts::shared_path(&d)).unwrap();
        assert!(on_disk.contains("old") && !on_disk.contains("\"ssh-ed25519 SHA256:new\""));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名
    fn tofu_trusted_matchesWholeKeyVerbatim() {
        let d = tmpdir("verbatim");
        seed_shared(
            &d,
            &serde_json::json!({
                "srv.example": "ssh-ed25519 SHA256:abc",
                "legacy.bare": "SHA256:def"
            }),
        );
        let kh = KnownHosts::open(&d).unwrap();
        // 整键逐字相等 → 放行
        tofu_verdict(&kh, "srv.example", 22, "ssh-ed25519 SHA256:abc").unwrap();
        // base64 巧合相等而算法名不同 → 拒（整键口径的负例臂）
        let e = tofu_verdict(&kh, "srv.example", 22, "ssh-rsa SHA256:abc").unwrap_err();
        assert!(matches!(e, TermError::HostKey(_)));
        // 裸旧记录（term 遗留值形制）正对照：指纹段命中 → Trusted 且升级落盘
        tofu_verdict(&kh, "legacy.bare", 22, "ssh-rsa SHA256:def").unwrap();
        assert_eq!(
            kh.get("legacy.bare", 22).as_deref(),
            Some("ssh-rsa SHA256:def")
        );
        // 非 22 端口键形状
        kh.accept("srv.example", 2222, "ssh-ed25519 SHA256:port2222")
            .unwrap();
        assert_eq!(
            kh.get("srv.example", 2222).as_deref(),
            Some("ssh-ed25519 SHA256:port2222")
        );
        assert!(
            KnownHosts::shared_path(&d).exists(),
            "明示 accept 后才允许落盘"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn known_hosts_roundtrip_and_forget() {
        let d = tmpdir("lifecycle");
        let kh = KnownHosts::open(&d).unwrap();
        assert_eq!(kh.get("srv.example", 22), None);
        kh.accept("srv.example", 22, "ssh-ed25519 SHA256:abc")
            .unwrap();
        kh.accept("srv.example", 2222, "ssh-ed25519 SHA256:def")
            .unwrap();
        assert_eq!(kh.entries().len(), 2);
        // 重开持久化（同一共享文件）
        let kh2 = KnownHosts::open(&d).unwrap();
        assert_eq!(
            kh2.get("srv.example", 22).as_deref(),
            Some("ssh-ed25519 SHA256:abc")
        );
        assert!(kh2.remove("srv.example", 22).unwrap());
        assert!(kh2.get("srv.example", 22).is_none());
        assert!(!kh2.remove("srv.example", 22).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn host_key_names() {
        // 规范键规则维持现行两域同谱形制（22 端口省略为裸 host）——
        // 任务书"废除 22 省略"经 §7.1 核账修正为维持，落地补记在册
        assert_eq!(host_core::ssh_trust::canonical_key("h", 22), "h");
        assert_eq!(host_core::ssh_trust::canonical_key("h", 2200), "[h]:2200");
    }
}
