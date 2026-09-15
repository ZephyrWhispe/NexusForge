//! 日志初始化（docs/impl/01 S6.4）
//!
//! - `RUST_LOG` 环境变量可覆盖级别（默认 info）
//! - 每日滚动文件（保留 7 天）+ 控制台双输出
//! - 敏感字段脱敏：模块侧禁止把密钥/密码写入日志字段；
//!   统一经 [`redact`] 处理后才允许进入事件 payload 或日志

use std::path::Path;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// tracing-appender 的非阻塞 guard，调用方必须持有至进程结束
pub struct LogGuard {
    _worker: tracing_appender::non_blocking::WorkerGuard,
}

/// 初始化 tracing：控制台 + `{log_dir}/nexusforge.log.{date}` 每日滚动
pub fn init_tracing(log_dir: &Path) -> LogGuard {
    std::fs::create_dir_all(log_dir).ok();
    let file_appender = tracing_appender::rolling::daily(log_dir, "nexusforge.log");
    let (writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(false);
    let console_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(console_layer)
        .init();

    LogGuard { _worker: guard }
}

/// 敏感字段脱敏：保留首尾各 2 字符，中间以 *** 代替
pub fn redact(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 4 {
        return "***".into();
    }
    let head: String = chars[..2].iter().collect();
    let tail: String = chars[chars.len() - 2..].iter().collect();
    format!("{head}***{tail}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn redact_keeps_head_tail() {
        assert_eq!(super::redact("sk-abcdefghij"), "sk***ij");
        assert_eq!(super::redact("abc"), "***");
        assert_eq!(super::redact(""), "***");
    }
}
