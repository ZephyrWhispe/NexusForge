//! T1/T5 终端会话管理（docs/impl/06）。
//!
//! 统一会话模型：ConPTY（本地/WSL）与 SSH 均投影为
//! `input_tx / resize_tx / output_rx / kill` 四件套；会话表管理生命周期，
//! 输出经 8ms 批处理（单批 ≤ 64KB）发 `term.output` 事件，前端 ack 背压
//! （落后 > 4MB 暂停拉取，docs/impl/06 T2）。
//! 终端内容敏感：默认不进日志（仅记录会话 id 与错误）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, RwLock};

use host_core::events::{Event, EventBus};
use host_core::ports::{ConptyPort, PtyHandle, TermCfg};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Notify};

use crate::error::{Result, TermError};

/// 输出批处理窗口
const BATCH_WINDOW_MS: u64 = 8;
/// 单批上限（超出立即切批发送）
const BATCH_MAX: usize = 64 * 1024;
/// 背压阈值：未 ack 字节数超过即暂停读（docs/impl/06 T2）
const BACKPRESSURE_LIMIT: i64 = 4 * 1024 * 1024;

/// 会话类别（docs/impl/06 T1/T3/T5）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermKind {
    Local,
    Wsl { distro: String },
    Ssh { host: String, port: u16, user: String },
}

/// 会话元信息（IPC DTO）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub kind: TermKind,
    pub title: String,
    pub alive: bool,
    pub cols: u16,
    pub rows: u16,
}

/// 会话关闭闭包（Box<dyn FnOnce> 无 Sync；互斥锁内调用保证独占）
struct KillFn(Box<dyn FnOnce() + Send>);
unsafe impl Sync for KillFn {}

/// 会话运行态（SessionState 由 Arc 共享给 reader 任务）
pub struct SessionState {
    pub id: String,
    pub kind: TermKind,
    pub title: String,
    pub cols: u16,
    pub rows: u16,
    pub alive: AtomicBool,
    /// 已发送未 ack 字节（背压计数）
    pub unacked: AtomicI64,
    /// 累计发送字节（前端 ack 回传对账）
    pub sent_total: AtomicI64,
    input_tx: mpsc::Sender<Vec<u8>>,
    resize_tx: mpsc::Sender<(u16, u16)>,
    kill: RwLock<Option<KillFn>>,
    /// 背压恢复信号
    resume: Notify,
}

impl SessionState {
    /// 写入终端输入（UTF-8 文本/控制序列）
    pub async fn write(&self, data: Vec<u8>) -> Result<()> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(TermError::DeadSession(self.id.clone()));
        }
        self.input_tx
            .send(data)
            .await
            .map_err(|_| TermError::DeadSession(self.id.clone()))
    }

    /// 调整尺寸（与写串行化由 transport 内部保证）
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.resize_tx
            .send((cols, rows))
            .await
            .map_err(|_| TermError::DeadSession(self.id.clone()))
    }

    /// 终止会话（幂等）
    pub fn kill(&self) {
        if self.alive.swap(false, Ordering::SeqCst) {
            if let Some(k) = self.kill.write().expect("会话 kill 锁污染").take() {
                (k.0)();
            }
        }
    }

    /// 背压恢复（ack 回调）
    pub fn release(&self, acked_total: i64) {
        let sent = self.sent_total.load(Ordering::SeqCst);
        // acked 为前端累计确认值；低于已发值即未 ack 量
        let unacked = (sent - acked_total.clamp(0, sent)).max(0);
        self.unacked.store(unacked, Ordering::SeqCst);
        if unacked < BACKPRESSURE_LIMIT {
            self.resume.notify_waiters();
        }
    }

    /// reader 背压等待（unacked 超限时挂起）
    pub async fn wait_while_paused(&self) {
        while self.unacked.load(Ordering::SeqCst) >= BACKPRESSURE_LIMIT
            && self.alive.load(Ordering::SeqCst)
        {
            self.resume.notified().await;
        }
    }
}

/// 终端会话表
pub struct TermSessions {
    conpty: RwLock<Option<Arc<dyn ConptyPort>>>,
    bus: RwLock<Option<Arc<EventBus>>>,
    sessions: RwLock<HashMap<String, Arc<SessionState>>>,
}

