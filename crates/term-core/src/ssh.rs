//! T3 SSH/SFTP（docs/impl/06 T3）：russh 客户端 + TOFU known_hosts。
//!
//! - T-B7-1 治本：首见**拒连**（`TERM_SSH_004` + 指纹进 hint），只有用户经
//!   `term_ssh_fingerprint_ack` 明示核对才落信任；指纹变更拒连点名两枚（TERM_SSH_002）
//! - known_hosts 单一事实源 `{app_data}/ssh/known_hosts.json`（term 与 file
//!   两域共读一表，构造与旧表迁移在 host-core `ssh_trust`）；坏文件 fail-closed
//! - 终端会话：request_pty + request_shell → 输出泵入统一 SessionState
//! - 一次性 exec（T-B7-2）：无 PTY 通道，stdout/stderr/exit 三分收口，
//!   超时只立 `timed_out` 不编造退出码
//! - SFTP：每次操作独立 channel + sftp subsystem（v1 简化，不复用连接池）
//! - ProxyJump（T-B7-4）：`SshTarget::jump` 链式递归（≤3 跳，逐跳独立凭据），
//!   逐跳经 direct-tcpip 建隧道；任一跳 TOFU 拒 → 整链拒且点名"第 N 跳"
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

/// ProxyJump 一跳（T-B7-4）：链式递归——`via` 是更靠近客户端的前链，
/// 最外层 `jump` 是离目标最近的一跳。每跳独立凭据（`auth` 字段），
/// 跨跳不复用不回落。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JumpHop {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    pub via: Option<Box<JumpHop>>,
}

/// 连接目标
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    /// ProxyJump 前链（T-B7-4）。serde default = 旧 JSON 会话参数无 jump 键可读
    #[serde(default)]
    pub jump: Option<Box<JumpHop>>,
}

/// 一次性非交互 exec 结果（T-B7-2）：`exit_code = None` 是"未收到退出码
/// 消息"（超时/信号终止/通道早关）——**不是 0**；`timed_out` 是超时的
/// 唯一终态证据，超时臂保留已收集的 partial 输出但绝不编造退出码
#[derive(Debug, Clone, Serialize)]
pub struct ExecResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// exec 入参守卫（空命令拒在连接之前——不产生任何出站请求）
fn exec_guard(command: &str) -> Result<()> {
    if command.trim().is_empty() {
        return Err(TermError::BadParam(
            "exec 命令不得为空（也不得只有空白）".into(),
        ));
    }
    Ok(())
}

/// 录制帧夹具的落点：逐帧消费 russh ChannelMsg 的纯累加器（零 IO 零时钟）。
/// Data→stdout；ExtendedData→stderr（ext=1 是标准错误；其余扩展数据也进
/// stderr 档——不冒充终端回显，也不另立第三出路）；ExitStatus 记码；
/// Eof/Close/ExitSignal 终结（ExitSignal 无退出码=如实 None）。
#[derive(Debug, Default)]
struct ExecAccumulator {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: Option<i32>,
    done: bool,
}

impl ExecAccumulator {
    fn on_msg(&mut self, msg: ChannelMsg) {
        match msg {
            ChannelMsg::Data { data } => self.stdout.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, .. } => self.stderr.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => {
                // 超 i32 范围的"退出码"不是退出码——如实 None，不截断伪造
                self.exit_code = i32::try_from(exit_status).ok();
            }
            ChannelMsg::Eof | ChannelMsg::Close | ChannelMsg::ExitSignal { .. } => {
                self.done = true;
            }
            _ => {}
        }
    }

    fn finish(self, timed_out: bool) -> ExecResult {
        ExecResult {
            exit_code: self.exit_code,
            stdout: String::from_utf8_lossy(&self.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&self.stderr).into_owned(),
            timed_out,
        }
    }
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
            note: String::new(),
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

/// russh 客户端配置（直连与隧道跳共用同一构造口——两臂参数永不漂移）
fn client_config() -> Arc<client::Config> {
    Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(600)),
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    })
}

/// ProxyJump 跳数上限（T-B7-4）：超限拒在任何建连之前
pub const JUMP_MAX_HOPS: usize = 3;

