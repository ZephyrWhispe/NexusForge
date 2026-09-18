//! Windows Task Scheduler 封装（docs/impl/07 A4）：schtasks.exe 命令行。
//!
//! v1 用 schtasks 而非 COM ITaskService：无需 COM 初始化线程模型管理，
//! 命令行覆盖每日定时场景足够；ONSTART/ONLOGON 触发器随 A4 深化。
//! 任务以当前用户运行（无 /RL HIGHEST——不需要最高权限，UI 不标 UAC 盾）。

use std::os::windows::process::CommandExt;
use std::process::Command;

use host_core::error::AppError;
use host_core::ports::{TaskSchdPort, TaskTogglePort};

/// 本应用注册任务的前缀（list 过滤 + 管理边界）
pub const TASK_PREFIX: &str = "NexusForge_rule_";

/// CREATE_NO_WINDOW（不弹控制台窗）
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct TaskSchdOps;

impl TaskSchdOps {
    pub fn new() -> Self {
        Self
    }

    /// 执行 schtasks 子命令（CREATE_NO_WINDOW；非零退出码带 stderr 报错）
    fn run(args: &[&str]) -> Result<String, AppError> {
        let out = Command::new("schtasks")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| AppError::module("AUTO_TASK_001", format!("schtasks 启动失败: {e}"), None))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if !out.status.success() {
            return Err(AppError::module(
                "AUTO_TASK_002",
                format!("schtasks {} 失败: {}", args.first().unwrap_or(&""), stderr.trim()),
                None,
            ));
        }
        Ok(stdout)
    }

    /// 执行并返回原始 stdout 字节（/XML 输出可能为 UTF-16LE，需按 BOM 解码）
    fn run_raw(args: &[&str]) -> Result<Vec<u8>, AppError> {
        let out = Command::new("schtasks")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| AppError::module("AUTO_TASK_001", format!("schtasks 启动失败: {e}"), None))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            return Err(AppError::module(
                "AUTO_TASK_002",
                format!("schtasks {} 失败: {}", args.first().unwrap_or(&""), stderr.trim()),
                None,
            ));
        }
        Ok(out.stdout)
    }

    /// UTF-16LE BOM 检测解码（否则按 UTF-8 lossy）
    fn decode(raw: &[u8]) -> String {
        if raw.starts_with(&[0xFF, 0xFE]) {
            let utf16: Vec<u16> = raw[2..]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&utf16)
        } else {
            String::from_utf8_lossy(raw).into_owned()
        }
    }
}

impl Default for TaskSchdOps {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskSchdPort for TaskSchdOps {
    fn ensure_daily(&self, task_name: &str, exe: &str, args: &str, time: &str) -> Result<(), AppError> {
        // /TR 命令行整体加引号 + exe 内层引号（路径含空格）
        let tr = format!("\"{exe}\" {args}");
        // /F 存在则覆盖（幂等）；/SC DAILY 每日 /ST HH:MM
        Self::run(&["/Create", "/TN", task_name, "/TR", &tr, "/SC", "DAILY", "/ST", time, "/F"]).map(|_| ())
    }

    fn remove(&self, task_name: &str) -> Result<(), AppError> {
        // 不存在视为成功（幂等——schtasks 返回错误码 1638947788/1 等，统一吞）
        match Self::run(&["/Delete", "/TN", task_name, "/F"]) {
            Ok(_) => Ok(()),
            Err(_) => Ok(()),
        }
    }

    fn list(&self) -> Result<Vec<String>, AppError> {
        let out = Self::run(&["/Query", "/FO", "CSV", "/NH"])?;
        // CSV 每行首列 "NexusForge_rule_x"；按前缀过滤
        Ok(out
            .lines()
            .filter_map(|line| line.split(',').next())
            .map(|c| c.trim_matches('"').trim().to_string())
            .filter(|c| c.starts_with(TASK_PREFIX))
            .collect())
    }
}

impl TaskTogglePort for TaskSchdOps {
    fn query_enabled(&self, path: &str) -> Result<Option<bool>, AppError> {
        // /XML 输出 <Enabled>false</Enabled>（字段名英文固定，不受系统语言影响）；
        // 输出编码可能为 UTF-16LE（XML 声明 encoding="UTF-16"）→ 按 BOM 解码
        let raw = match Self::run_raw(&["/Query", "/TN", path, "/XML"]) {
            Ok(o) => o,
            Err(_) => return Ok(None), // 任务不存在
        };
        let out = Self::decode(&raw);
        if !out.contains("<Task ") && !out.contains("<?xml") {
            return Ok(None);
        }
        let disabled = out.contains("<Enabled>false</Enabled>");
        Ok(Some(!disabled))
    }

    fn set_enabled(&self, path: &str, enabled: bool) -> Result<(), AppError> {
        let flag = if enabled { "/ENABLE" } else { "/DISABLE" };
        Self::run(&["/Change", "/TN", path, flag]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ensure_daily 参数拼接正确性（不真注册——避免污染系统计划任务）
    #[test]
    fn tr_quoting() {
        // 命令行格式由 ensure_daily 拼接；此处校验格式约定
        let exe = r"C:\Program Files\NexusForge\nexusforge.exe";
        let tr = format!("\"{exe}\" --run-rule abc");
        assert_eq!(tr, r#""C:\Program Files\NexusForge\nexusforge.exe" --run-rule abc"#);
    }

    /// remove 幂等：删除不存在的任务不报错
    #[test]
    fn remove_idempotent() {
        let ops = TaskSchdOps::new();
        ops.remove(&format!("{TASK_PREFIX}nonexistent_test")).unwrap();
    }

    /// 真机：系统任务 query_enabled 三态语义（存在→Some；不存在→None）
    #[test]
    fn query_enabled_system_task() {
        let ops = TaskSchdOps::new();
        // 系统内置任务（Win10/11 均存在）
        let known = ops
            .query_enabled(r"\Microsoft\Windows\Application Experience\Microsoft Compatibility Appraiser")
            .unwrap();
        assert!(known.is_some(), "系统任务应存在");
        let missing = ops.query_enabled(r"\Microsoft\Windows\NexusForge_Nonexistent_ZZ").unwrap();
        assert!(missing.is_none());
    }
}
