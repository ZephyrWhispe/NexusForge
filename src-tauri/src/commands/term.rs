use host_core::error::AppError;
use tauri::State;

use crate::state::HostState;

// ======================== 终端与运维（M11 T，docs/impl/06） ========================

use host_core::ports::DockerPipePort;

fn term_err(e: term_core::TermError) -> AppError {
    // T-B7-1：TOFU 首见拒连时整键描述符逐字进 hint——前端确认对话框展示的
    // 就是盘上要比对的那串（永不归一化，展示面与信任面同源）
    let hint = match &e {
        term_core::TermError::HostKeyUnknown { descriptor, .. } => Some(descriptor.clone()),
        _ => None,
    };
    AppError::module(e.code(), e.to_string(), hint.as_deref())
}

/// 本地会话（T1）
#[tauri::command]
pub async fn term_spawn_local(
    shell: Option<String>,
    cwd: Option<std::path::PathBuf>,
    cols: u16,
    rows: u16,
    state: State<'_, HostState>,
) -> Result<term_core::SessionInfo, AppError> {
    state
        .term
        .sessions()
        .spawn_local(shell, cwd, cols, rows)
        .await
        .map_err(term_err)
}

/// WSL 会话（T5）
#[tauri::command]
pub async fn term_spawn_wsl(
    distro: String,
    cols: u16,
    rows: u16,
    state: State<'_, HostState>,
) -> Result<term_core::SessionInfo, AppError> {
    state
        .term
        .sessions()
        .spawn_wsl(&distro, cols, rows)
        .await
        .map_err(term_err)
}

/// WSL 分发列表（T5）
#[tauri::command]
pub async fn term_wsl_list(_state: State<'_, HostState>) -> Result<Vec<String>, AppError> {
    let distros = tokio::task::spawn_blocking(term_core::wsl::list_distros)
        .await
        .map_err(|e| AppError::module("TERM_IPC_001", e.to_string(), None))?
        .map_err(term_err)?;
    Ok(distros)
}

/// 终端输入（UTF-8；含控制序列）
#[tauri::command]
pub async fn term_write(
    session_id: String,
    data: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state
        .term
        .sessions()
        .get(&session_id)
        .map_err(term_err)?
        .write(data.into_bytes())
        .await
        .map_err(term_err)
}

/// 调整尺寸
#[tauri::command]
pub async fn term_resize(
    session_id: String,
    cols: u16,
    rows: u16,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state
        .term
        .sessions()
        .get(&session_id)
        .map_err(term_err)?
        .resize(cols, rows)
        .await
        .map_err(term_err)
}

/// 背压 ack（T2：前端回传累计已收字节数）
#[tauri::command]
pub async fn term_ack(
    session_id: String,
    received_total: i64,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    state
        .term
        .sessions()
        .ack(&session_id, received_total)
        .map_err(term_err)
}

/// 终止会话
#[tauri::command]
pub async fn term_kill(session_id: String, state: State<'_, HostState>) -> Result<(), AppError> {
    state
        .term
        .sessions()
        .kill_session(&session_id)
        .map_err(term_err)
}

/// 会话列表
#[tauri::command]
pub async fn term_sessions(
    state: State<'_, HostState>,
) -> Result<Vec<term_core::SessionInfo>, AppError> {
    Ok(state.term.sessions().list())
}

// ---- T3 SSH/SFTP ----

/// SSH 参数（auth 内联）
#[derive(serde::Deserialize)]
pub struct SshConnectDto {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: term_core::SshAuth,
    pub cols: u16,
    pub rows: u16,
    /// ProxyJump 跳链（T-B7-4）：既有连接命令扩参，零新命令；旧前端无此键可读
    #[serde(default)]
    pub jump: Option<Box<term_core::JumpHop>>,
}

/// SSH 终端会话（T3）
#[tauri::command]
pub async fn term_ssh_connect(
    conn: SshConnectDto,
    state: State<'_, HostState>,
) -> Result<term_core::SessionInfo, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    let target = term_core::SshTarget {
        host: conn.host,
        port: conn.port,
        user: conn.user,
        auth: conn.auth,
        jump: conn.jump,
    };
    ssh.open_shell(target, conn.cols, conn.rows, state.term.sessions())
        .await
        .map_err(term_err)
}

/// 已记录主机指纹列表（TOFU 管理）
#[tauri::command]
pub async fn term_ssh_known_hosts(
    state: State<'_, HostState>,
) -> Result<Vec<SshKnownHostDto>, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    Ok(ssh
        .known_hosts()
        .entries()
        .into_iter()
        .map(|(host, fingerprint)| SshKnownHostDto { host, fingerprint })
        .collect())
}

#[derive(serde::Serialize)]
pub struct SshKnownHostDto {
    pub host: String,
    pub fingerprint: String,
}

