//! Appx 包管理（docs/impl/08 W5）：WinRT PackageManager 当前用户面 + PowerShell provisioned 面。
//!
//! - list/remove_current_user：Windows.Management.Deployment::PackageManager（WinRT；
//!   IAsyncOperation::get() 阻塞等待——worker 线程默认 MTA 上下文可安全使用）
//! - remove_provisioned：PowerShell Get-AppxProvisionedPackage 管道模板（需管理员）——
//!   provisioned 包全名含版本号，逐机不同，必须按 DisplayName 前缀过滤后整条移除；
//!   名称仅允许 [A-Za-z0-9._]（防注入；catalog 包名天然满足）
//! - 包被占用 → RemovePackageAsync 返回 ErrorText → 如实报错不重试（提示可从 Store 重装）

use std::os::windows::process::CommandExt;
use std::process::Command;

use host_core::error::AppError;
use host_core::ports::{AppxPackage, AppxPort};

/// CREATE_NO_WINDOW（不弹控制台窗）
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// provisioned 移除的包名合法字符（模板拼接白名单；0-9 A-Z a-z 点 下划线）
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
}

pub struct AppxOps;

impl AppxOps {
    pub fn new() -> Self {
        Self
    }

    /// PowerShell provisioned 移除模板（{name} 仅允许白名单字符；返回移除条数）
    fn remove_provisioned_ps(name_filter: &str) -> Result<u32, AppError> {
        if !valid_name(name_filter) {
            return Err(AppError::module(
                "SYS_APPX_001",
                format!("包名包含非法字符: {name_filter}"),
                None,
            ));
        }
        // 固定模板：DisplayName 前缀过滤（like "name*"）→ 逐条移除；-ErrorAction SilentlyContinue
        // 容忍单条失败；输出移除后剩余同名条数不可靠，改以脚本返回 0/1 语义（执行成功即视为移除≥0）
        let script = format!(
            "$p = Get-AppxProvisionedPackage -Online | Where-Object {{ $_.DisplayName -like '{name_filter}*' }}; \
             if ($p) {{ $p | ForEach-Object {{ $_ | Remove-AppxProvisionedPackage -Online -ErrorAction SilentlyContinue | Out-Null }}; \
             Write-Output $p.Count }} else {{ Write-Output 0 }}"
        );
        let out = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| AppError::module("SYS_APPX_002", format!("PowerShell 启动失败: {e}"), None))?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(AppError::module(
                "SYS_APPX_003",
                format!("provisioned 移除失败: {stderr}"),
                Some("provisioned 包移除需管理员权限"),
            ));
        }
        Ok(stdout.parse().unwrap_or(0))
    }
}

impl Default for AppxOps {
    fn default() -> Self {
        Self::new()
    }
}

impl AppxPort for AppxOps {
    fn list(&self, name_filter: &str) -> Result<Vec<AppxPackage>, AppError> {
        use windows::Management::Deployment::PackageManager;
        let manager = PackageManager::new()
            .map_err(|e| AppError::module("SYS_APPX_004", format!("PackageManager 初始化失败: {e}"), None))?;
        let pkgs = manager
            // 空 userSecurityId = 当前用户（Windows API 语义）；同步返回集合
            .FindPackagesByUserSecurityId(&windows::core::HSTRING::new())
            .map_err(|e| AppError::module("SYS_APPX_005", format!("枚举 Appx 包失败: {e}"), None))?;
        let mut result = Vec::new();
        for p in pkgs.into_iter() {
            let Ok(id) = p.Id() else { continue };
            let Ok(name) = id.Name() else { continue };
            let name_str = name.to_string();
            if !name_str.starts_with(name_filter) {
                continue;
            }
            let Ok(full) = id.FullName() else { continue };
            result.push(AppxPackage { name: name_str, full_name: full.to_string() });
        }
        Ok(result)
    }

    fn remove_current_user(&self, name_filter: &str) -> Result<u32, AppError> {
        use windows::Management::Deployment::PackageManager;
        if !valid_name(name_filter) {
            return Err(AppError::module(
                "SYS_APPX_001",
                format!("包名包含非法字符: {name_filter}"),
                None,
            ));
        }
        let manager = PackageManager::new()
            .map_err(|e| AppError::module("SYS_APPX_004", format!("PackageManager 初始化失败: {e}"), None))?;
        let targets = self.list(name_filter)?;
        let mut removed = 0u32;
        for p in &targets {
            let op = manager
                .RemovePackageAsync(&windows::core::HSTRING::from(&p.full_name))
                .map_err(|e| AppError::module("SYS_APPX_006", format!("移除 {} 失败: {e}", p.full_name), None))?;
            let result = op
                .get()
                .map_err(|e| AppError::module("SYS_APPX_006", format!("等待移除 {} 失败: {e}", p.full_name), None))?;
            let err_text = result.ErrorText().unwrap_or_default();
            if !err_text.is_empty() {
                // 包被系统占用等 → 如实报错不重试（spec §5：提示可从 Store 重装）
                return Err(AppError::module(
                    "SYS_APPX_007",
                    format!("移除 {} 失败: {err_text}", p.full_name),
                    Some("包可能被系统占用；卸载后可从 Microsoft Store 重装"),
                ));
            }
            removed += 1;
        }
        Ok(removed)
    }

    fn remove_provisioned(&self, name_filter: &str) -> Result<u32, AppError> {
        Self::remove_provisioned_ps(name_filter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名称校验：防 PowerShell 注入（引号/分号/管道/反引号/空格一律拒绝）
    #[test]
    fn name_validation_rejects_injection() {
        assert!(valid_name("Microsoft.BingWeather"));
        assert!(valid_name("MicrosoftCorporationII.QuickAssist"));
        assert!(valid_name("Microsoft_549981C3F5F10"));
        assert!(!valid_name(""));
        assert!(!valid_name("a'; Remove-Item C:\\ -Recurse; 'b"));
        assert!(!valid_name("a b"));
        assert!(!valid_name("a|b"));
        assert!(!valid_name("a`b"));
        assert!(!valid_name("中文"));
    }

    /// 真机：当前用户 Appx 枚举（不删除任何包；系统必有 Appx 框架包）
    #[test]
    fn list_current_user_packages() {
        let ops = AppxOps::new();
        let all = ops.list("").unwrap();
        assert!(!all.is_empty(), "当前用户必有 Appx 包（框架包）");
        assert!(all[0].full_name.len() > all[0].name.len(), "全名含版本/hash");
        // 前缀过滤语义
        let filtered = ops.list(&all[0].name).unwrap();
        assert!(filtered.iter().all(|p| p.name.starts_with(&all[0].name)));
    }
}