/// 展开 jump 链：`via` 最深者 = 第 1 跳 …… 最外层 `jump` = 紧邻目标的一跳
fn jump_chain(jump: &Option<Box<JumpHop>>) -> Vec<&JumpHop> {
    let mut out = Vec::new();
    let mut cur = jump.as_deref();
    while let Some(h) = cur {
        out.push(h);
        cur = h.via.as_deref();
    }
    out.reverse();
    out
}

/// 深度闸（在任何一跳建连之前判定；"跳数超限"唯一措辞，负例臂在测）
fn jump_guard(jump: &Option<Box<JumpHop>>) -> Result<()> {
    let n = jump_chain(jump).len();
    if n > JUMP_MAX_HOPS {
        return Err(TermError::BadParam(format!(
            "跳数超限（收到 {n} 跳 > 上限 {JUMP_MAX_HOPS} 跳）——未连接任何一跳"
        )));
    }
    Ok(())
}

/// 信任臂错误的跳数展示前缀：只给 Unknown 加 note，host/port/descriptor
/// 逐字原样（指纹确认的三枚输入不被跳数信息污染）；已有 note 不覆写（幂等）
fn hop_ctx(n: usize, e: TermError) -> TermError {
    match e {
        TermError::HostKeyUnknown {
            note,
            host,
            port,
            descriptor,
        } => TermError::HostKeyUnknown {
            note: if note.is_empty() {
                format!("第 {n} 跳 ")
            } else {
                note
            },
            host,
            port,
            descriptor,
        },
        other => other,
    }
}

/// 连接错误统一措辞（直连臂与隧道臂同源——`jump_none_keepsLegacyPath`
/// 的逐字相等由此构造性保证）：信任两臂原样过网，其余裹成点名臂；
/// `n=Some(k)` 再叠跳数前缀
fn connect_map(n: Option<usize>, target: &SshTarget, e: TermError) -> TermError {
    let is_trust = matches!(e, TermError::HostKey(_) | TermError::HostKeyUnknown { .. });
    let mapped = if is_trust {
        e
    } else {
        let disp = e.to_string();
        TermError::Ssh(format!("连接 {}:{} 失败: {disp}", target.host, target.port))
    };
    match n {
        Some(k) => hop_ctx(k, mapped),
        None => mapped,
    }
}

/// 建立连接（TOFU + 认证）
async fn connect_ssh(target: &SshTarget, known: Arc<KnownHosts>) -> Result<Handle<TofuHandler>> {
    let config = client_config();
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
    .map_err(|e| connect_map(None, target, e))?;
    authenticate(&mut handle, &target.user, &target.auth).await?;
    Ok(handle)
}

/// 经既有隧道连接一跳（direct-tcpip → connect_stream）。`n=Some(k)` 给
/// 该跳的信任拒叠"第 k 跳"前缀；目标腿传 `None`（消息本体已点名目标）
async fn connect_hop(
    prev: &Handle<TofuHandler>,
    target: &SshTarget,
    known: Arc<KnownHosts>,
    n: Option<usize>,
) -> Result<Handle<TofuHandler>> {
    let channel = prev
        .channel_open_direct_tcpip(target.host.clone(), target.port as u32, "nexusforge", 0)
        .await
        .map_err(|e| {
            TermError::Ssh(format!(
                "打开下一跳 {}:{} 的隧道通道失败: {e}",
                target.host, target.port
            ))
        })?;
    let handler = TofuHandler {
        known,
        host: target.host.clone(),
        port: target.port,
    };
    let mut handle = tokio::time::timeout(
        CONNECT_TIMEOUT,
        russh::client::connect_stream(client_config(), channel.into_stream(), handler),
    )
    .await
    .map_err(|_| TermError::Ssh(format!("连接超时（{}s）", CONNECT_TIMEOUT.as_secs())))?
    .map_err(|e| connect_map(n, target, e))?;
    authenticate(&mut handle, &target.user, &target.auth).await?;
    Ok(handle)
}

