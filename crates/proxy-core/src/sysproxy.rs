//! PR4 系统代理切换与恢复（docs/impl/05 PR4）：注册表操作经 [`SysProxyPort`](host_core::ports::SysProxyPort)，
//! 本文件只承载**备份/识别/还原**纯逻辑 —— 崩溃安全的核心。
//!
//! 还原入口（三处 + 启动扫描，缺一即断网事故）：
//! 1. panic hook（src-tauri state.rs 注册 [`crash`](host_core::crash) 恢复钩子）
//! 2. `ProxyModule::stop`（正常停用/应用退出）
//! 3. `--restore-proxy` 启动参数（lib.rs，紧急抢救入口）
//! 4. 启动扫描 [`restore_if_ours`]：kill -9 残留 → 下次启动自动恢复（验收项）

use std::path::{Path, PathBuf};

use host_core::ports::{SysProxyPort, SysProxyState};
use serde::{Deserialize, Serialize};

use crate::error::{ProxyError, Result};

const BACKUP_FILE: &str = "proxy_backup.json";

/// 首次开启前记录的原始系统代理（用户自己的代理设置，绝不能丢）
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Backup {
    ts_ms: u64,
    state: SysProxyState,
}

fn backup_path(proxy_dir: &Path) -> PathBuf {
    proxy_dir.join(BACKUP_FILE)
}

