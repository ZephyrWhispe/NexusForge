//! T-B7-5 端口转发 -L/-R/-D（**红线批：端口暴露**）。
//!
//! - 唯一出站判定口 [`bind_gate`]：回环默认臂放行；非回环必须走带显式确认位的
//!   `Other` 臂，否则 `TERM_FWD_002`（消息含"需显式指定"）——默认永不监听公网卡
//! - [`ForwardTable`] 纯进程内（重启即失=诚实）；state 三态 Listening/Refused/Closed，
//!   **禁自动换端口**：bind 撞 = Refused 行点名原因（静默换=用户以为转对了）
//! - Local/Dynamic 经 `channel_open_direct_tcpip`；Remote 发 `tcpip_forward` 全局请求
//!   （显式请求面为 `&mut Handle`，走 `RusshTunnel` 装配臂）+ 入站通道经
//!   [`FwdInbox`] 路由自 TofuHandler 的 forwarded-tcpip 钩子
//! - SOCKS5 最小子集：无认证 + CONNECT only；UDP 按协议字节 0x07 拒且点名（日志面）
//! - 会话终结（kill / 远端 EOF 两路）⇒ 全部监听器拆除（判据=端口可重 bind）

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex as AsyncMutex, Notify};

use crate::error::{Result, TermError};

/// 类型擦除的双向字节流（ChannelStream / duplex 夹具同臂）
pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}
pub type DynStream = Box<dyn AsyncStream>;

/// 绑定地址白名单（任务书形状：默认 `"127.0.0.1"` 字面 / 显式 Other 臂）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindAddr {
    /// 回环默认臂——序列化字面就是盘上/线路上的 "127.0.0.1"
    #[serde(rename = "127.0.0.1")]
    Loopback,
    /// 显式非回环：必须携带 `acknowledged: true` 确认位（红线：端口暴露是
    /// 明示决定，不是顺手填出来的地址）
    Other { addr: String, acknowledged: bool },
}

/// 唯一出站判定口：非回环且未显式指定 ⇒ `TERM_FWD_002`（含"需显式指定"）
pub fn bind_gate(addr: &BindAddr) -> Result<()> {
    match addr {
        BindAddr::Loopback => Ok(()),
        BindAddr::Other { addr, acknowledged: true } => {
            // 显式确认后仍须是可解析的 IP 字面量（主机名不放行——bind 面
            // 不接受 DNS 歧义："我确认暴露哪张网卡"必须是数字地址）
            addr
                .parse::<IpAddr>()
                .map_err(|_| TermError::ForwardBind(format!("绑定地址不是可解析的 IP 字面量: {addr}")))?;
            Ok(())
        }
        BindAddr::Other { addr, acknowledged: false } => Err(TermError::ForwardBind(format!(
            "非回环绑定 {addr} 未经显式指定（需显式指定：确认位 acknowledged=true 且为 IP 字面量）；\
             默认绑定面只有回环 \"127.0.0.1\""
        ))),
    }
}

impl BindAddr {
    /// 收口成监听用 SocketAddr（bind_gate 之后调用；Other 臂的解析在此兜底复验）
    fn socket_addr(&self, port: u16) -> Result<SocketAddr> {
        let ip = match self {
            BindAddr::Loopback => IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            BindAddr::Other { addr, .. } => addr
                .parse::<IpAddr>()
                .map_err(|_| TermError::ForwardBind(format!("绑定地址不可解析: {addr}")))?,
        };
        Ok(SocketAddr::new(ip, port))
    }

    /// Remote 臂发给服务端的绑定串（服务端语义，本机不 bind）
    fn bind_str(&self) -> String {
        match self {
            BindAddr::Loopback => "127.0.0.1".to_string(),
            BindAddr::Other { addr, .. } => addr.clone(),
        }
    }
}

/// 转发类型（三臂对外形状；serde 外部标签 snake_case）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardKind {
    /// -L：本地监听 → 隧道拨到 dest
    Local {
        listen_port: u16,
        dest_host: String,
        dest_port: u16,
    },
    /// -R：服务端监听 bind:listen_port → 入站通道拨本地 dest
    Remote {
        bind: BindAddr,
        listen_port: u16,
        dest_host: String,
        dest_port: u16,
    },
    /// -D：本地 SOCKS5（无认证 + CONNECT only）
    Dynamic { listen_port: u16 },
}