/// 连接口（T-B7-4）：`jump=None` 逐字委派直连（既有腿零漂移）；有链则
/// 逐跳建连——第 1 跳走 TCP 直连（TOFU 如常），后续跳与目标经
/// direct-tcpip 隧道。**任一跳指纹 Unknown/变更 → 整链拒**，信任错误
/// 点名"第 N 跳"。跳会话的显式收编口属 T-B7-5 ForwardTable（随批登记）：
/// 本行复用既有会话清扫面（通道关 + inactivity/keepalive 失效即终）。
async fn connect_via_jumps(
    target: &SshTarget,
    known: Arc<KnownHosts>,
) -> Result<Handle<TofuHandler>> {
    jump_guard(&target.jump)?;
    let hops = jump_chain(&target.jump);
    if hops.is_empty() {
        return connect_ssh(target, known).await;
    }
    let to_target = |h: &JumpHop| SshTarget {
        host: h.host.clone(),
        port: h.port,
        user: h.user.clone(),
        auth: h.auth.clone(),
        jump: None,
    };
    let mut cur = connect_ssh(&to_target(hops[0]), known.clone())
        .await
        .map_err(|e| hop_ctx(1, e))?;
    for (i, hop) in hops[1..].iter().enumerate() {
        cur = connect_hop(&cur, &to_target(hop), known.clone(), Some(i + 2)).await?;
    }
    connect_hop(&cur, target, known, None).await
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
        let handle = connect_via_jumps(&target, self.known.clone()).await?;
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

    /// SFTP 目录列表（SFTP 各口维持直连腿——T-B7-4 行范围=终端连接命令，
    /// jump 接线挂批次尾台账）
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

    /// 一次性非交互远端命令（T-B7-2）：无 PTY 的 exec 通道，输出按
    /// stdout/stderr/exit 三分收口——**不冒充终端回显**。TOFU 门复用
    /// T-B7-1 的 connect 臂：首见主机在 KEX 即拒，通道打开与 exec 请求
    /// 结构性地发生在认证通过之后（首见 = 零出站 exec 请求）。
    /// 超时=到点停收：partial 输出保留、`timed_out` 立牌，退出码不编造。
    pub async fn exec(
        &self,
        target: &SshTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<ExecResult> {
        exec_guard(command)?;
        let handle = connect_via_jumps(target, self.known.clone()).await?;
        let mut channel = handle
            .channel_open_session()
            .await
            .map_err(|e| TermError::Ssh(format!("打开会话通道失败: {e}")))?;
        channel
            .exec(true, command)
            .await
            .map_err(|e| TermError::Ssh(format!("发送 exec 请求失败: {e}")))?;
        let mut acc = ExecAccumulator::default();
        let deadline = tokio::time::Instant::now() + timeout;
        let mut timed_out = false;
        while !acc.done {
            match tokio::time::timeout_at(deadline, channel.wait()).await {
                Err(_elapsed) => {
                    timed_out = true;
                    break;
                }
                Ok(None) => break, // 通道关闭：EOF 即终局
                Ok(Some(msg)) => acc.on_msg(msg),
            }
        }
        // best-effort 断连：exec 是一次性会话，不等 inactivity_timeout 收尸
        let _ = handle
            .disconnect(russh::Disconnect::ByApplication, "exec done", "en")
            .await;
        Ok(acc.finish(timed_out))
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

    // ---- T-B7-2 一次性非交互 exec（录制帧夹具谱：累加器逐帧消费） ----

    fn data_msg(b: &[u8]) -> ChannelMsg {
        ChannelMsg::Data {
            data: b.to_vec().into(),
        }
    }
    fn ext_msg(b: &[u8]) -> ChannelMsg {
        ChannelMsg::ExtendedData {
            ext: 1, // SSH_EXTENDED_DATA_STDERR
            data: b.to_vec().into(),
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-2）字面测试名优先于 rustc 命名惯例
    fn exec_returnsExitCodeAndStreams() {
        let mut acc = ExecAccumulator::default();
        for msg in [
            data_msg(b"out-1"),
            ext_msg(b"err-1"),
            data_msg(b"out-2"),
            ChannelMsg::ExitStatus { exit_status: 7 },
            ChannelMsg::Close,
        ] {
            acc.on_msg(msg);
        }
        assert!(acc.done, "Close 帧必须置终");
        let r = acc.finish(false);
        assert_eq!(r.exit_code, Some(7));
        assert_eq!(r.stdout, "out-1out-2", "stdout 只收 Data 帧");
        assert_eq!(r.stderr, "err-1", "stderr 只收 ExtendedData 帧，两档不混流");
        assert!(!r.timed_out);
        // 无退出码消息 = None 而非 0（"没拿到"与"拿到 0"是两回事）
        let mut acc2 = ExecAccumulator::default();
        acc2.on_msg(data_msg(b"partial"));
        acc2.on_msg(ChannelMsg::Eof);
        let r2 = acc2.finish(false);
        assert_eq!(r2.exit_code, None, "无 ExitStatus 帧不得伪造 0");
        assert_eq!(r2.stdout, "partial");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-2）字面测试名优先于 rustc 命名惯例
    fn exec_timeout_setsTimedOutNotZero() {
        // 超时臂：到点只有一枚 partial stdout、无 ExitStatus——终态证据
        // 只有 timed_out，退出码保持 None（不谎报 0）
        let mut acc = ExecAccumulator::default();
        acc.on_msg(data_msg(b"slow-out"));
        let r = acc.finish(true);
        assert!(r.timed_out);
        assert_eq!(r.exit_code, None, "超时终态禁止编造退出码");
        assert_eq!(r.stdout, "slow-out", "partial 输出必须保留");
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-2）字面测试名优先于 rustc 命名惯例
    fn exec_emptyCommand_rejected() {
        for cmd in ["", "   ", "\t\n"] {
            let e = exec_guard(cmd).unwrap_err();
            assert!(
                matches!(e, TermError::BadParam(_)),
                "空命令必须 BadParam 拒在连接之前（零出站），实得 {e:?}"
            );
        }
        exec_guard("hostname").unwrap();
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-2）字面测试名优先于 rustc 命名惯例
    fn exec_tofuUnknown_refusesBeforeChannelOpen() {
        // 任务书 fallback（"两枚不可得则登记为冒烟项并以 Handler 单测替代"）：
        // exec 与 shell 共用 connect_ssh 的同一道 TOFU 门（唯一牌面，本文件
        // 不存在第二处裁决构造）；通道打开/认证/exec 请求结构性地排在
        // KEX 裁决之后——首见 = KEX 即拒 = 零出站 exec 请求。此处钉死门
        // 本身：Unknown → TERM_SSH_004 且信任表零写。真实服务器的
        // "零出站 exec 包"抓包为人工冒烟项（批次尾登记）。
        let d = tmpdir("execgate");
        let kh = KnownHosts::open(&d).unwrap();
        let e = tofu_verdict(&kh, "never.seen", 22, "ssh-ed25519 SHA256:zzz").unwrap_err();
        assert_eq!(e.code(), "TERM_SSH_004");
        assert!(!KnownHosts::shared_path(&d).exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    // ---- T-B7-4 ProxyJump ----

    fn hop(
        host: &str,
        port: u16,
        user: &str,
        auth: SshAuth,
        via: Option<Box<JumpHop>>,
    ) -> Box<JumpHop> {
        Box::new(JumpHop {
            host: host.into(),
            port,
            user: user.into(),
            auth,
            via,
        })
    }

    /// 闭端口（127.0.0.1:1）目标——连接必败且零凭据风险，专供错误臂断言
    fn closed_target(jump: Option<Box<JumpHop>>) -> SshTarget {
        SshTarget {
            host: "127.0.0.1".into(),
            port: 1,
            user: "t".into(),
            auth: SshAuth::Password {
                password: "x".into(),
            },
            jump,
        }
    }

    /// Handle 无 Debug ⇒ unwrap_err 不可用；连接臂必败取错
    fn conn_err(r: Result<Handle<TofuHandler>>) -> TermError {
        match r {
            Err(e) => e,
            Ok(_) => panic!("127.0.0.1:1 闭端口连接本应必败"),
        }
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-4）字面测试名优先于 rustc 命名惯例
    async fn jump_none_keepsLegacyPath() {
        let d = tmpdir("junnolegacy");
        let kh = Arc::new(KnownHosts::open(&d).unwrap());
        // 正对照：jump=None 与直连臂逐字同消息（空链臂字面委派 connect_ssh，
        // 任何偏差=跳链改造污染了直连腿）
        let direct = conn_err(connect_ssh(&closed_target(None), kh.clone()).await).to_string();
        let via = conn_err(connect_via_jumps(&closed_target(None), kh.clone()).await).to_string();
        assert_eq!(direct, via, "两臂错误措辞必须逐字相等");
        assert!(direct.contains("连接 127.0.0.1:1 失败"), "实得 {direct}");
        // 旧 JSON 会话参数无 jump 键可读（serde default——加键兼容）
        let legacy = serde_json::json!({
            "host": "h", "port": 22, "user": "u",
            "auth": {"password": {"password": "p"}}
        });
        let t: SshTarget = serde_json::from_value(legacy).unwrap();
        assert!(t.jump.is_none());
        // 序列化显式写 null 键（形状可机检，不是省键）
        let out = serde_json::to_value(&t).unwrap();
        assert_eq!(out["jump"], serde_json::Value::Null);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名
    fn jump_firstHopUnknown_refuses() {
        // 首跳首见拒 → 整链拒。"第 N 跳"点名只有 hop_ctx 一处构造（唯一牌面），
        // 本臂同时是真实链路的同型断言；指纹确认三枚输入逐字不污染
        let d = tmpdir("jhop1");
        let kh = KnownHosts::open(&d).unwrap();
        let raw = tofu_verdict(&kh, "j1.example", 22, "ssh-ed25519 SHA256:j1key").unwrap_err();
        let named = hop_ctx(1, raw);
        let msg = named.to_string();
        assert!(
            msg.contains("第 1 跳 j1.example:22"),
            "跳数与主机必须连排点名，实得 {msg}"
        );
        match &named {
            TermError::HostKeyUnknown {
                descriptor,
                note,
                host,
                port,
            } => {
                assert_eq!(descriptor, "ssh-ed25519 SHA256:j1key", "指纹逐字进错误体");
                assert_eq!((host.as_str(), *port), ("j1.example", 22));
                assert_eq!(note, "第 1 跳 ");
            }
            other => panic!("必须仍是 TERM_SSH_004 臂，实得 {other:?}"),
        }
        assert_eq!(named.code(), "TERM_SSH_004");
        // 正对照：变更臂（HostKey）过 hop_ctx 逐字不变——前缀只加 Unknown 臂
        let changed = TermError::HostKey("主机密钥已变更".into());
        assert!(matches!(hop_ctx(1, changed), TermError::HostKey(m) if m == "主机密钥已变更"));
        // 整链拒=零写（裁决不落地，跳数信息不是放行通道）
        assert!(!KnownHosts::shared_path(&d).exists(), "首见零写");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-4）D-28 栏安全负例
    fn tofu_jumpSecondHop_refusesNamingHopIndex() {
        // 第二跳 TOFU 拒 = 整链拒且点名跳数——connect_hop 错误统一走
        // connect_map(Some(n))（唯一入口），指纹确认通道在第二跳同样存活
        let t = SshTarget {
            host: "j2.example".into(),
            port: 2222,
            user: "u".into(),
            auth: SshAuth::Password {
                password: "x".into(),
            },
            jump: None,
        };
        let raw = TermError::HostKeyUnknown {
            note: String::new(),
            host: "j2.example".into(),
            port: 2222,
            descriptor: "ssh-ed25519 SHA256:j2key".into(),
        };
        let mapped = connect_map(Some(2), &t, raw);
        let msg = mapped.to_string();
        assert!(msg.contains("第 2 跳 j2.example:2222"), "实得 {msg}");
        match &mapped {
            TermError::HostKeyUnknown { descriptor, .. } => {
                assert_eq!(descriptor, "ssh-ed25519 SHA256:j2key")
            }
            other => panic!("信任臂必须原样过网、不得裹成 TERM_SSH_001，实得 {other:?}"),
        }
        // 正对照：非信任错误由连接口裹主机名（第二跳 IO/认证失败同样点名主机端口）
        let wrapped = connect_map(Some(2), &t, TermError::Auth("公钥认证失败".into()));
        assert!(wrapped.to_string().contains("连接 j2.example:2222 失败"));
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名
    async fn jump_depthExceeded_errsBeforeAnyConnect() {
        let d = tmpdir("jdepth");
        let kh = Arc::new(KnownHosts::open(&d).unwrap());
        let pw = || SshAuth::Password {
            password: "x".into(),
        };
        // 4 跳 = 超限一枚（via 最深者=第 1 跳的链式形状）
        let deep = Some(hop(
            "127.0.0.1",
            1,
            "a",
            pw(),
            Some(hop(
                "127.0.0.1",
                1,
                "b",
                pw(),
                Some(hop(
                    "127.0.0.1",
                    1,
                    "c",
                    pw(),
                    Some(hop("127.0.0.1", 1, "d", pw(), None)),
                )),
            )),
        ));
        let e = conn_err(connect_via_jumps(&closed_target(deep), kh.clone()).await);
        let msg = e.to_string();
        assert!(msg.contains("跳数超限"), "实得 {msg}");
        assert!(
            msg.contains("4 跳 > 上限 3 跳"),
            "收到数与上限都要点名，实得 {msg}"
        );
        // 零 IO 证据：任何建连尝试的错误都含主机名或"失败"（connect_map 措辞），
        // 闸内消息两者皆无
        assert!(
            !msg.contains("127.0.0.1") && !msg.contains("失败"),
            "深度闸必须先于任何连接，实得 {msg}"
        );
        assert!(matches!(e, TermError::BadParam(_)));
        assert_eq!(e.code(), "TERM_SESSION_004");
        // 正对照：3 跳（=上限）过深度闸
        let three = Some(hop(
            "127.0.0.1",
            1,
            "a",
            pw(),
            Some(hop(
                "127.0.0.1",
                1,
                "b",
                pw(),
                Some(hop("127.0.0.1", 1, "c", pw(), None)),
            )),
        ));
        jump_guard(&three).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    #[allow(non_snake_case)] // 任务书字面测试名
    fn jump_authPerHop_independent() {
        // 逐跳独立凭据：Password/Key 各自经 JSON 往返保留，跨跳不复用不回落
        let outer = hop(
            "jump-b.example",
            2201,
            "ub",
            SshAuth::Key {
                key_path: "C:/keys/b_id".into(),
                passphrase: Some("pb".into()),
            },
            Some(hop(
                "jump-a.example",
                22,
                "ua",
                SshAuth::Password {
                    password: "pa".into(),
                },
                None,
            )),
        );
        let t = SshTarget {
            host: "final.example".into(),
            port: 22,
            user: "uf".into(),
            auth: SshAuth::Key {
                key_path: "/home/u/.ssh/id_f".into(),
                passphrase: None,
            },
            jump: Some(outer),
        };
        let back: SshTarget = serde_json::from_value(serde_json::to_value(&t).unwrap()).unwrap();
        let b = back.jump.as_ref().unwrap();
        assert_eq!(b.host, "jump-b.example");
        assert!(matches!(&b.auth,
            SshAuth::Key { key_path, passphrase: Some(p) }
                if key_path == "C:/keys/b_id" && p == "pb"));
        let a = b.via.as_ref().unwrap();
        assert_eq!(
            (a.host.as_str(), a.port, a.user.as_str()),
            ("jump-a.example", 22, "ua")
        );
        assert!(matches!(&a.auth, SshAuth::Password { password } if password == "pa"));
        assert!(a.via.is_none());
        assert!(matches!(
            &back.auth,
            SshAuth::Key {
                passphrase: None,
                ..
            }
        ));
        // 任务书形状：最内层 via 显式 null（不是省键）
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["jump"]["via"]["via"], serde_json::Value::Null);
    }
}