/// 我们写入的标记值：`127.0.0.1:{port}`。识别"当前系统代理是否残留我们设置"
pub fn our_server(mixed_port: u16) -> String {
    format!("127.0.0.1:{mixed_port}")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 开启系统代理前备份原值（幂等：已有备份不覆盖 —— 后续开关不刷掉用户原始设置）
pub fn backup_before_enable(proxy_dir: &Path, sp: &dyn SysProxyPort) -> Result<()> {
    if backup_path(proxy_dir).exists() {
        return Ok(());
    }
    let current = sp
        .read()
        .map_err(|e| ProxyError::SysProxy(format!("读取当前系统代理失败: {e}")))?;
    let backup = Backup { ts_ms: now_ms(), state: current };
    let tmp = backup_path(proxy_dir).with_extension("json.tmp");
    std::fs::create_dir_all(proxy_dir)?;
    std::fs::write(&tmp, serde_json::to_vec(&backup)?)?;
    std::fs::rename(&tmp, backup_path(proxy_dir))?;
    Ok(())
}

/// 启用系统代理：指向本机 mixed 入站并广播生效
pub fn enable(proxy_dir: &Path, sp: &dyn SysProxyPort, mixed_port: u16) -> Result<()> {
    backup_before_enable(proxy_dir, sp)?;
    let state = SysProxyState {
        enable: true,
        server: our_server(mixed_port),
        // 系统默认例外 + 本机直连（防自旋：应用自身的 IPC/内核流量不走代理）
        bypass: "localhost;127.*;10.*;172.16.*;172.17.*;172.18.*;172.19.*;172.20.*;172.21.*;172.22.*;172.23.*;172.24.*;172.25.*;172.26.*;172.27.*;172.28.*;172.29.*;172.30.*;172.31.*;192.168.*;<local>".into(),
    };
    sp.write(&state)
        .map_err(|e| ProxyError::SysProxy(format!("写入系统代理失败: {e}")))?;
    sp.refresh()
        .map_err(|e| ProxyError::SysProxy(format!("刷新系统代理失败: {e}")))?;
    Ok(())
}

/// 关闭/还原：恢复用户原值并广播，成功后删除备份。无备份时仅关闭开关（保守）。
pub fn restore(proxy_dir: &Path, sp: &dyn SysProxyPort) -> Result<()> {
    let backup = std::fs::read(backup_path(proxy_dir)).ok().and_then(|raw| {
        serde_json::from_slice::<Backup>(&raw).ok()
    });
    let target = match backup {
        Some(b) => {
            // 用户原始设置原样恢复（含原本就没开代理的情形：enable=false）
            b.state
        }
        None => SysProxyState { enable: false, ..Default::default() },
    };
    sp.write(&target)
        .map_err(|e| ProxyError::SysProxy(format!("还原系统代理失败: {e}")))?;
    sp.refresh()
        .map_err(|e| ProxyError::SysProxy(format!("刷新系统代理失败: {e}")))?;
    let _ = std::fs::remove_file(backup_path(proxy_dir));
    Ok(())
}

/// 静默版（panic hook / --restore-proxy 上下文）：错误仅记日志，绝不 panic
pub fn restore_quiet(proxy_dir: &Path, sp: &dyn SysProxyPort) {
    if let Err(e) = restore(proxy_dir, sp) {
        tracing::error!(error = %e, "系统代理还原失败（崩溃恢复路径）");
    }
}

/// 启动扫描：备份存在 && 当前系统代理正是我们写入的值 → kill -9 残留，还原。
/// 返回是否执行了还原（UI 提示用）。用户后来自己改过系统代理则不动。
pub fn restore_if_ours(proxy_dir: &Path, sp: &dyn SysProxyPort, mixed_port: u16) -> Result<bool> {
    if !backup_path(proxy_dir).exists() {
        return Ok(false);
    }
    let current = sp
        .read()
        .map_err(|e| ProxyError::SysProxy(format!("读取当前系统代理失败: {e}")))?;
    if current.enable && current.server == our_server(mixed_port) {
        restore(proxy_dir, sp)?;
        tracing::warn!("检测到上次运行残留的系统代理，已自动还原");
        Ok(true)
    } else {
        // 当前值不是我们的（用户改过/已关闭）：备份已失效，清理
        let _ = std::fs::remove_file(backup_path(proxy_dir));
        Ok(false)
    }
}

/// 备份是否存在（UI 状态展示）
pub fn has_backup(proxy_dir: &Path) -> bool {
    backup_path(proxy_dir).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// 内存版 SysProxyPort（免注册表单测）
    #[derive(Clone)]
    struct MockSp {
        state: Arc<Mutex<SysProxyState>>,
        refreshes: Arc<Mutex<u32>>,
    }

    impl MockSp {
        fn new() -> Self {
            Self {
                state: Arc::new(Mutex::new(SysProxyState {
                    enable: false,
                    server: String::new(),
                    bypass: "<local>".into(),
                })),
                refreshes: Arc::new(Mutex::new(0)),
            }
        }
    }

    impl SysProxyPort for MockSp {
        fn read(&self) -> std::result::Result<SysProxyState, host_core::error::AppError> {
            Ok(self.state.lock().unwrap().clone())
        }
        fn write(&self, state: &SysProxyState) -> std::result::Result<(), host_core::error::AppError> {
            *self.state.lock().unwrap() = state.clone();
            Ok(())
        }
        fn refresh(&self) -> std::result::Result<(), host_core::error::AppError> {
            *self.refreshes.lock().unwrap() += 1;
            Ok(())
        }
        fn is_admin(&self) -> bool {
            false
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        // 测试并发运行：目录必须按测试名隔离，否则备份文件互相覆盖（service.rs 同款教训）
        let d = std::env::temp_dir().join(format!("nf_proxy_sp_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn enable_backups_user_state_and_writes_ours() {
        let dir = tmpdir("enable");
        let sp = MockSp::new();
        // 用户原本开着另一个代理
        *sp.state.lock().unwrap() = SysProxyState {
            enable: true,
            server: "192.168.1.5:7890".into(),
            bypass: "<local>".into(),
        };
        enable(&dir, &sp, 7890).unwrap();
        assert!(has_backup(&dir));
        let cur = sp.read().unwrap();
        assert!(cur.enable);
        assert_eq!(cur.server, our_server(7890));
        assert!(!cur.bypass.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_recovers_original_then_clears_backup() {
        let dir = tmpdir("restore");
        let sp = MockSp::new();
        *sp.state.lock().unwrap() = SysProxyState {
            enable: true,
            server: "192.168.1.5:7890".into(),
            bypass: "<local>".into(),
        };
        enable(&dir, &sp, 7890).unwrap();
        restore(&dir, &sp).unwrap();
        let cur = sp.read().unwrap();
        assert!(cur.enable, "用户原代理应恢复为开启");
        assert_eq!(cur.server, "192.168.1.5:7890");
        assert!(!has_backup(&dir), "还原成功后备份应清除");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_without_backup_disables() {
        let dir = tmpdir("restore-nb");
        let sp = MockSp::new();
        sp.write(&SysProxyState { enable: true, server: "x:1".into(), bypass: String::new() }).unwrap();
        restore(&dir, &sp).unwrap();
        assert!(!sp.read().unwrap().enable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_is_idempotent_across_cycles() {
        let dir = tmpdir("idem");
        let sp = MockSp::new();
        *sp.state.lock().unwrap() = SysProxyState {
            enable: true,
            server: "orig:1".into(),
            bypass: String::new(),
        };
        enable(&dir, &sp, 7890).unwrap();
        enable(&dir, &sp, 7890).unwrap(); // 二次开启不得覆盖备份
        restore(&dir, &sp).unwrap();
        assert_eq!(sp.read().unwrap().server, "orig:1", "备份不应被第二次 enable 刷掉");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_scan_restores_our_residual() {
        let dir = tmpdir("scan-ours");
        let sp = MockSp::new();
        enable(&dir, &sp, 7890).unwrap();
        // 模拟 kill -9：注册表里还是我们的值
        let restored = restore_if_ours(&dir, &sp, 7890).unwrap();
        assert!(restored, "残留应被识别并还原");
        assert!(!sp.read().unwrap().enable);
        // 再扫一次：无备份，不应动作
        assert!(!restore_if_ours(&dir, &sp, 7890).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_scan_leaves_user_changed_state_alone() {
        let dir = tmpdir("scan-user");
        let sp = MockSp::new();
        enable(&dir, &sp, 7890).unwrap();
        // 用户后来自己改了系统代理
        sp.write(&SysProxyState { enable: true, server: "8.8.8.8:3128".into(), bypass: String::new() }).unwrap();
        assert!(!restore_if_ours(&dir, &sp, 7890).unwrap());
        assert_eq!(sp.read().unwrap().server, "8.8.8.8:3128");
        assert!(!has_backup(&dir), "失效备份应清理");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
