//! T5 WSL 集成（docs/impl/06 T5）：分发探测。
//!
//! wsl.exe 输出为 UTF-16LE（历史行为）；spawn 走 ConPTY（`wsl.exe -d {distro}`）。

use crate::error::{Result, TermError};

/// wsl.exe 是否可用
pub fn is_available() -> bool {
    std::process::Command::new("wsl.exe")
        .arg("--status")
        .output()
        .is_ok()
}

/// 已安装分发列表（`wsl --list --quiet`，UTF-16LE 输出）
pub fn list_distros() -> Result<Vec<String>> {
    let out = std::process::Command::new("wsl.exe")
        .arg("--list")
        .arg("--quiet")
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TermError::Wsl("wsl.exe 不可用（未安装 WSL）".into())
            } else {
                TermError::Wsl(format!("执行 wsl.exe 失败: {e}"))
            }
        })?;
    if !out.status.success() {
        return Err(TermError::Wsl(format!(
            "wsl --list 退出码 {:?}",
            out.status.code()
        )));
    }
    Ok(decode_utf16_list(&out.stdout))
}

/// 解析 wsl.exe 的 UTF-16LE 行列表
fn decode_utf16_list(bytes: &[u8]) -> Vec<String> {
    let u16s: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u16s)
        .split(['\0', '\r', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_utf16_list_parses_names() {
        // "Ubuntu-22.04\r\nDebian\r\n" 的 UTF-16LE
        let mut bytes = Vec::new();
        for s in ["Ubuntu-22.04\r\n", "Debian\r\n"] {
            for u in s.encode_utf16() {
                bytes.extend_from_slice(&u.to_le_bytes());
            }
        }
        assert_eq!(decode_utf16_list(&bytes), vec!["Ubuntu-22.04", "Debian"]);
    }

    #[test]
    fn decode_empty() {
        assert!(decode_utf16_list(&[]).is_empty());
    }

    #[test]
    fn wsl_availability_does_not_panic() {
        // 环境相关：只保证不 panic
        let _ = is_available();
    }
}