/// 转发运行态（Refused 是终态展示不是重试中；Closed 只在拆除瞬间出现）
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardState {
    Listening { bound_port: u16 },
    Refused { reason: String },
    Closed,
}

/// 一行转发（命令面回显与面板行同源）
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ForwardSpec {
    pub id: String,
    pub kind: ForwardKind,
    pub state: ForwardState,
}

/// 进程内转发表（重启即失=诚实，不落盘防"幽灵转发端口开机自开"）
#[derive(Debug, Default)]
pub struct ForwardTable {
    rows: std::sync::RwLock<Vec<ForwardSpec>>,
}

impl ForwardTable {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&self, spec: ForwardSpec) {
        self.rows.write().unwrap().push(spec);
    }

    fn set_state(&self, id: &str, state: ForwardState) {
        if let Some(r) = self.rows.write().unwrap().iter_mut().find(|r| r.id == id) {
            r.state = state;
        }
    }

    fn take(&self, id: &str) -> Option<ForwardSpec> {
        let mut w = self.rows.write().unwrap();
        let i = w.iter().position(|r| r.id == id)?;
        Some(w.remove(i))
    }

    pub fn rows(&self) -> Vec<ForwardSpec> {
        self.rows.read().unwrap().clone()
    }
}

/// 出站隧道缝（RusshTunnel 实装 = 生产臂；测试夹具 = 假拨号零真网）
#[async_trait]
pub trait Tunnel: Send + Sync + 'static {
    /// 拨一条 direct-tcpip（Local/Dynamic 的远端腿）
    async fn open_direct_tcpip(
        &self,
        host: &str,
        port: u32,
        orig_host: &str,
        orig_port: u32,
    ) -> Result<DynStream>;
    /// 请服务端开 -R 监听，返回服务端实际绑定端口
    async fn tcpip_forward(&self, addr: &str, port: u32) -> Result<u32>;
    /// 撤销 -R 监听（拆除臂尽力而为）
    async fn cancel_tcpip_forward(&self, addr: &str, port: u32) -> Result<()>;
    /// 订阅 (bind:port) 的入站转发通道流（服务端拨进来的连接）
    async fn subscribe_inbound(
        &self,
        addr: &str,
        port: u32,
    ) -> Result<mpsc::UnboundedReceiver<DynStream>>;
    /// 注销订阅（拆除臂）
    async fn unsubscribe_inbound(&self, addr: &str, port: u32);
}

type InboundRoutes = HashMap<(String, u32), mpsc::UnboundedSender<DynStream>>;

/// 入站通道路由面：TofuHandler 的 forwarded-tcpip 钩子按 (bind, port) 投递
#[derive(Clone, Default)]
pub struct FwdInbox {
    subs: Arc<AsyncMutex<InboundRoutes>>,
}

impl FwdInbox {
    /// 把服务端拨入的通道交给对应 Remote 转发腿；无订阅者返回 false（由调用方关闭通道）
    pub async fn route(&self, addr: &str, port: u32, stream: DynStream) -> bool {
        let key = (addr.to_string(), port);
        if let Some(tx) = self.subs.lock().await.get(&key) {
            return tx.send(stream).is_ok();
        }
        false
    }

    pub async fn subscribe(&self, addr: &str, port: u32) -> mpsc::UnboundedReceiver<DynStream> {
        let key = (addr.to_string(), port);
        let (tx, rx) = mpsc::unbounded_channel();
        // 同键重订阅 = 编程错误（同端口两条 Remote 腿），覆盖并留痕
        self.subs.lock().await.insert(key, tx);
        rx
    }

    pub async fn unsubscribe(&self, addr: &str, port: u32) {
        self.subs.lock().await.remove(&(addr.to_string(), port));
    }

    /// 是否有该 (bind:port) 的 Remote 腿在等入站（TofuHandler 钩子据此决定
    /// 投递还是关通道——无订阅者不静默吞连接）
    pub async fn has(&self, addr: &str, port: u32) -> bool {
        self.subs
            .lock()
            .await
            .contains_key(&(addr.to_string(), port))
    }
}