/// 删除主机指纹（用户确认主机重建后）
#[tauri::command]
pub async fn term_ssh_forget_host(
    host: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    let (h, port) = parse_host_port(&host);
    ssh.known_hosts().remove(&h, port).map_err(term_err)
}

/// TOFU 首见指纹确认（T-B7-1）：信任决定的唯一写入口。
/// `fingerprint` 必须是连接被拒时 hint 里的**整键描述符逐字**
/// （`"algo SHA256:base64"`）——接受的是这一枚指纹，不是这台主机。
#[tauri::command]
pub async fn term_ssh_fingerprint_ack(
    host: String,
    port: u16,
    fingerprint: String,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    if host.trim().is_empty() {
        return Err(term_err(term_core::TermError::BadParam(
            "主机名不得为空".into(),
        )));
    }
    if fingerprint.trim().is_empty()
        || fingerprint.lines().count() > 1
        || fingerprint.contains(['\r', '\n'])
    {
        return Err(term_err(term_core::TermError::BadParam(
            "指纹确认参数必须是单行非空的整键描述符（拒绝多行注入）".into(),
        )));
    }
    ssh.known_hosts()
        .accept(&host, port, &fingerprint)
        .map_err(term_err)
}

/// T-B7-3：`~/.ssh/config` **只读导入**候选列表（裁决：后端读——前端无 fs 权）。
/// 缺席/不可读一律空表非错误；ignored 点名留在 term-core 侧（预填面不消费）。
#[tauri::command]
pub async fn term_ssh_config_hosts() -> Result<Vec<term_core::SshHostEntry>, AppError> {
    let (entries, _ignored) = term_core::sshconfig::load_default_config();
    Ok(entries)
}

/// exec 结果线上形状（term_core::ExecResult 同名单）
#[derive(serde::Serialize)]
pub struct ExecResultDto {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// exec 超时夹取：默认 15s，上限 120s，下限 1s（0=立即超时是误用不是语义）
const EXEC_TIMEOUT_DEFAULT_MS: u64 = 15_000;
const EXEC_TIMEOUT_MAX_MS: u64 = 120_000;

/// 一次性非交互远端命令（T-B7-2）：连接入参复用 SshConnectDto 形状
/// （cols/rows 对 exec 无意义，忽略）。exit_code=None=未获退出码（超时/
/// 信号），与 0 严格区分；timed_out 是超时唯一终态证据。
#[tauri::command]
pub async fn term_ssh_exec(
    target: SshConnectDto,
    command: String,
    timeout_ms: Option<u64>,
    state: State<'_, HostState>,
) -> Result<ExecResultDto, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    let ms = timeout_ms
        .unwrap_or(EXEC_TIMEOUT_DEFAULT_MS)
        .clamp(1_000, EXEC_TIMEOUT_MAX_MS);
    let t = term_core::SshTarget {
        host: target.host,
        port: target.port,
        user: target.user,
        auth: target.auth,
        jump: target.jump,
    };
    let r = ssh
        .exec(&t, &command, std::time::Duration::from_millis(ms))
        .await
        .map_err(term_err)?;
    Ok(ExecResultDto {
        exit_code: r.exit_code,
        stdout: r.stdout,
        stderr: r.stderr,
        timed_out: r.timed_out,
    })
}

fn parse_host_port(host: &str) -> (String, u16) {
    // "[h]:port" / "h"（缺省 22）
    if let Some(rest) = host.strip_prefix('[') {
        if let Some((h, p)) = rest.split_once("]:") {
            return (h.to_string(), p.parse().unwrap_or(22));
        }
    }
    (host.to_string(), 22)
}

/// SFTP 目录列表
#[tauri::command]
pub async fn term_sftp_list(
    host: String,
    port: u16,
    user: String,
    auth: term_core::SshAuth,
    path: String,
    state: State<'_, HostState>,
) -> Result<Vec<term_core::SftpEntry>, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    let target = term_core::SshTarget {
        host,
        port,
        user,
        auth,
        // SFTP 各口维持直连腿（T-B7-4 行范围=终端连接命令，jump 接线挂批次尾台账）
        jump: None,
    };
    ssh.sftp_list(&target, &path).await.map_err(term_err)
}

/// SFTP 下载
#[tauri::command]
pub async fn term_sftp_download(
    host: String,
    port: u16,
    user: String,
    auth: term_core::SshAuth,
    remote_path: String,
    local_path: std::path::PathBuf,
    state: State<'_, HostState>,
) -> Result<u64, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    let target = term_core::SshTarget {
        host,
        port,
        user,
        auth,
        // SFTP 各口维持直连腿（T-B7-4 行范围=终端连接命令，jump 接线挂批次尾台账）
        jump: None,
    };
    ssh.sftp_download(&target, &remote_path, &local_path)
        .await
        .map_err(term_err)
}

