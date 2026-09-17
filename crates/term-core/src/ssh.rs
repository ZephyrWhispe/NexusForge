//! T3 SSH/SFTP（docs/impl/06 T3）：russh 客户端 + TOFU known_hosts。
//!
//! - 首次连接记录指纹（TOFU）；指纹变更拒绝连接并报 TERM_SSH_002（不允许静默接受）
//! - known_hosts 存 `{appData}/term/known_hosts.json`（host:port → SHA256 指纹）
//! - 终端会话：request_pty + request_shell → 输出泵入统一 SessionState
//! - SFTP：每次操作独立 channel + sftp subsystem（v1 简化，不复用连接池）
//! - 私钥走路径引用（不复制内容进 vault）；密码由 UI 现场输入不入库

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
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

/// 连接超时
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// 认证方式（IPC 入参；密码现场输入，密钥走路径）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshAuth {
    Password { password: String },
    Key { key_path: String, passphrase: Option<String> },
}

/// 连接目标
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
}

/// known_hosts 存储（JSON：host:port → SHA256:xxxx）
pub struct KnownHosts {
    path: PathBuf,
    map: RwLock<HashMap<String, String>>,
}

impl KnownHosts {
    pub fn open(path: PathBuf) -> Result<Self> {
        let map = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };
        Ok(Self { path, map: RwLock::new(map) })
    }

    fn key(host: &str, port: u16) -> String {
        if port == 22 {
            host.to_string()
        } else {
            format!("[{host}]:{port}")
        }
    }

    /// 查询已记录指纹
    pub fn get(&self, host: &str, port: u16) -> Option<String> {
        self.map.read().expect("known_hosts 锁污染").get(&Self::key(host, port)).cloned()
    }

    /// 记录/更新指纹（UI 明确接受后调用）
    pub fn accept(&self, host: &str, port: u16, fingerprint: &str) -> Result<()> {
        self.map
            .write()
            .expect("known_hosts 锁污染")
            .insert(Self::key(host, port), fingerprint.to_string());
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(TermError::Io)?;
        }
        let data =
            serde_json::to_vec_pretty(&*self.map.read().expect("known_hosts 锁污染")).map_err(|e| {
                TermError::BadState(format!("known_hosts 序列化失败: {e}"))
            })?;
        std::fs::write(&self.path, data).map_err(TermError::Io)?;
        Ok(())
    }

    /// 删除记录（用户确认主机重建后允许重连）
    pub fn remove(&self, host: &str, port: u16) -> Result<bool> {
        let removed = self
            .map
            .write()
            .expect("known_hosts 锁污染")
            .remove(&Self::key(host, port))
            .is_some();
        if removed {
            let data =
                serde_json::to_vec_pretty(&*self.map.read().expect("known_hosts 锁污染"))
                    .map_err(|e| TermError::BadState(format!("known_hosts 序列化失败: {e}")))?;
            std::fs::write(&self.path, data).map_err(TermError::Io)?;
        }
        Ok(removed)
    }

    /// 全部记录（UI 管理：host:port → 指纹）
    pub fn entries(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .map
            .read()
            .expect("known_hosts 锁污染")
            .iter()
            .map(|(k, fp)| (k.clone(), fp.clone()))
            .collect();
        v.sort();
        v
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.read().expect("known_hosts 锁污染").len()
    }
}

/// SHA256 指纹（russh-keys 自带格式 "SHA256:base64"）
fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint()
}

/// russh Handler：TOFU 校验（指纹变更强拒绝，不允许静默接受——docs/impl/06 风险标注）
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
        let fp = fingerprint(server_public_key);
        match self.known.get(&self.host, self.port) {
            Some(recorded) if recorded == fp => Ok(true),
            Some(recorded) => Err(TermError::HostKey(format!(
                "主机 {host}:{port} 密钥已变更！记录 {recorded}，实际 {fp}。\
                 若确认主机重建，请删除已知主机记录后重连。",
                host = self.host,
                port = self.port
            ))),
            None => {
                // 首次连接：TOFU 自动记录指纹（首次确认 UI 在后续迭代）
                self.known.accept(&self.host, self.port, &fp)?;
                Ok(true)
            }
        }
    }
}

/// 认证（密码 / 私钥路径）；0.46 返回 Result<bool>
async fn authenticate(handle: &mut Handle<TofuHandler>, user: &str, auth: &SshAuth) -> Result<()> {
    let ok = match auth {
        SshAuth::Password { password } => handle
            .authenticate_password(user, password.as_str())
            .await
            .map_err(|e| TermError::Auth(format!("密码认证失败: {e}")))?,
        SshAuth::Key { key_path, passphrase } => {
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
    // connect 返回 H::Error = TermError（check_server_key 拒绝即 HostKey 详情）
    .map_err(|e| match &e {
        TermError::HostKey(_) => e,
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
    pub fn new(known_hosts_path: PathBuf) -> Result<Self> {
        Ok(Self {
            known: Arc::new(KnownHosts::open(known_hosts_path)?),
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
        let mut handle = connect_ssh(&target, self.known.clone()).await?;
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| TermError::Ssh(format!("打开会话通道失败: {e}")))?;
        channel
            .request_pty(false, "xterm-256color", cols.max(1) as u32, rows.max(1) as u32, 0, 0, &[])
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
                let mut ch = ch_in.lock().await;
                if ch.data(&data[..]).await.is_err() {
                    break;
                }
            }
        });
        // resize 泵
        let ch_r = channel.clone();
        tokio::spawn(async move {
            while let Some((c, r)) = resize_rx.recv().await {
                let mut ch = ch_r.lock().await;
                if ch.window_change(c.max(1) as u32, r.max(1) as u32, 0, 0).await.is_err() {
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
                    let mut c = ch.lock().await;
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
    pub async fn sftp_download(&self, target: &SshTarget, remote: &str, local: &Path) -> Result<u64> {
        let mut handle = connect_ssh(target, self.known.clone()).await?;
        let sftp = open_sftp(&mut handle).await?;
        let mut remote_file = sftp
            .open(remote)
            .await
            .map_err(|e| TermError::Sftp(format!("打开远程文件失败: {e}")))?;
        let mut local_file = tokio::fs::File::create(local).await.map_err(TermError::Io)?;
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
        remote_file.flush().await.map_err(|e| TermError::Sftp(e.to_string()))?;
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

    #[test]
    fn known_hosts_tofu_lifecycle() {
        let d = tmpdir("tofu");
        let kh = KnownHosts::open(d.join("known_hosts.json")).unwrap();
        assert_eq!(kh.get("srv.example", 22), None);
        kh.accept("srv.example", 22, "SHA256:abc").unwrap();
        assert_eq!(kh.get("srv.example", 22).as_deref(), Some("SHA256:abc"));
        // 非 22 端口的 host:port 键
        kh.accept("srv.example", 2222, "SHA256:def").unwrap();
        assert_eq!(kh.get("srv.example", 2222).as_deref(), Some("SHA256:def"));
        assert_eq!(kh.len(), 2);
        // 重开持久化
        let kh2 = KnownHosts::open(d.join("known_hosts.json")).unwrap();
        assert_eq!(kh2.get("srv.example", 22).as_deref(), Some("SHA256:abc"));
        assert!(kh2.remove("srv.example", 22).unwrap());
        assert!(kh2.get("srv.example", 22).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn host_key_names() {
        assert_eq!(KnownHosts::key("h", 22), "h");
        assert_eq!(KnownHosts::key("h", 2200), "[h]:2200");
    }
}
