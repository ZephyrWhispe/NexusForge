//! PR2 Sidecar 管理（docs/impl/05 PR2）：sing-box / wintun 内核资产按需下载、
//! SHA256 记录、zip 解压与 manifest 持久化。
//!
//! 合规红线：**不内置任何内核二进制**；全部从官方 GitHub Release 拉取，
//! 下载网络路径与校验在 [`crate::service::ProxyService`] 完成（reqwest + rustls）。

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{ProxyError, Result};

/// 默认 sing-box 版本（stable 通道；UI 可覆盖）
pub const DEFAULT_SINGBOX_VERSION: &str = "1.10.7";
/// wintun 版本（TUN 数据面依赖；sing-box 会在运行时加载同目录 wintun.dll）
pub const DEFAULT_WINTUN_VERSION: &str = "0.14.1";

/// 官方 Release 直链（SagerNet/sing-box；分发不内置二进制，见 docs/DESIGN.md §10.5）
pub fn singbox_download_url(version: &str) -> String {
    format!(
        "https://github.com/SagerNet/sing-box/releases/download/v{version}/sing-box-{version}-windows-amd64.zip"
    )
}

/// wintun 官方直链（wireguard 官方发行包；zip 内 bin/amd64/wintun.dll）
pub fn wintun_download_url(version: &str) -> String {
    format!("https://www.wintun.net/builds/wintun-{version}.zip")
}

/// 已安装内核清单（`{appData}/proxy/bin/manifest.json`）
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub kernel_id: String,
    pub kernel_version: String,
    /// 安装时计算的 sing-box.exe SHA256（官方签名校验外的完整性记录）
    pub sha256: String,
    pub installed_at: u64,
    pub channel: String,
}

pub fn read_manifest(bin_dir: &Path) -> Option<Manifest> {
    let raw = std::fs::read(bin_dir.join("manifest.json")).ok()?;
    serde_json::from_slice(&raw).ok()
}

pub fn write_manifest(bin_dir: &Path, manifest: &Manifest) -> Result<()> {
    let tmp = bin_dir.join("manifest.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(manifest)?)
        .map_err(|e| ProxyError::Download(format!("manifest 写入失败: {e}")))?;
    std::fs::rename(&tmp, bin_dir.join("manifest.json"))
        .map_err(|e| ProxyError::Download(format!("manifest 落盘失败: {e}")))?;
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

/// 从官方 zip 安装 sing-box：解压包内 `sing-box.exe` 到 `bin_dir`，写 manifest。
/// zip 结构完整性由 zip crate 校验（CRC32），exe SHA256 记录到 manifest。
pub fn install_singbox_from_zip(bin_dir: &Path, zip_bytes: &[u8], version: &str) -> Result<Manifest> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| ProxyError::Download(format!("zip 打开失败（包不完整？）: {e}")))?;
    std::fs::create_dir_all(bin_dir).map_err(ProxyError::from)?;

    // Release 包为 sing-box-{ver}-windows-amd64/sing-box.exe 单层目录
    let mut exe_bytes: Option<Vec<u8>> = None;
    for i in 0..archive.len() {
        let mut f = archive
            .by_index(i)
            .map_err(|e| ProxyError::Download(format!("zip 条目读取失败: {e}")))?;
        let name = f.name().to_string();
        if name.ends_with("sing-box.exe") {
            let mut buf = Vec::with_capacity(f.size() as usize);
            std::io::copy(&mut f, &mut buf).map_err(ProxyError::Io)?;
            exe_bytes = Some(buf);
            break;
        }
    }
    let exe_bytes = exe_bytes
        .ok_or_else(|| ProxyError::Download("zip 内未找到 sing-box.exe（非官方包？）".into()))?;

    // 原子替换：先写 tmp 再 rename（安装中断不破坏旧内核）
    let exe = bin_dir.join("sing-box.exe");
    let tmp = bin_dir.join("sing-box.exe.tmp");
    std::fs::write(&tmp, &exe_bytes).map_err(ProxyError::from)?;
    std::fs::rename(&tmp, &exe).map_err(ProxyError::from)?;

    let manifest = Manifest {
        kernel_id: "sing-box".into(),
        kernel_version: version.to_string(),
        sha256: sha256_hex(&exe_bytes),
        installed_at: now_ms(),
        channel: "stable".into(),
    };
    write_manifest(bin_dir, &manifest)?;
    Ok(manifest)
}