impl TermSessions {
    pub fn new() -> Self {
        Self {
            conpty: RwLock::new(None),
            bus: RwLock::new(None),
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// 模块 init 注入
    pub fn attach(&self, conpty: Arc<dyn ConptyPort>, bus: Arc<EventBus>) {
        *self.conpty.write().expect("conpty 锁污染") = Some(conpty);
        *self.bus.write().expect("bus 锁污染") = Some(bus);
    }

    fn bus(&self) -> Result<Arc<EventBus>> {
        self.bus
            .read()
            .expect("bus 锁污染")
            .clone()
            .ok_or_else(|| TermError::BadState("事件总线未就绪".into()))
    }

    fn conpty(&self) -> Result<Arc<dyn ConptyPort>> {
        self.conpty
            .read()
            .expect("conpty 锁污染")
            .clone()
            .ok_or_else(|| TermError::BadState("ConPTY 未就绪".into()))
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        self.sessions
            .read()
            .expect("会话表锁污染")
            .values()
            .map(|s| SessionInfo {
                id: s.id.clone(),
                kind: s.kind.clone(),
                title: s.title.clone(),
                alive: s.alive.load(Ordering::SeqCst),
                cols: s.cols,
                rows: s.rows,
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Result<Arc<SessionState>> {
        self.sessions
            .read()
            .expect("会话表锁污染")
            .get(id)
            .cloned()
            .ok_or_else(|| TermError::NoSuchSession(id.to_string()))
    }

    /// 本地 shell 会话（T1；shell 为完整命令行，默认 powershell）
    pub async fn spawn_local(
        &self,
        shell: Option<String>,
        cwd: Option<PathBuf>,
        cols: u16,
        rows: u16,
    ) -> Result<SessionInfo> {
        let shell = shell.unwrap_or_else(default_shell);
        self.spawn_pty(TermKind::Local, shell_title(&shell), shell, cwd, cols, rows)
            .await
    }

    /// WSL 会话（T5）：`wsl.exe -d {distro}`
    pub async fn spawn_wsl(&self, distro: &str, cols: u16, rows: u16) -> Result<SessionInfo> {
        let shell = format!("wsl.exe -d {}", distro);
        self.spawn_pty(
            TermKind::Wsl { distro: distro.to_string() },
            format!("WSL: {distro}"),
            shell,
            None,
            cols,
            rows,
        )
        .await
    }

    async fn spawn_pty(
        &self,
        kind: TermKind,
        title: String,
        shell: String,
        cwd: Option<PathBuf>,
        cols: u16,
        rows: u16,
    ) -> Result<SessionInfo> {
        let handle = self
            .conpty()?
            .spawn(TermCfg {
                shell,
                cwd,
                cols: cols.max(1),
                rows: rows.max(1),
                env: HashMap::new(),
            })
            .map_err(|e| TermError::Pty(e.to_string()))?;
        self.register(kind, title, cols, rows, handle).await
    }

    /// 注册会话并启动输出 reader（PTY 与 SSH 共用；handle 四件套直接接线）
    pub async fn register(
        &self,
        kind: TermKind,
        title: String,
        cols: u16,
        rows: u16,
        handle: PtyHandle,
    ) -> Result<SessionInfo> {
        let id = uuid::Uuid::now_v7().to_string();
        let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(256);
        let (resize_tx, mut resize_rx) = mpsc::channel::<(u16, u16)>(16);

        // 输入转发：统一入口 input_tx → transport input
        let mut pty_input = handle.input_tx.clone();
        tokio::spawn(async move {
            while let Some(data) = input_rx.recv().await {
                if pty_input.send(data).await.is_err() {
                    break;
                }
            }
        });
        // resize 转发
        let mut pty_resize = handle.resize_tx.clone();
        tokio::spawn(async move {
            while let Some(sz) = resize_rx.recv().await {
                if pty_resize.send(sz).await.is_err() {
                    break;
                }
            }
        });

        let bus = self.bus()?;
        let state = Arc::new(SessionState {
            id: id.clone(),
            kind: kind.clone(),
            title: title.clone(),
            cols: cols.max(1),
            rows: rows.max(1),
            alive: AtomicBool::new(true),
            unacked: AtomicI64::new(0),
            sent_total: AtomicI64::new(0),
            input_tx,
            resize_tx,
            kill: RwLock::new(None),
            resume: Notify::new(),
        });

        // kill 闭包最后取（into_kill 消耗 handle；output_rx 先行 move）
        let PtyHandle { output_rx, kill, .. } = handle;
        let mut out_rx = output_rx;
        state
            .kill
            .write()
            .expect("会话 kill 锁污染")
            .replace(KillFn(kill));

        // reader：8ms 批处理 + 单批 64KB 切分 + 背压（T2）
        let reader_state = state.clone();
        tokio::spawn(async move {
            let mut window: Vec<u8> = Vec::with_capacity(BATCH_MAX);
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(BATCH_WINDOW_MS));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await; // 首跳立即返回
            loop {
                let got = tokio::select! {
                    _ = ticker.tick() => false,
                    chunk = out_rx.recv() => match chunk {
                        Some(data) => {
                            window.extend_from_slice(&data);
                            true
                        }
                        None => break, // EOF：会话结束
                    },
                };
                // 批满 / 窗口到点且有数据 → 发送
                if window.len() >= BATCH_MAX || (got && !window.is_empty()) {
                    reader_state.wait_while_paused().await;
                    if !reader_state.alive.load(Ordering::SeqCst) {
                        break;
                    }
                    let mut off = 0;
                    while off < window.len() {
                        let end = (off + BATCH_MAX).min(window.len());
                        let slice = String::from_utf8_lossy(&window[off..end]).into_owned();
                        let n = (end - off) as i64;
                        reader_state.sent_total.fetch_add(n, Ordering::SeqCst);
                        reader_state.unacked.fetch_add(n, Ordering::SeqCst);
                        bus.publish(Event::new(
                            "term.output",
                            "term",
                            serde_json::json!({
                                "session_id": reader_state.id,
                                "data": slice,
                                "seq": reader_state.sent_total.load(Ordering::SeqCst),
                            }),
                        ))
                        .ok();
                        off = end;
                    }
                    window.clear();
                }
            }
            // EOF：标记死亡 + 回收 + 发退出事件
            reader_state.alive.store(false, Ordering::SeqCst);
            if let Some(k) = reader_state.kill.write().expect("会话 kill 锁污染").take() {
                (k.0)();
            }
            bus.publish(Event::new(
                "term.exit",
                "term",
                serde_json::json!({ "session_id": reader_state.id, "code": null }),
            ))
            .ok();
        });

        self.sessions
            .write()
            .expect("会话表锁污染")
            .insert(id.clone(), state);
        // 顺带清理死会话
        self.sessions
            .write()
            .expect("会话表锁污染")
            .retain(|_, s| s.alive.load(Ordering::SeqCst));

        Ok(SessionInfo {
            id,
            kind,
            title,
            alive: true,
            cols: cols.max(1),
            rows: rows.max(1),
        })
    }

    pub fn kill_session(&self, id: &str) -> Result<()> {
        let s = self.get(id)?;
        s.kill();
        Ok(())
    }

    /// ack 背压回调（T2）
    pub fn ack(&self, id: &str, received_total: i64) -> Result<()> {
        let s = self.get(id)?;
        s.release(received_total);
        Ok(())
    }

    /// 模块 stop：强制回收全部会话（docs/impl/06 风险标注）
    pub fn kill_all(&self) {
        let mut sessions = self.sessions.write().expect("会话表锁污染");
        for s in sessions.values() {
            s.kill();
        }
        sessions.clear();
    }
}

impl Default for TermSessions {
    fn default() -> Self {
        Self::new()
    }
}

fn default_shell() -> String {
    // 默认 PowerShell（Win10 1809+ 均有 Windows PowerShell）
    if let Ok(pwsh) = std::env::var("ProgramFiles") {
        let p = PathBuf::from(pwsh).join("PowerShell\\7\\pwsh.exe");
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    "powershell.exe".to_string()
}

fn shell_title(shell: &str) -> String {
    std::path::Path::new(shell)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| shell.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_and_missing_session() {
        let s = TermSessions::new();
        assert!(s.list().is_empty());
        assert!(matches!(
            s.get("nope"),
            Err(TermError::NoSuchSession(_))
        ));
        assert!(matches!(
            s.kill_session("nope"),
            Err(TermError::NoSuchSession(_))
        ));
    }

    #[tokio::test]
    async fn default_shell_title() {
        // 纯逻辑探测：title 取 stem
        assert_eq!(shell_title("wsl.exe -d Ubuntu"), "wsl");
        assert_eq!(shell_title("powershell.exe"), "powershell");
    }

    #[tokio::test]
    async fn kill_all_on_empty_is_ok() {
        let s = TermSessions::new();
        s.kill_all();
    }
}
