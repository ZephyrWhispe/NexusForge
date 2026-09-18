//! Windows 服务控制封装（docs/impl/08 WinOps W2）：sc.exe 命令行。
//!
//! sc 输出的字段名（START_TYPE/STATE）英文固定、值前的数字稳定，解析取数字
//! 不依赖系统语言（中文系统 AUTO_START 显示为「自动」等）。
//! 修改启动类型需管理员——非提权进程 sc config 返回 Access Denied（1060/5），
//! 由引擎层 requires_admin 三态拦截，正常路径不会触达。

use std::os::windows::process::CommandExt;
use std::process::Command;

use host_core::error::AppError;
use host_core::ports::{ServiceCtlPort, ServiceInfo, StartType};

/// CREATE_NO_WINDOW（不弹控制台窗）
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct ServiceOps;

impl ServiceOps {
    pub fn new() -> Self {
        Self
    }

    /// 执行 sc 子命令（CREATE_NO_WINDOW；失败带 stderr 报错）
    fn run(args: &[&str]) -> Result<String, AppError> {
        let out = Command::new("sc")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| AppError::module("SYS_SVC_001", format!("sc 启动失败: {e}"), None))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if !out.status.success() {
            return Err(AppError::module(
                "SYS_SVC_002",
                format!("sc {} 失败: {}", args.first().unwrap_or(&""), stderr.trim()),
                None,
            ));
        }
        Ok(stdout)
    }

    /// 解析 sc 输出中「字段名 : 数字 ...」行（STATE/START_TYPE；忽略本地化关键字）
    fn parse_numeric(output: &str, field: &str) -> Option<u32> {
        for line in output.lines() {
            if !line.contains(field) {
                continue;
            }
            // 取冒号后的首个 token（数字）
            let after = line.split(':').nth(1)?;
            let tok = after.split_whitespace().next()?;
            return tok.parse().ok();
        }
        None
    }
}

impl Default for ServiceOps {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceCtlPort for ServiceOps {
    fn query(&self, name: &str) -> Result<ServiceInfo, AppError> {
        // qc：启动类型（START_TYPE 行：2 AUTO / 3 DEMAND / 4 DISABLED / 5 DRIVER 等）
        let qc = Self::run(&["qc", name])?;
        let start_num = Self::parse_numeric(&qc, "START_TYPE")
            .ok_or_else(|| AppError::module("SYS_SVC_003", format!("服务 {name} START_TYPE 解析失败"), None))?;
        let start_type = match start_num {
            2 => StartType::Auto,
            3 => StartType::Manual,
            4 => StartType::Disabled,
            other => {
                // 驱动/未知类型（5-16）不属可管理范围——报错而非猜测
                return Err(AppError::module(
                    "SYS_SVC_003",
                    format!("服务 {name} 启动类型 {other} 不受支持"),
                    None,
                ));
            }
        };
        // query：运行状态（STATE 行：4 RUNNING）
        let q = Self::run(&["query", name])?;
        let running = Self::parse_numeric(&q, "STATE") == Some(4);
        Ok(ServiceInfo { name: name.to_string(), start_type, running })
    }

    fn set_start_type(&self, name: &str, st: StartType) -> Result<(), AppError> {
        // sc config 语法：start= auto|demand|disabled（等号后必须有空格）
        let v = match st {
            StartType::Auto => "auto",
            StartType::Manual => "demand",
            StartType::Disabled => "disabled",
        };
        Self::run(&["config", name, &format!("start= {v}")]).map(|_| ())
    }

    fn stop(&self, name: &str) -> Result<(), AppError> {
        // 幂等：已停止（sc stop 对 stopped 服务返回 1062）→ 视为成功
        match Self::run(&["stop", name]) {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("1062") => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn start(&self, name: &str) -> Result<(), AppError> {
        // Disabled 服务启动必失败（1058）——clear_cache 幂等语义：跳过而非报错
        let info = self.query(name)?;
        if info.start_type == StartType::Disabled {
            return Ok(());
        }
        // 已运行（sc start 对 running 服务返回 1056）→ 视为成功
        match Self::run(&["start", name]) {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("1056") => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机：系统服务查询（Dnscache 全系统存在；数字解析不受中文输出影响）
    #[test]
    fn query_system_service() {
        let ops = ServiceOps::new();
        let info = ops.query("Dnscache").unwrap();
        assert_eq!(info.name, "Dnscache");
        // Dnscache 无法被禁用（受保护），启动类型为 Auto 或 Manual
        assert!(matches!(info.start_type, StartType::Auto | StartType::Manual));
        let missing = ops.query("NexusForge_Nonexistent_SVC_ZZ");
        assert!(missing.is_err(), "不存在的服务应报错");
    }

    /// 数字行解析（英文样本 + 冒号多空格）
    #[test]
    fn parse_numeric_forms() {
        let s = "        START_TYPE         : 2    AUTO_START\r\n";
        assert_eq!(ServiceOps::parse_numeric(s, "START_TYPE"), Some(2));
        let s2 = "        STATE              : 4  RUNNING\r\n";
        assert_eq!(ServiceOps::parse_numeric(s2, "STATE"), Some(4));
        assert_eq!(ServiceOps::parse_numeric("no match", "STATE"), None);
    }
}