/// 从 wintun 官方 zip 安装 `bin/amd64/wintun.dll` 到 `bin_dir`（PR5 TUN 数据面依赖）。
pub fn install_wintun_from_zip(bin_dir: &Path, zip_bytes: &[u8]) -> Result<()> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| ProxyError::Download(format!("zip 打开失败: {e}")))?;
    std::fs::create_dir_all(bin_dir).map_err(ProxyError::from)?;
    let mut dll_bytes: Option<Vec<u8>> = None;
    for i in 0..archive.len() {
        let mut f = archive
            .by_index(i)
            .map_err(|e| ProxyError::Download(format!("zip 条目读取失败: {e}")))?;
        if f.name().ends_with("bin/amd64/wintun.dll") {
            let mut buf = Vec::with_capacity(f.size() as usize);
            std::io::copy(&mut f, &mut buf).map_err(ProxyError::Io)?;
            dll_bytes = Some(buf);
            break;
        }
    }
    let dll_bytes = dll_bytes
        .ok_or_else(|| ProxyError::Download("zip 内未找到 bin/amd64/wintun.dll".into()))?;
    let dll = bin_dir.join("wintun.dll");
    let tmp = bin_dir.join("wintun.dll.tmp");
    std::fs::write(&tmp, &dll_bytes).map_err(ProxyError::from)?;
    std::fs::rename(&tmp, &dll).map_err(ProxyError::from)?;
    Ok(())
}

pub fn wintun_installed(bin_dir: &Path) -> bool {
    bin_dir.join("wintun.dll").is_file()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造内存 zip（含指定文件）用于免网络安装测试
    fn make_zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let buf = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(buf);
        let opts =
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            w.start_file(*name, opts).unwrap();
            std::io::Write::write_all(&mut w, bytes).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn install_singbox_extracts_exe_and_writes_manifest() {
        let dir = std::env::temp_dir().join(format!("nf_proxy_sc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let exe_payload = b"MZ-fake-singbox".to_vec();
        let zip = make_zip(&[
            ("sing-box-1.10.7-windows-amd64/README.md", b"readme".to_vec()),
            ("sing-box-1.10.7-windows-amd64/sing-box.exe", exe_payload.clone()),
        ]);
        let m = install_singbox_from_zip(&dir, &zip, "1.10.7").unwrap();
        assert_eq!(m.kernel_version, "1.10.7");
        assert_eq!(m.sha256, sha256_hex(&exe_payload));
        assert_eq!(read_manifest(&dir).unwrap().sha256, m.sha256);
        assert!(dir.join("sing-box.exe").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_rejects_zip_without_exe() {
        let dir = std::env::temp_dir().join(format!("nf_proxy_bad_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let zip = make_zip(&[("other.txt", b"x".to_vec())]);
        let err = install_singbox_from_zip(&dir, &zip, "1.0.0");
        assert!(matches!(err, Err(ProxyError::Download(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_wintun_finds_amd64_dll() {
        let dir = std::env::temp_dir().join(format!("nf_proxy_wt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let zip = make_zip(&[
            ("wintun/bin/x86/wintun.dll", b"x86".to_vec()),
            ("wintun/bin/amd64/wintun.dll", b"amd64".to_vec()),
        ]);
        install_wintun_from_zip(&dir, &zip).unwrap();
        assert!(wintun_installed(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn download_urls_are_official_endpoints() {
        assert!(singbox_download_url("1.10.7").starts_with("https://github.com/SagerNet/sing-box/releases/download/v1.10.7/"));
        assert!(wintun_download_url(DEFAULT_WINTUN_VERSION).contains("wintun.net"));
    }
}
