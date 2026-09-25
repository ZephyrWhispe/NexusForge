//! COR-26：Mica 能力探测——Mica 仅 Windows 11（build ≥ 22000）且非远程会话可用。
//! 旧路径 `transparent: true` + windowEffects=mica 在 Win10/RDP/部分 VM 上被系统
//! 静默忽略，页面基底透明 → chrome 直接透出壁纸（可读性崩坏）。前端据此回退
//! 渐变底（MicaBackdrop 的 micaFallback），探测须在渲染前完成。

use windows::core::w;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_REMOTESESSION};

/// Mica 可用性：Win11（build ≥ 22000）且非远程桌面会话。
pub fn mica_supported() -> bool {
    os_build() >= 22000 && !is_remote_session()
}

/// OS build 号（HKLM\...\CurrentVersion:CurrentBuildNumber；读取失败 = 0 = 不支持）
fn os_build() -> u32 {
    unsafe {
        let mut buf = [0u16; 32];
        let mut size = (buf.len() * 2) as u32;
        let status = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        );
        if status.is_err() || size < 2 {
            tracing::debug!("OS build 号读取失败（按不支持 Mica 处理）");
            return 0;
        }
        let len = (size as usize) / 2 - 1; // 去尾 NUL
        String::from_utf16_lossy(&buf[..len])
            .trim()
            .parse()
            .unwrap_or(0)
    }
}

/// 是否远程桌面会话（RDP 下系统合成器不走 Mica）
fn is_remote_session() -> bool {
    unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn mica_support_on_current_machine_is_consistent() {
        // 本机（开发基线 Win11）真值只要求"可调用且自洽"——build 读不到时必判否
        let sup = super::mica_supported();
        let build = super::os_build();
        if build == 0 {
            assert!(!sup, "build 未知必须判否");
        } else if build >= 22000 && !super::is_remote_session() {
            assert!(sup);
        } else {
            assert!(!sup);
        }
    }
}