/// 一条在跑的转发：停止旗 + 收摊时需要的 Remote 撤单信息
struct Running {
    stop: Arc<AtomicBool>,
    cancel: Arc<Notify>,
    /// Remote 臂的服务端绑定 (addr, port)——收摊 cancel_tcpip_forward 用
    remote_bind: Option<(String, u32)>,
    /// accept 任务句柄：close/teardown 必须 await 它，监听器 drop 后才返回
    /// （否则端口尚未释放，重 bind 竞态）
    join: tokio::task::JoinHandle<()>,
}

/// 会话级转发组（表 + 在跑行；SshService 按 session_id 持有）
pub struct SessionForwards {
    tunnel: Arc<dyn Tunnel>,
    table: Arc<ForwardTable>,
    running: AsyncMutex<HashMap<String, Running>>,
}

impl SessionForwards {
    pub fn new(tunnel: Arc<dyn Tunnel>) -> Self {
        Self {
            tunnel,
            table: Arc::new(ForwardTable::new()),
            running: AsyncMutex::new(HashMap::new()),
        }
    }

    pub fn table(&self) -> &ForwardTable {
        &self.table
    }

    /// 开一条转发：**bind_gate 与 bind/请求全在返回前完成**——调用方拿到的
    /// ForwardSpec 就是终态（Listening 点名实bind端口 / Refused 点名原因），不存在
    /// "先回成功再悄悄失败"的窗口。禁自动换端口。
    pub async fn open(&self, kind: ForwardKind) -> Result<ForwardSpec> {
        let id = uuid::Uuid::now_v7().to_string();
        let spec = match &kind {
            ForwardKind::Local { listen_port, .. } | ForwardKind::Dynamic { listen_port } => {
                self.start_listener(id.clone(), kind.clone(), *listen_port)
                    .await
            }
            ForwardKind::Remote {
                bind, listen_port, ..
            } => {
                bind_gate(bind)?; // 出站判定口：非回环未显式指定 → 整条拒，零请求上隧道
                self.start_remote(id.clone(), kind.clone(), bind.bind_str(), *listen_port)
                    .await
            }
        };
        self.table.push(spec.clone());
        Ok(spec)
    }

