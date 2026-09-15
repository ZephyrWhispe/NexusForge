//! 崩溃恢复（docs/impl/01 S6.5）
//!
//! - panic hook：写崩溃文件（堆栈 + 模块 id + 时间）→ 执行已注册恢复钩子
//!   （如还原系统代理）→ 交还默认 hook 重新输出 panic
//! - 启动扫描：上一次崩溃痕迹、`pending_ops/` 未完成操作（文件操作断点续传的恢复入口）

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 恢复钩子签名（如：还原系统代理注册表设置）
pub type RecoveryHook = Arc<dyn Fn() + Send + Sync>;

static HOOKS: Mutex<Vec<RecoveryHook>> = Mutex::new(Vec::new());

/// 注册恢复钩子（幂等：同一指针不去重，调用方自行保证）
pub fn add_recovery_hook(hook: RecoveryHook) {
    HOOKS.lock().expect("恢复钩子表写锁").push(hook);
}

/// 安装全局 panic hook。必须在应用最早期调用（早于任何模块 start）。
pub fn install_panic_hook(app_data_dir: PathBuf) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = app_data_dir.join("crash");
        let _ = std::fs::create_dir_all(&dir);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let payload = payload_of(info);
        let report = serde_json::json!({
            "ts": ts,
            "payload": payload,
            "location": info.location().map(|l| l.to_string()),
            "threads": std::thread::current().name(),
            "recent_log_hint": "详见 log/ 目录当日日志末尾 50 行",
        });
        let _ = std::fs::write(
            dir.join(format!("crash_{ts}.json")),
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        );
        // 恢复钩子尽力执行（单个钩子失败不影响其余）
        for hook in HOOKS.lock().expect("恢复钩子表读锁").iter() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook()));
        }
        default_hook(info);
    }));
}

fn payload_of(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else {
        "unknown panic payload".into()
    }
}

/// 启动期扫描结果（UI 据此提示恢复选项）
#[derive(Debug, Default, serde::Serialize)]
pub struct StartupReport {
    /// 上次未正常退出（crash/ 目录非空）
    pub crashed_last_run: bool,
    /// 最近一次崩溃文件（如有）
    pub last_crash_file: Option<PathBuf>,
    /// `pending_ops/` 中未完成的操作数
    pub pending_ops: usize,
}

pub fn scan_startup(app_data_dir: &Path) -> StartupReport {
    let mut report = StartupReport::default();
    let crash_dir = app_data_dir.join("crash");
    if crash_dir.exists() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&crash_dir)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        files.sort();
        if !files.is_empty() {
            report.crashed_last_run = true;
            report.last_crash_file = files.pop();
        }
    }
    let pending = app_data_dir.join("pending_ops");
    report.pending_ops = std::fs::read_dir(&pending)
        .map(|rd| rd.filter_map(|e| e.ok()).count())
        .unwrap_or(0);
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_reports_crash_and_pending() {
        let dir = std::env::temp_dir().join(format!("nf_crash_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("crash")).unwrap();
        std::fs::create_dir_all(dir.join("pending_ops")).unwrap();
        std::fs::write(dir.join("crash").join("crash_1.json"), "{}").unwrap();
        std::fs::write(dir.join("pending_ops").join("op1.json"), "{}").unwrap();

        let report = scan_startup(&dir);
        assert!(report.crashed_last_run);
        assert_eq!(report.pending_ops, 1);
        assert!(report.last_crash_file.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn panic_hook_writes_crash_file_and_runs_hooks() {
        let dir = std::env::temp_dir().join(format!("nf_hook_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        install_panic_hook(dir.clone());
        let fired = Arc::new(Mutex::new(false));
        let fired2 = fired.clone();
        add_recovery_hook(Arc::new(move || *fired2.lock().unwrap() = true));

        let result = std::panic::catch_unwind(|| panic!("hook 测试 panic"));
        assert!(result.is_err(), "panic 应继续传播");

        let crash_dir = dir.join("crash");
        let files: Vec<PathBuf> = std::fs::read_dir(&crash_dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        assert!(!files.is_empty(), "崩溃文件应已写入");
        assert!(*fired.lock().unwrap(), "恢复钩子应已执行");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
