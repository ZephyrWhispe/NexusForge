//! 系统强调色读取（docs/UI-PLAN.md U1-3）
//!
//! 主路径：`DwmGetColorizationColor`（COLORREF 0x00BBGGRR）；
//! 失败（DWM 关闭 / 会话未就绪）返回 Err，由前端回退默认 Windows 蓝。

use windows::Win32::Foundation::{FALSE, BOOL};
use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;

use host_core::error::AppError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccentColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl AccentColor {
    /// 前端可用的 `#rrggbb`
    pub fn to_hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// 读取当前 Windows 强调色。
///
/// 错误码：`HOST_SYSTEM_001`（强调色不可用，UI 应回退默认主题色）。
pub fn system_accent_color() -> Result<AccentColor, AppError> {
    unsafe {
        let mut colorref: u32 = 0;
        let mut opaque: BOOL = FALSE;
        DwmGetColorizationColor(&mut colorref, &mut opaque)
            .map_err(|e| {
                AppError::module(
                    "HOST_SYSTEM_001",
                    format!("读取系统强调色失败: {e}"),
                    Some("将使用默认 Windows 蓝"),
                )
            })?;
        // COLORREF = 0x00BBGGRR
        Ok(AccentColor {
            r: (colorref & 0xFF) as u8,
            g: ((colorref >> 8) & 0xFF) as u8,
            b: ((colorref >> 16) & 0xFF) as u8,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_hex_format() {
        assert_eq!(AccentColor { r: 0, g: 0x78, b: 0xd4 }.to_hex(), "#0078d4");
    }

    #[test]
    fn colorref_channel_order() {
        // COLORREF 0x00BBGGRR：低 8 位是 R
        let c: u32 = 0x00D4_78_00; // r=0x00, g=0x78, b=0xd4
        let a = AccentColor {
            r: (c & 0xFF) as u8,
            g: ((c >> 8) & 0xFF) as u8,
            b: ((c >> 16) & 0xFF) as u8,
        };
        assert_eq!(a.to_hex(), "#0078d4");
    }
}