    async fn start_listener(&self, id: String, kind: ForwardKind, listen_port: u16) -> ForwardSpec {
        let socks5 = matches!(kind, ForwardKind::Dynamic { .. });
        let (bind_addr, dest) = match &kind {
            ForwardKind::Local {
                dest_host,
                dest_port,
                ..
            } => (BindAddr::Loopback, Some((dest_host.clone(), *dest_port))),
            ForwardKind::Dynamic { .. } => (BindAddr::Loopback, None),
            ForwardKind::Remote { .. } => unreachable!("start_listener 不吃 Remote 臂"),
        };
        let bind = match bind_addr.socket_addr(listen_port) {
            Ok(b) => b,
            Err(e) => return refused(id, kind, e),
        };
        let listener = match TcpListener::bind(bind).await {
            Ok(l) => l,
            Err(e) => {
                return refused(
                    id,
                    kind,
                    TermError::Forward(format!(
                        "bind {bind} 失败: {e}——不自动换端口（静默换端口=你以为转对了），\
                         换 listen_port 重来或先释放占用"
                    )),
                )
            }
        };
        let bound = listener
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(listen_port);
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(Notify::new());
        let tunnel = self.tunnel.clone();
        let id_task = id.clone();
        let (stop_t, cancel_t) = (stop.clone(), cancel.clone());
        let join = tokio::spawn(async move {
            loop {
                // 先登记唤醒许可再查旗——杜绝"stop 与 notified 注册之间"的丢唤醒
                let w = cancel_t.notified();
                tokio::pin!(w);
                w.as_mut().enable();
                if stop_t.load(Ordering::Relaxed) {
                    break;
                }
                tokio::select! {
                    _ = w.as_mut() => break,
                    r = listener.accept() => match r {
                        Ok((sock, orig)) => {
                            let tunnel = tunnel.clone();
                            let (dest2, socks5) = (dest.clone(), socks5);
                            let id2 = id_task.clone();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    serve_local_conn(tunnel, sock, orig, dest2, socks5).await
                                {
                                    tracing::warn!("转发腿结束（点名 {id2}）: {e}");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!("accept 失败（{id_task}）: {e}");
                            if stop_t.load(Ordering::Relaxed) {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                    },
                }
            }
            // listener 在此 drop：端口即刻可重 bind（拆除判据面）
        });
        self.running.lock().await.insert(
            id.clone(),
            Running {
                stop,
                cancel,
                remote_bind: None,
                join,
            },
        );
        ForwardSpec {
            id,
            kind,
            state: ForwardState::Listening { bound_port: bound },
        }
    }

    async fn start_remote(
        &self,
        id: String,
        kind: ForwardKind,
        addr: String,
        port: u16,
    ) -> ForwardSpec {
        let dest = match &kind {
            ForwardKind::Remote {
                dest_host,
                dest_port,
                ..
            } => (dest_host.clone(), *dest_port),
            _ => unreachable!(),
        };
        let bound = match self.tunnel.tcpip_forward(&addr, port as u32).await {
            Ok(b) => b,
            Err(e) => return refused(id, kind, e),
        };
        let mut inbound = match self.tunnel.subscribe_inbound(&addr, bound).await {
            Ok(rx) => rx,
            Err(e) => return refused(id, kind, e),
        };
        // 服务端回执端口必须落进 u16 展示面；越界=如实 Refused（不假装端口存在）
        let Ok(bound_port) = u16::try_from(bound) else {
            let _ = self.tunnel.cancel_tcpip_forward(&addr, bound).await;
            return refused(
                id,
                kind,
                TermError::Forward(format!("服务端回执的绑定端口越界: {bound}")),
            );
        };
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(Notify::new());
        let tunnel = self.tunnel.clone();
        let (stop_r, cancel_r) = (stop.clone(), cancel.clone());
        let addr_task = addr.clone();
        let join = tokio::spawn(async move {
            loop {
                let w = cancel_r.notified();
                tokio::pin!(w);
                w.as_mut().enable();
                if stop_r.load(Ordering::Relaxed) {
                    break;
                }
                tokio::select! {
                    _ = w.as_mut() => break,
                    ch = inbound.recv() => match ch {
                        Some(stream) => {
                            let dest = dest.clone();
                            tokio::spawn(async move {
                                match TcpStream::connect((dest.0.as_str(), dest.1)).await {
                                    Ok(local) => relay(stream, local, &dest).await,
                                    Err(e) => tracing::warn!(
                                        "Remote 腿拨本地 {}:{} 失败: {e}",
                                        dest.0, dest.1
                                    ),
                                }
                            });
                        }
                        None => break,
                    },
                }
            }
            // 收摊：注销订阅（会话还活着时的显式撤单走 close() 的 cancel 臂）
            let _ = tunnel.unsubscribe_inbound(&addr_task, bound).await;
        });
        self.running.lock().await.insert(
            id.clone(),
            Running {
                stop,
                cancel,
                remote_bind: Some((addr.clone(), bound)),
                join,
            },
        );
        ForwardSpec {
            id,
            kind,
            state: ForwardState::Listening { bound_port },
        }
    }

    /// 关一条转发：停腿 → 撤单（Remote）→ 行移除。不存在"表里没了但还在监听"
    pub async fn close(&self, id: &str) -> Result<bool> {
        let running = self.running.lock().await.remove(id);
        let Some(r) = running else {
            return Ok(self.table.take(id).is_some());
        };
        r.stop.store(true, Ordering::SeqCst);
        r.cancel.notify_waiters();
        // 等 accept 任务真正退出：listener 在其作用域末尾 drop，端口即刻可重 bind
        // （不 await 则拆除竞态——见 fwd_sessionClose_tearsDownAllListeners 判据）
        let _ = r.join.await;
        if let Some((addr, port)) = &r.remote_bind {
            let _ = self.tunnel.cancel_tcpip_forward(addr, *port).await;
            self.tunnel.unsubscribe_inbound(addr, *port).await;
        }
        // 行终态先记 Closed 再摘除（面板闪一帧诚实终态；list 后不见此条）
        self.table.set_state(id, ForwardState::Closed);
        self.table.take(id);
        Ok(true)
    }

    /// 会话拆除（kill / 远端 EOF 两路共用）：全停全撤清空
    pub async fn teardown(&self) {
        let ids: Vec<String> = self.running.lock().await.keys().cloned().collect();
        for id in ids {
            let _ = self.close(&id).await;
        }
    }
}

fn refused(id: String, kind: ForwardKind, e: TermError) -> ForwardSpec {
    ForwardSpec {
        id,
        kind,
        state: ForwardState::Refused {
            reason: e.to_string(),
        },
    }
}

async fn serve_local_conn(
    tunnel: Arc<dyn Tunnel>,
    mut sock: TcpStream,
    orig: SocketAddr,
    dest: Option<(String, u16)>,
    socks5: bool,
) -> Result<()> {
    let (host, port) = if socks5 {
        let target = socks5_handshake(&mut sock).await?;
        // 握手后禁止信任任何未点名目标（拒臂在 handshake 内已 Err 早退）
        target
    } else {
        let (h, p) = dest.expect("Local/Dynamic 二选一臂：非 SOCKS5 必有 dest");
        (h, p)
    };
    let mut chan = tunnel
        .open_direct_tcpip(
            &host,
            port as u32,
            &orig.ip().to_string(),
            orig.port() as u32,
        )
        .await
        .map_err(|e| TermError::Forward(format!("拨向 {host}:{port} 的隧道通道失败: {e}")))?;
    let (a, b) = tokio::io::copy_bidirectional(&mut sock, &mut chan)
        .await
        .map_err(|e| TermError::Forward(format!("转发中继结束: {e}")))?;
    tracing::debug!("转发腿 {host}:{port} 收束，字节 {a}/{b}");
    Ok(())
}

async fn relay(mut a: DynStream, mut b: TcpStream, dest: &(String, u16)) {
    match tokio::io::copy_bidirectional(&mut a, &mut b).await {
        Ok((x, y)) => tracing::debug!("Remote 腿 {}:{} 收束，字节 {x}/{y}", dest.0, dest.1),
        Err(e) => tracing::warn!("Remote 腿 {}:{} 中继失败: {e}", dest.0, dest.1),
    }
}

/// SOCKS5 最小子集握手（RFC1928 形状）：无认证 + CONNECT only。
/// UDP_ASSOCIATE 按命令位 0x07 拒**且点名**；返回 CONNECT 目标。
async fn socks5_handshake<S>(s: &mut S) -> Result<(String, u16)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let ver = s.read_u8().await.map_err(io_fwd("读 SOCKS5 版本"))?;
    if ver != 5 {
        return Err(TermError::Forward(format!(
            "SOCKS5 版本字节应为 0x05，实收 {ver:#04x}——拒（本实现只做 SOCKS5 最小子集，不猜协议）"
        )));
    }
    let nmethods = s.read_u8().await.map_err(io_fwd("读方法数"))?;
    let mut methods = vec![0u8; nmethods as usize];
    s.read_exact(&mut methods)
        .await
        .map_err(io_fwd("读方法表"))?;
    if !methods.contains(&0) {
        return Err(TermError::Forward(
            "SOCKS5 方法协商：客户端不提供 no-auth(0x00)——本实现不做认证协商，拒".into(),
        ));
    }
    s.write_all(&[5, 0]).await.map_err(io_fwd("写方法选择"))?;

    let _ver = s.read_u8().await.map_err(io_fwd("读请求版本"))?;
    let cmd = s.read_u8().await.map_err(io_fwd("读命令"))?;
    let _rsv = s.read_u8().await.map_err(io_fwd("读保留位"))?;
    let atyp = s.read_u8().await.map_err(io_fwd("读地址类型"))?;
    let host = match atyp {
        1 => {
            let mut ip = [0u8; 4];
            s.read_exact(&mut ip).await.map_err(io_fwd("读 IPv4"))?;
            IpAddr::V4(std::net::Ipv4Addr::from(ip)).to_string()
        }
        3 => {
            let len = s.read_u8().await.map_err(io_fwd("读域名长度"))?;
            let mut buf = vec![0u8; len as usize];
            s.read_exact(&mut buf).await.map_err(io_fwd("读域名"))?;
            String::from_utf8_lossy(&buf).into_owned()
        }
        4 => {
            let mut ip = [0u8; 16];
            s.read_exact(&mut ip).await.map_err(io_fwd("读 IPv6"))?;
            IpAddr::V6(std::net::Ipv6Addr::from(ip)).to_string()
        }
        other => {
            // 回复码 0x08 = address type not supported
            reject_socks5(s, 0x08).await;
            return Err(TermError::Forward(format!(
                "SOCKS5 地址类型 {other:#04x} 不支持（只认 IPv4/域名/IPv6）——已按协议点名拒绝"
            )));
        }
    };
    let port = s.read_u16().await.map_err(io_fwd("读端口"))?;
    match cmd {
        1 => {}
        3 => {
            // 回复码 0x07 = command not supported——UDP 关联臂**点名拒**（不是沉默）
            reject_socks5(s, 0x07).await;
            return Err(TermError::Forward(
                "SOCKS5 UDP_ASSOCIATE 不支持——本实现只做 CONNECT（TCP）中继，\
                 UDP 转发需按目标另立设计，拒且已按协议回 0x07"
                    .into(),
            ));
        }
        other => {
            reject_socks5(s, 0x07).await;
            return Err(TermError::Forward(format!(
                "SOCKS5 命令 {other:#04x} 不支持（只认 CONNECT=0x01）——已按协议点名拒绝"
            )));
        }
    }
    // CONNECT 成功回包：VER=5 REP=0 RSV=0 ATYP=1 BND.ADDR 零字节 BND.PORT 零
    // （隧道侧不承诺源地址，按规范零填充）
    s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
        .await
        .map_err(io_fwd("写 CONNECT 成功"))?;
    Ok((host, port))
}

async fn reject_socks5<S: AsyncWrite + Unpin>(s: &mut S, rep: u8) {
    let _ = s.write_all(&[5, rep, 0, 1, 0, 0, 0, 0, 0, 0]).await;
    let _ = s.flush().await;
}

fn io_fwd<'a>(stage: &'a str) -> impl FnOnce(std::io::Error) -> TermError + 'a {
    move |e: std::io::Error| TermError::Forward(format!("SOCKS5 握手{stage}失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    /// 假隧道：记录拨号/撤单，dial 交回 duplex 的对面（测试握其手端）
    struct FakeTunnel {
        dials: AsyncMutex<Vec<(String, u32, String, u32)>>,
        forwards: AsyncMutex<Vec<(String, u32)>>,
        cancels: AsyncMutex<Vec<(String, u32)>>,
        dialed: AsyncMutex<Option<mpsc::UnboundedSender<DynStream>>>,
        inbox: FwdInbox,
    }

    impl FakeTunnel {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                dials: AsyncMutex::new(Vec::new()),
                forwards: AsyncMutex::new(Vec::new()),
                cancels: AsyncMutex::new(Vec::new()),
                dialed: AsyncMutex::new(None),
                inbox: FwdInbox::default(),
            })
        }
    }

    #[async_trait]
    impl Tunnel for FakeTunnel {
        async fn open_direct_tcpip(
            &self,
            host: &str,
            port: u32,
            orig_host: &str,
            orig_port: u32,
        ) -> Result<DynStream> {
            self.dials.lock().await.push((
                host.to_string(),
                port,
                orig_host.to_string(),
                orig_port,
            ));
            let (ours, theirs) = duplex(1024);
            if let Some(tx) = &*self.dialed.lock().await {
                let _ = tx.send(Box::new(theirs) as DynStream);
            }
            Ok(Box::new(ours))
        }
        async fn tcpip_forward(&self, addr: &str, port: u32) -> Result<u32> {
            self.forwards.lock().await.push((addr.to_string(), port));
            Ok(port) // 假服务端就绑所请求端口
        }
        async fn cancel_tcpip_forward(&self, addr: &str, port: u32) -> Result<()> {
            self.cancels.lock().await.push((addr.to_string(), port));
            Ok(())
        }
        async fn subscribe_inbound(
            &self,
            addr: &str,
            port: u32,
        ) -> Result<mpsc::UnboundedReceiver<DynStream>> {
            Ok(self.inbox.subscribe(addr, port).await)
        }
        async fn unsubscribe_inbound(&self, addr: &str, port: u32) {
            self.inbox.unsubscribe(addr, port).await;
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-5）字面测试名优先于 rustc 命名惯例
    fn bindGate_defaultLoopback_allows_nonLoopbackRequiresExplicit() {
        // 正臂：回环默认放行
        bind_gate(&BindAddr::Loopback).unwrap();
        // 红线臂：非回环未显式指定 ⇒ TERM_FWD_002 且消息含"需显式指定"
        let e = bind_gate(&BindAddr::Other {
            addr: "10.1.2.3".into(),
            acknowledged: false,
        })
        .unwrap_err();
        assert_eq!(e.code(), "TERM_FWD_002");
        assert!(e.to_string().contains("需显式指定"), "实得 {e}");
        // 显式确认后放行；但垃圾串（不可解析/主机名）仍拒——确认位不是万金油
        bind_gate(&BindAddr::Other {
            addr: "10.1.2.3".into(),
            acknowledged: true,
        })
        .unwrap();
        let e2 = bind_gate(&BindAddr::Other {
            addr: "evil.example".into(),
            acknowledged: true,
        })
        .unwrap_err();
        assert_eq!(e2.code(), "TERM_FWD_002");
        // 线路形状白名单：默认臂的字面序列化就是 "127.0.0.1"
        assert_eq!(
            serde_json::to_value(&BindAddr::Loopback).unwrap(),
            serde_json::json!("127.0.0.1")
        );
        let back: BindAddr = serde_json::from_value(serde_json::json!("127.0.0.1")).unwrap();
        assert_eq!(back, BindAddr::Loopback);
        // 第三形态（裸 "0.0.0."开头的对象缺确认位）反序列化即显式形状缺失
        let other: BindAddr = serde_json::from_value(serde_json::json!(
            {"other": {"addr": "10.1.2.3", "acknowledged": true}}
        ))
        .unwrap();
        assert!(matches!(
            other,
            BindAddr::Other {
                acknowledged: true,
                ..
            }
        ));
        assert!(
            serde_json::from_value::<BindAddr>(serde_json::json!({"other": {"addr": "x"}}))
                .is_err()
        );
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名
    async fn fwd_portConflict_reportsRefusedState() {
        // 撞已有监听 ⇒ state=Refused（不 panic、**不换端口**、原因点名）
        let squatter = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = squatter.local_addr().unwrap().port();
        let sf = SessionForwards::new(FakeTunnel::new());
        let spec = sf
            .open(ForwardKind::Local {
                listen_port: port,
                dest_host: "h".into(),
                dest_port: 80,
            })
            .await
            .unwrap();
        match &spec.state {
            ForwardState::Refused { reason } => {
                assert!(
                    reason.contains(&format!("127.0.0.1:{port}")),
                    "实得 {reason}"
                );
                assert!(reason.contains("不自动换端口"), "实得 {reason}");
            }
            other => panic!("撞端口必须落 Refused 行，实得 {other:?}"),
        }
        // 面板数据源=表行：Refused 行在表里（不是静默消失）
        assert_eq!(sf.table().rows().len(), 1);
        assert_eq!(sf.table().rows()[0].id, spec.id);
    }

    async fn open_dynamic(sf: &SessionForwards) -> u16 {
        let spec = sf
            .open(ForwardKind::Dynamic { listen_port: 0 })
            .await
            .unwrap();
        match spec.state {
            ForwardState::Listening { bound_port } => bound_port,
            other => panic!("Dynamic 开放应 Listening，实得 {other:?}"),
        }
    }

    /// 完成 SOCKS5 问候协商，返回连接句柄
    async fn socks_greet(port: u16) -> TcpStream {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(&[5, 1, 0]).await.unwrap();
        let mut b = [0u8; 2];
        s.read_exact(&mut b).await.unwrap();
        assert_eq!(b, [5, 0], "方法协商必须选中 no-auth");
        s
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名
    async fn socks5_greetingConnectRoundtrip() {
        let fake = FakeTunnel::new();
        let (dial_tx, mut dial_rx) = mpsc::unbounded_channel::<DynStream>();
        fake.dialed.lock().await.replace(dial_tx);
        let sf = SessionForwards::new(fake.clone());
        let port = open_dynamic(&sf).await;

        let mut s = socks_greet(port).await;
        // CONNECT example.com:80（ATYP=3 域名臂）
        s.write_all(&[
            5, 1, 0, 3, 11, b'e', b'x', b'a', b'm', b'p', b'l', b'e', b'.', b'c', b'o', b'm', 0, 80,
        ])
        .await
        .unwrap();
        let mut rep = [0u8; 10];
        s.read_exact(&mut rep).await.unwrap();
        assert_eq!(&rep[..4], &[5, 0, 0, 1], "CONNECT 成功回包（BND 零填充）");
        // 拨号点名目标 + 起算方地址
        let dials = fake.dials.lock().await;
        assert_eq!(dials[0].0, "example.com");
        assert_eq!(dials[0].1, 80);
        drop(dials);
        // 数据往返：隧道端与客户端互为镜像（假端 echo 一次）
        let mut theirs = dial_rx.recv().await.expect("dial 应有通道交回");
        let echo = async {
            let mut b = [0u8; 4];
            theirs.read_exact(&mut b).await.unwrap();
            assert_eq!(&b, b"ping");
            theirs.write_all(b"pong").await.unwrap();
            theirs.flush().await.unwrap();
        };
        let client = async {
            s.write_all(b"ping").await.unwrap();
            s.flush().await.unwrap();
            let mut b = [0u8; 4];
            s.read_exact(&mut b).await.unwrap();
            assert_eq!(&b, b"pong");
        };
        tokio::join!(echo, client);
        sf.teardown().await;
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名
    async fn socks5_udpRefusedNaming() {
        let fake = FakeTunnel::new();
        let sf = SessionForwards::new(fake.clone());
        let port = open_dynamic(&sf).await;
        let mut s = socks_greet(port).await;
        // UDP_ASSOCIATE (cmd=0x03) 到 IPv4:port
        s.write_all(&[5, 3, 0, 1, 1, 2, 3, 4, 0, 53]).await.unwrap();
        let mut rep = [0u8; 10];
        s.read_exact(&mut rep).await.unwrap();
        assert_eq!(
            rep[1], 0x07,
            "UDP 臂必须按协议回 0x07 command-not-supported"
        );
        // 点名拒：零拨号（UDP 从未被当作可中继目标）
        assert!(fake.dials.lock().await.is_empty());
        sf.teardown().await;
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名
    async fn fwd_sessionClose_tearsDownAllListeners() {
        // 泄漏红线：拆除后端口必须可重 bind
        let fake = FakeTunnel::new();
        let sf = SessionForwards::new(fake);
        let spec = sf
            .open(ForwardKind::Local {
                listen_port: 0,
                dest_host: "d".into(),
                dest_port: 9,
            })
            .await
            .unwrap();
        let bound = match spec.state {
            ForwardState::Listening { bound_port } => bound_port,
            other => panic!("实得 {other:?}"),
        };
        sf.teardown().await;
        assert!(sf.table().rows().is_empty(), "拆除后转发表清空");
        let reborn = TcpListener::bind(("127.0.0.1", bound)).await;
        assert!(
            reborn.is_ok(),
            "会话终结后端口 {bound} 必须可重 bind（监听器泄漏红线）"
        );
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书字面测试名
    async fn remoteForward_sendsTcpipForwardRequest() {
        // -R 帧断言走缝：tcpip_forward 请求参数逐字点名（默认回环臂=bind 字面
        // "127.0.0.1"），收摊必须 cancel 同键
        let fake = FakeTunnel::new();
        let sf = SessionForwards::new(fake.clone());
        let spec = sf
            .open(ForwardKind::Remote {
                bind: BindAddr::Loopback,
                listen_port: 14_321,
                dest_host: "127.0.0.1".into(),
                dest_port: 8080,
            })
            .await
            .unwrap();
        assert_eq!(
            spec.state,
            ForwardState::Listening { bound_port: 14_321 },
            "服务端回执端口即行 state 的 bound_port"
        );
        assert_eq!(
            *fake.forwards.lock().await,
            vec![("127.0.0.1".to_string(), 14_321u32)]
        );
        // 显式未确认的 Remote bind 在**请求上隧道之前**被闸拒（零 tcpip_forward）
        let e = sf
            .open(ForwardKind::Remote {
                bind: BindAddr::Other {
                    addr: "10.9.8.7".into(),
                    acknowledged: false,
                },
                listen_port: 14_322,
                dest_host: "x".into(),
                dest_port: 1,
            })
            .await
            .unwrap_err();
        assert_eq!(e.code(), "TERM_FWD_002");
        assert_eq!(fake.forwards.lock().await.len(), 1, "被闸拒的臂零出站");
        sf.teardown().await;
        assert_eq!(
            *fake.cancels.lock().await,
            vec![("127.0.0.1".to_string(), 14_321u32)]
        );
    }
}