/// SFTP 上传
#[tauri::command]
pub async fn term_sftp_upload(
    host: String,
    port: u16,
    user: String,
    auth: term_core::SshAuth,
    local_path: std::path::PathBuf,
    remote_path: String,
    state: State<'_, HostState>,
) -> Result<u64, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    let target = term_core::SshTarget {
        host,
        port,
        user,
        auth,
        // SFTP 各口维持直连腿（T-B7-4 行范围=终端连接命令，jump 接线挂批次尾台账）
        jump: None,
    };
    ssh.sftp_upload(&target, &local_path, &remote_path)
        .await
        .map_err(term_err)
}

// ---- T-B7-5 端口转发 -L/-R/-D（**红线批：端口暴露**，转发挂 SSH 会话、会话关即全拆） ----

/// 开一条转发（`term_forward_open`）：返回即终态 ForwardSpec——Listening 点名
/// 实 bind 端口 / Refused 点名原因（含 TERM_FWD_002 非回环未显式指定），
/// 不存在"先回成功再悄悄失败"的窗口；禁自动换端口
#[tauri::command]
pub async fn term_forward_open(
    session_id: String,
    spec: term_core::ForwardKind,
    state: State<'_, HostState>,
) -> Result<term_core::ForwardSpec, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    ssh.forward_open(&session_id, spec, state.term.sessions())
        .await
        .map_err(term_err)
}

/// 关一条转发（`term_forward_close`）：forward_id 全局唯一，停监听 + 撤 -R + 行摘除
#[tauri::command]
pub async fn term_forward_close(
    forward_id: String,
    state: State<'_, HostState>,
) -> Result<bool, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    ssh.forward_close(&forward_id).await.map_err(term_err)
}

/// 列转发（`term_forward_list`）：逐行真 state 回显（含被拒原因），
/// 端口占用显示失败行而非静默——前端据此渲染徽标，永不空表冒充无冲突
#[tauri::command]
pub async fn term_forward_list(
    session_id: String,
    state: State<'_, HostState>,
) -> Result<Vec<term_core::ForwardSpec>, AppError> {
    let ssh = state
        .term
        .ssh()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "SSH 服务未就绪", None))?;
    ssh.forward_list(&session_id, state.term.sessions())
        .await
        .map_err(term_err)
}

// ---- T6 Docker ----

/// 容器列表
#[tauri::command]
pub async fn term_docker_containers(
    state: State<'_, HostState>,
) -> Result<Vec<term_core::docker::DockerContainer>, AppError> {
    let docker = state
        .ports
        .get::<dyn DockerPipePort>()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "Docker 管道未注册", None))?;
    tokio::task::spawn_blocking(move || term_core::docker::containers_list(docker.as_ref()))
        .await
        .map_err(|e| AppError::module("TERM_IPC_001", e.to_string(), None))?
        .map_err(term_err)
}

/// 启动/停止容器
#[tauri::command]
pub async fn term_docker_lifecycle(
    id: String,
    start: bool,
    state: State<'_, HostState>,
) -> Result<(), AppError> {
    let docker = state
        .ports
        .get::<dyn DockerPipePort>()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "Docker 管道未注册", None))?;
    tokio::task::spawn_blocking(move || {
        term_core::docker::container_lifecycle(docker.as_ref(), &id, start)
    })
    .await
    .map_err(|e| AppError::module("TERM_IPC_001", e.to_string(), None))?
    .map_err(term_err)
}

/// 容器日志（tail 最近 N 行）
#[tauri::command]
pub async fn term_docker_logs(
    id: String,
    tail: u32,
    state: State<'_, HostState>,
) -> Result<String, AppError> {
    let docker = state
        .ports
        .get::<dyn DockerPipePort>()
        .ok_or_else(|| AppError::module("TERM_IPC_001", "Docker 管道未注册", None))?;
    tokio::task::spawn_blocking(move || {
        term_core::docker::container_logs(docker.as_ref(), &id, tail)
    })
    .await
    .map_err(|e| AppError::module("TERM_IPC_001", e.to_string(), None))?
    .map_err(term_err)
}

#[cfg(test)]
mod tests {
    use super::parse_host_port;

    #[test]
    fn bracketed_ipv6_host_keeps_explicit_port() {
        assert_eq!(
            parse_host_port("[fe80::1]:2222"),
            ("fe80::1".to_string(), 2222)
        );
    }

    #[test]
    fn malformed_port_falls_back_to_22() {
        assert_eq!(parse_host_port("[host]:abc"), ("host".to_string(), 22));
    }

    #[test]
    fn plain_or_unclosed_host_defaults_to_22() {
        assert_eq!(
            parse_host_port("example.com"),
            ("example.com".to_string(), 22)
        );
        // 未闭合括号不满足 "[h]:p" 形态：整串作为主机名
        assert_eq!(parse_host_port("[unclosed"), ("[unclosed".to_string(), 22));
    }
}
