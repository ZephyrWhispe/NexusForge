//! PR2 Sidecar 管理（docs/impl/05 PR2；D-29 B2 T-B2-4 泛化）：内核资产按需下载、
//! SHA256 记录、zip 解压与 per-kernel manifest / TOFU pin 持久化。
//!
//! 合规红线：**不内置任何内核二进制**；全部从官方 Release 直链拉取。
//! 下载安装唯一入口收口本文件（`install_binary_from_zip`）——B9 合流的物理保证，
//! 任何新内核都必须走同一 zip→固定名→原子替换→pin 通道，不得旁路写文件。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{ProxyError, Result};

/// 默认 sing-box 版本（stable 通道；UI 可覆盖）
pub const DEFAULT_SINGBOX_VERSION: &str = "1.10.7";
/// wintun 版本（TUN 数据面依赖；内核会在运行时加载同目录 wintun.dll）
pub const DEFAULT_WINTUN_VERSION: &str = "0.14.1";
/// 默认 Xray-core 版本（2026-09-20 核证官方 latest = v26.3.27）
pub const DEFAULT_XRAY_VERSION: &str = "26.3.27";
/// 默认 mihomo 版本（2026-09-20 核证官方 latest = v1.19.31）
pub const DEFAULT_MIHOMO_VERSION: &str = "1.19.31";

/// 官方 Release 直链（SagerNet/sing-box；分发不内置二进制，见 docs/DESIGN.md §10.5）
pub fn singbox_download_url(version: &str) -> String {
    format!(
        "https://github.com/SagerNet/sing-box/releases/download/v{version}/sing-box-{version}-windows-amd64.zip"
    )
}

/// Xray 官方直链。核证结论（2026-09-20，releases/latest + expanded_assets/v26.3.27）：
/// 官方仓库 = **XTLS/Xray-core**（任务书预测的 Xray-install/xray 是其安装脚本仓库，
/// 不发布二进制，按行内"以官方 release 页核证为准"修正）；windows amd64 资产名
/// 恒为 `Xray-windows-64.zip`（不随版本号变化），zip 根含 `xray.exe`。
pub fn xray_download_url(version: &str) -> String {
    format!("https://github.com/XTLS/Xray-core/releases/download/v{version}/Xray-windows-64.zip")
}

/// mihomo 官方直链。核证结论（2026-09-20，expanded_assets/v1.19.31）：
/// MetaCubeX/mihomo windows amd64 有 v1/v2/v3/compatible 四档 ISA 变体，取
/// **`mihomo-windows-amd64-compatible-v{version}.zip`**（兼容最老 x86-64，
/// 任务书预测形态与官方一致）；zip 根含 `mihomo.exe`。
pub fn mihomo_download_url(version: &str) -> String {
    format!(
        "https://github.com/MetaCubeX/mihomo/releases/download/v{version}/mihomo-windows-amd64-compatible-v{version}.zip"
    )
}

/// wintun 官方直链（wireguard 官方发行包；zip 内 bin/amd64/wintun.dll）
pub fn wintun_download_url(version: &str) -> String {
    format!("https://www.wintun.net/builds/wintun-{version}.zip")
}

/// 内核/数据面二进制资产规格（T-B2-4）：id → 官方 zip → 固定落盘名。
/// `entry_suffix` 在构造期就核证钉死（见各 url 函数注释），防"任意条目命中"歧义：
/// wintun 官方 zip 同时含 `bin/x86/wintun.dll` 与 `bin/amd64/wintun.dll`，
/// 只按 exe 文件名后缀匹配会拿 32 位驱动垫背，所以规格带全相对后缀。
#[derive(Clone, Debug)]
pub struct AssetSpec {
    pub id: &'static str,
    /// 恒写到 `bin_dir/exe_name`（zip 条目路径一律不参与落盘名 = zip-slip 红线）
    pub exe_name: &'static str,
    /// zip 条目匹配后缀（首个命中生效）
    pub entry_suffix: &'static str,
    pub url_for: fn(&str) -> String,
    pub default_version: &'static str,
}

/// 三内核下载资产表（新增内核 = 此处加行 + kernel.rs 注册表加臂，两条通道不互用）
pub const KERNEL_ASSETS: &[AssetSpec] = &[
    AssetSpec {
        id: "sing-box",
        exe_name: "sing-box.exe",
        entry_suffix: "sing-box.exe",
        url_for: singbox_download_url,
        default_version: DEFAULT_SINGBOX_VERSION,
    },
    AssetSpec {
        id: "xray",
        exe_name: "xray.exe",
        entry_suffix: "xray.exe",
        url_for: xray_download_url,
        default_version: DEFAULT_XRAY_VERSION,
    },
    AssetSpec {
        id: "mihomo",
        exe_name: "mihomo.exe",
        entry_suffix: "mihomo.exe",
        url_for: mihomo_download_url,
        default_version: DEFAULT_MIHOMO_VERSION,
    },
];

/// wintun 走同一安装通道（T-B2-4 行字面）：id "wintun"，无版本 UI、按默认档手动装
pub const WINTUN_ASSET: AssetSpec = AssetSpec {
    id: "wintun",
    exe_name: "wintun.dll",
    entry_suffix: "bin/amd64/wintun.dll",
    url_for: wintun_download_url,
    default_version: DEFAULT_WINTUN_VERSION,
};

/// id → 资产规格（未注册 id 在任何 IO 前如实拒——T-B2-3 的 check_install_kernel
/// 单内核门由本表整体接管）
pub fn asset_for(id: &str) -> Result<&'static AssetSpec> {
    KERNEL_ASSETS
        .iter()
        .chain(std::iter::once(&WINTUN_ASSET))
        .find(|a| a.id == id)
        .ok_or_else(|| {
            ProxyError::Kernel(format!(
                "未知内核: {id}（下载通道未注册，支持: {}）",
                KERNEL_ASSETS
                    .iter()
                    .map(|a| a.id)
                    .collect::<Vec<_>>()
                    .join("/")
            ))
        })
}

// ---------------- T-B2-10：geo 数据面资产（Binary 原始文件通道） ----------------

/// geo 数据资产规格：与内核 zip 通道分野——release 资产本身就是数据文件
/// （.db/.dat/.mmdb），**非 zip**，恒写 `proxy_dir/sub_dir/file_name`。
/// 通道核证⑭（2026-09-21，releases/latest + HEAD 实测）：
/// - SagerNet/sing-geoip(20260912)/sing-geosite(20260920133716) 只发 `.db`（无 srs）；
/// - XTLS/Xray-core release **不含 geo dat**（任务书预测落空）→ 官方镜像通道改用
///   **Loyalsoldier/v2ray-rules-dat**（Xray 上游官方规则库镜像，latest=202609202346，
///   geoip.dat/geosite.dat 直链 200 实测）；
/// - mihomo geo 文件住 **MetaCubeX/meta-rules-dat**（mihomo 本体 release 无 geo 资产；
///   其原生自刷默认源即 testingcf.jsdelivr …meta-rules-dat@release/country.mmdb，
///   本通道取 GitHub 直连同仓库）。
#[derive(Clone, Debug)]
pub struct GeoAsset {
    pub id: &'static str,
    /// UI 展示名（状态表数据驱动渲染，前端零特例）
    pub label: &'static str,
    /// proxy_dir 下子目录："geo"（sing-box 共享）|"bin"（xray 读 exe 同级）|
    /// "mihomo"（内核 workdir，只认自家目录——geo/ 落点方言不消费，故按消费点落）
    pub sub_dir: &'static str,
    pub file_name: &'static str,
    pub url_for: fn(&str) -> String,
    /// 官方 sha256sum 伴随件（有则装前强验，拒≠放行；无 = 仅 TOFU pin）
    pub checksum_url: Option<fn(&str) -> String>,
    pub default_version: &'static str,
}

pub fn singbox_geoip_url(v: &str) -> String {
    format!("https://github.com/SagerNet/sing-geoip/releases/download/{v}/geoip.db")
}
pub fn singbox_geoip_sha_url(v: &str) -> String {
    format!("https://github.com/SagerNet/sing-geoip/releases/download/{v}/geoip.db.sha256sum")
}
pub fn singbox_geosite_url(v: &str) -> String {
    format!("https://github.com/SagerNet/sing-geosite/releases/download/{v}/geosite.db")
}
pub fn singbox_geosite_sha_url(v: &str) -> String {
    format!("https://github.com/SagerNet/sing-geosite/releases/download/{v}/geosite.db.sha256sum")
}
pub fn xray_geoip_url(v: &str) -> String {
    format!("https://github.com/Loyalsoldier/v2ray-rules-dat/releases/download/{v}/geoip.dat")
}
pub fn xray_geosite_url(v: &str) -> String {
    format!("https://github.com/Loyalsoldier/v2ray-rules-dat/releases/download/{v}/geosite.dat")
}
/// meta-rules-dat 为滚动 latest 通道（无稳定版本号可锁）：pin 键恒 "latest"，
/// 每次内容更新都会触发 TOFU 漂移确认——数据面更新本就是该通道的语义，如实呈现。
pub fn mihomo_countrymmdb_url(_: &str) -> String {
    "https://github.com/MetaCubeX/meta-rules-dat/releases/latest/download/country.mmdb".into()
}
pub fn mihomo_geosite_url(_: &str) -> String {
    "https://github.com/MetaCubeX/meta-rules-dat/releases/latest/download/geosite.dat".into()
}

/// geo 资产注册表（新资产=加行；安装/预检/状态全数据驱动）
pub const GEO_ASSETS: &[GeoAsset] = &[
    GeoAsset {
        id: "singbox-geoip",
        label: "sing-box GeoIP（geoip.db）",
        sub_dir: "geo",
        file_name: "geoip.db",
        url_for: singbox_geoip_url,
        checksum_url: Some(singbox_geoip_sha_url),
        default_version: "20260912",
    },
    GeoAsset {
        id: "singbox-geosite",
        label: "sing-box GeoSite（geosite.db）",
        sub_dir: "geo",
        file_name: "geosite.db",
        url_for: singbox_geosite_url,
        checksum_url: Some(singbox_geosite_sha_url),
        default_version: "20260920133716",
    },
    GeoAsset {
        id: "xray-geoip",
        label: "Xray GeoIP（geoip.dat 镜像）",
        sub_dir: "bin",
        file_name: "geoip.dat",
        url_for: xray_geoip_url,
        checksum_url: None,
        default_version: "202609202346",
    },
    GeoAsset {
        id: "xray-geosite",
        label: "Xray GeoSite（geosite.dat 镜像）",
        sub_dir: "bin",
        file_name: "geosite.dat",
        url_for: xray_geosite_url,
        checksum_url: None,
        default_version: "202609202346",
    },
    GeoAsset {
        id: "mihomo-countrymmdb",
        label: "mihomo Country.mmdb",
        sub_dir: "mihomo",
        file_name: "Country.mmdb",
        url_for: mihomo_countrymmdb_url,
        checksum_url: None,
        default_version: "latest",
    },
    GeoAsset {
        id: "mihomo-geosite",
        label: "mihomo GeoSite（geosite.dat）",
        sub_dir: "mihomo",
        file_name: "geosite.dat",
        url_for: mihomo_geosite_url,
        checksum_url: None,
        default_version: "latest",
    },
];

pub fn artifact_for(id: &str) -> Result<&'static GeoAsset> {
    GEO_ASSETS.iter().find(|a| a.id == id).ok_or_else(|| {
        ProxyError::Download(format!(
            "geo 资产下载通道未注册: {id}（支持: {}）",
            GEO_ASSETS
                .iter()
                .map(|a| a.id)
                .collect::<Vec<_>>()
                .join("/")
        ))
    })
}

/// geo 资产的元数据落点：`{proxy_dir}/geo/`（manifest/pin 与数据文件同根，
/// B9 接管 §3 表行 3 同路径约定）
fn artifact_meta_path(proxy_dir: &Path, file: &str) -> PathBuf {
    proxy_dir.join("geo").join(file)
}

pub fn artifact_manifest_path(proxy_dir: &Path, id: &str) -> PathBuf {
    artifact_meta_path(proxy_dir, &format!("manifest-{id}.json"))
}

fn artifact_pin_path(proxy_dir: &Path, id: &str) -> PathBuf {
    artifact_meta_path(proxy_dir, &format!("pin-{id}.json"))
}

/// 资产消费路径（渲染预检与状态表共用，禁各处重拼路径）
pub fn artifact_dest(spec: &GeoAsset, proxy_dir: &Path) -> PathBuf {
    proxy_dir.join(spec.sub_dir).join(spec.file_name)
}

pub fn artifact_installed(spec: &GeoAsset, proxy_dir: &Path) -> bool {
    artifact_dest(spec, proxy_dir).is_file()
}

/// 读 geo 资产清单（无 zip 通道版 manifest）
pub fn read_artifact_manifest(proxy_dir: &Path, id: &str) -> Option<Manifest> {
    std::fs::read(artifact_manifest_path(proxy_dir, id))
        .ok()
        .and_then(|raw| serde_json::from_slice::<Manifest>(&raw).ok())
}

/// 原始文件安装通道（Binary 资产唯一写盘入口，与 zip 通道并列收口）：
/// 字节按原样恒写 `proxy_dir/sub_dir/file_name`（tmp+rename 原子替换），
/// TOFU pin 按版本键控（同版本字节漂移未 ack → Integrity 拒且坏字节不落盘）。
pub fn install_raw_asset(
    spec: &'static GeoAsset,
    proxy_dir: &Path,
    bytes: &[u8],
    version: &str,
    ack_pin: bool,
) -> Result<Manifest> {
    let sha = sha256_hex(bytes);
    let pin_file = artifact_pin_path(proxy_dir, spec.id);
    let mut pins = std::fs::read(&pin_file)
        .ok()
        .and_then(|raw| serde_json::from_slice::<PinStore>(&raw).ok())
        .unwrap_or_default();
    if let Some(pinned) = pins.0.get(version) {
        if pinned != &sha && !ack_pin {
            return Err(ProxyError::Integrity(format!(
                "{} v{version} 的 sha256 与首见记录不符（记录 {pinned}，本次 {sha}）：包可能被篡改；确认来源可信后可显式确认继续",
                spec.id
            )));
        }
    }
    let dest = artifact_dest(spec, proxy_dir);
    let parent = dest
        .parent()
        .ok_or_else(|| ProxyError::Download("geo 资产落点无父目录".into()))?;
    std::fs::create_dir_all(parent).map_err(ProxyError::from)?;
    let tmp = dest.with_file_name(format!("{}.tmp", spec.file_name));
    std::fs::write(&tmp, bytes).map_err(ProxyError::from)?;
    std::fs::rename(&tmp, &dest).map_err(ProxyError::from)?;
    pins.0.insert(version.to_string(), sha.clone());
    std::fs::create_dir_all(artifact_meta_path(proxy_dir, "")).map_err(ProxyError::from)?;
    let tmp_pin = pin_file.with_extension("json.tmp");
    std::fs::write(&tmp_pin, serde_json::to_vec_pretty(&pins)?).map_err(ProxyError::from)?;
    std::fs::rename(&tmp_pin, &pin_file).map_err(ProxyError::from)?;
    let manifest = Manifest {
        kernel_id: spec.id.to_string(),
        kernel_version: version.to_string(),
        sha256: sha,
        installed_at: now_ms(),
        channel: "release".into(),
    };
    let mp = artifact_manifest_path(proxy_dir, spec.id);
    let tmp_m = mp.with_extension("json.tmp");
    std::fs::write(&tmp_m, serde_json::to_vec_pretty(&manifest)?).map_err(ProxyError::from)?;
    std::fs::rename(&tmp_m, &mp).map_err(ProxyError::from)?;
    Ok(manifest)
}

/// 已安装内核清单（T-B2-4 起 per-kernel：`{appData}/proxy/bin/manifest-<id>.json`）
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub kernel_id: String,
    pub kernel_version: String,
    /// 安装时计算的二进制 SHA256（官方签名校验外的完整性记录）
    pub sha256: String,
    pub installed_at: u64,
    pub channel: String,
}

/// 旧单文件清单（sing-box 时代产物）——只读兼容，永不回写
const LEGACY_MANIFEST_FILE: &str = "manifest.json";

pub fn manifest_path(bin_dir: &Path, id: &str) -> PathBuf {
    bin_dir.join(format!("manifest-{id}.json"))
}

/// 读清单：新文件优先；旧单文件 **仅 sing-box 兼容读**（xray/mihomo 时代起
/// manifest.json 里的记录天然只可能属于 sing-box，冒领即视同缺失）
pub fn read_manifest(bin_dir: &Path, id: &str) -> Option<Manifest> {
    if let Some(m) = std::fs::read(manifest_path(bin_dir, id))
        .ok()
        .and_then(|raw| serde_json::from_slice::<Manifest>(&raw).ok())
    {
        return Some(m);
    }
    if id != "sing-box" {
        return None;
    }
    let legacy = std::fs::read(bin_dir.join(LEGACY_MANIFEST_FILE))
        .ok()
        .and_then(|raw| serde_json::from_slice::<Manifest>(&raw).ok())?;
    (legacy.kernel_id == "sing-box").then_some(legacy)
}

fn write_manifest(bin_dir: &Path, manifest: &Manifest) -> Result<()> {
    let path = manifest_path(bin_dir, &manifest.kernel_id);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(manifest)?)
        .map_err(|e| ProxyError::Download(format!("manifest 写入失败: {e}")))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| ProxyError::Download(format!("manifest 落盘失败: {e}")))?;
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    host_core::util::hex_lower(&h.finalize())
}

/// TOFU pin 存储：`pin-<id>.json` = { 版本号 → 首见 sha256 }。
/// 按版本键控是刻意的：跨版本升级哈希必变，若全局单哈希则每次正常升级都要人工
/// ack，红线会淹没在"每次都问"里；同版本字节漂移才是真篡改信号。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PinStore(BTreeMap<String, String>);

fn pin_path(bin_dir: &Path, id: &str) -> PathBuf {
    bin_dir.join(format!("pin-{id}.json"))
}

fn read_pins(bin_dir: &Path, id: &str) -> PinStore {
    std::fs::read(pin_path(bin_dir, id))
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default()
}

fn write_pins(bin_dir: &Path, id: &str, store: &PinStore) -> Result<()> {
    let path = pin_path(bin_dir, id);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(store)?)
        .map_err(|e| ProxyError::Download(format!("pin 写入失败: {e}")))?;
    std::fs::rename(&tmp, &path).map_err(|e| ProxyError::Download(format!("pin 落盘失败: {e}")))?;
    Ok(())
}

/// 唯一二进制安装入口（红线收口，B9 合流物理保证）：官方 zip → 条目后缀匹配 →
/// sha256/pin 校验（同版本字节漂移且 !ack_pin → Integrity 拒）→ 恒写
/// `bin_dir/spec.exe_name` 固定名（zip 条目路径一律丢弃 = zip-slip 免疫）→
/// tmp+rename 原子替换（安装中断不毁旧核）→ per-kernel manifest + pin 落盘。
pub fn install_binary_from_zip(
    spec: &'static AssetSpec,
    bin_dir: &Path,
    zip_bytes: &[u8],
    version: &str,
    ack_pin: bool,
) -> Result<Manifest> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| ProxyError::Download(format!("zip 打开失败（包不完整？）: {e}")))?;
    let mut exe_bytes: Option<Vec<u8>> = None;
    for i in 0..archive.len() {
        let mut f = archive
            .by_index(i)
            .map_err(|e| ProxyError::Download(format!("zip 条目读取失败: {e}")))?;
        if f.name().ends_with(spec.entry_suffix) {
            // COR-24：声明尺寸来自 zip 头（不受信）——不得用于预分配；
            // take 限流拷贝，超上限即中止
            const MAX_EXE_BYTES: u64 = 256 * 1024 * 1024;
            let mut buf: Vec<u8> = Vec::new();
            let copied = std::io::copy(
                &mut std::io::Read::take(&mut f, MAX_EXE_BYTES + 1),
                &mut buf,
            )
            .map_err(ProxyError::Io)?;
            if copied > MAX_EXE_BYTES {
                return Err(ProxyError::Download(format!(
                    "可执行文件超过 {MAX_EXE_BYTES} 字节上限（疑似非官方包）"
                )));
            }
            exe_bytes = Some(buf);
            break;
        }
    }
    let exe_bytes = exe_bytes.ok_or_else(|| {
        ProxyError::Download(format!("zip 内未找到 {}（非官方包？）", spec.entry_suffix))
    })?;
    let sha = sha256_hex(&exe_bytes);

    // TOFU：首见版本记哈希；同版本换字节 = 篡改面，必须显式 ack 才放行
    let mut pins = read_pins(bin_dir, spec.id);
    if let Some(pinned) = pins.0.get(version) {
        if pinned != &sha && !ack_pin {
            return Err(ProxyError::Integrity(format!(
                "{id} v{version} 的 sha256 与首见记录不符（记录 {pinned}，本次 {sha}）：包可能被篡改；确认来源可信后可显式确认继续",
                id = spec.id
            )));
        }
    }

    // 原子替换 + 固定名写出：条目叫什么路径、有几层 ../ 都与我无关
    std::fs::create_dir_all(bin_dir).map_err(ProxyError::from)?;
    let target = bin_dir.join(spec.exe_name);
    let tmp = bin_dir.join(format!("{}.tmp", spec.exe_name));
    std::fs::write(&tmp, &exe_bytes).map_err(ProxyError::from)?;
    std::fs::rename(&tmp, &target).map_err(ProxyError::from)?;

    pins.0.insert(version.to_string(), sha.clone());
    write_pins(bin_dir, spec.id, &pins)?;
    let manifest = Manifest {
        kernel_id: spec.id.to_string(),
        kernel_version: version.to_string(),
        sha256: sha,
        installed_at: now_ms(),
        channel: "stable".into(),
    };
    write_manifest(bin_dir, &manifest)?;
    Ok(manifest)
}

pub fn wintun_installed(bin_dir: &Path) -> bool {
    bin_dir.join("wintun.dll").is_file()
}

use host_core::util::now_ms_u64 as now_ms;

#[cfg(test)]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
mod tests {
    use super::*;

    /// 构造内存 zip（含指定文件）用于免网络安装测试
    fn make_zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let buf = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            w.start_file(*name, opts).unwrap();
            std::io::Write::write_all(&mut w, bytes).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nf_proxy_sc_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn singbox_zip(payload: &[u8]) -> Vec<u8> {
        make_zip(&[
            (
                "sing-box-1.10.7-windows-amd64/README.md",
                b"readme".to_vec(),
            ),
            (
                "sing-box-1.10.7-windows-amd64/sing-box.exe",
                payload.to_vec(),
            ),
        ])
    }

    #[test]
    fn install_singbox_extracts_exe_and_writes_manifest() {
        let dir = tmp_dir("extract");
        let zip = singbox_zip(b"MZ-fake-singbox");
        let m = install_binary_from_zip(&KERNEL_ASSETS[0], &dir, &zip, "1.10.7", false).unwrap();
        assert_eq!(m.kernel_version, "1.10.7");
        assert_eq!(m.sha256, sha256_hex(b"MZ-fake-singbox"));
        assert_eq!(read_manifest(&dir, "sing-box").unwrap().sha256, m.sha256);
        assert!(dir.join("sing-box.exe").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_rejects_zip_without_exe() {
        let dir = tmp_dir("bad");
        let zip = make_zip(&[("other.txt", b"x".to_vec())]);
        let err =
            install_binary_from_zip(&KERNEL_ASSETS[0], &dir, &zip, "1.0.0", false).unwrap_err();
        assert!(matches!(err, ProxyError::Download(_)));
        // 拒装不留半成品
        assert!(!dir.join("sing-box.exe").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_wintun_finds_amd64_dll() {
        let dir = tmp_dir("wintun");
        let zip = make_zip(&[
            ("wintun/bin/x86/wintun.dll", b"x86".to_vec()),
            ("wintun/bin/amd64/wintun.dll", b"amd64".to_vec()),
        ]);
        install_binary_from_zip(&WINTUN_ASSET, &dir, &zip, DEFAULT_WINTUN_VERSION, false).unwrap();
        assert!(wintun_installed(&dir));
        // 规格化 entry_suffix 的真实功效：拿到的必须是 amd64 那份
        assert_eq!(std::fs::read(dir.join("wintun.dll")).unwrap(), b"amd64");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn download_urls_are_official_endpoints() {
        assert!(singbox_download_url("1.10.7")
            .starts_with("https://github.com/SagerNet/sing-box/releases/download/v1.10.7/"));
        assert!(wintun_download_url(DEFAULT_WINTUN_VERSION).contains("wintun.net"));
        // 2026-09-20 对官方 release 页核证过的形态（见各函数注释）
        assert_eq!(
            xray_download_url("26.3.27"),
            "https://github.com/XTLS/Xray-core/releases/download/v26.3.27/Xray-windows-64.zip"
        );
        assert_eq!(
            mihomo_download_url("1.19.31"),
            "https://github.com/MetaCubeX/mihomo/releases/download/v1.19.31/mihomo-windows-amd64-compatible-v1.19.31.zip"
        );
    }

    #[test]
    fn assetFor_threeKernelsAndWintun_unknownRejected() {
        for (id, exe) in [
            ("sing-box", "sing-box.exe"),
            ("xray", "xray.exe"),
            ("mihomo", "mihomo.exe"),
            ("wintun", "wintun.dll"),
        ] {
            let a = asset_for(id).unwrap();
            assert_eq!(a.id, id);
            assert_eq!(a.exe_name, exe);
        }
        // 未注册 id 在任何 IO 前拒，错误文案指路支持集
        let e = asset_for("clash-premium").unwrap_err();
        match &e {
            ProxyError::Kernel(msg) => {
                assert!(msg.contains("clash-premium"));
                assert!(
                    msg.contains("sing-box/xray/mihomo"),
                    "文案须列出支持内核: {msg}"
                );
            }
            other => panic!("必须是 Kernel 错，实得 {other:?}"),
        }
    }

    #[test]
    fn sidecarLegacyManifest_singboxOnly() {
        let dir = tmp_dir("legacy");
        // 旧单文件现场：manifest.json 只有 sing-box 时代的记录
        let legacy = Manifest {
            kernel_id: "sing-box".into(),
            kernel_version: "1.10.7".into(),
            sha256: "old".into(),
            installed_at: 1,
            channel: "stable".into(),
        };
        std::fs::write(
            dir.join(LEGACY_MANIFEST_FILE),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        // sing-box 兼容读旧文件
        assert_eq!(
            read_manifest(&dir, "sing-box").unwrap().kernel_version,
            "1.10.7"
        );
        // 其余内核不得冒领旧单文件（它物理上不可能记录 xray）
        assert!(read_manifest(&dir, "xray").is_none());
        assert!(read_manifest(&dir, "mihomo").is_none());
        // 新 per-kernel 文件优先于旧单文件
        let mut fresh = legacy.clone();
        fresh.kernel_version = "1.11.0".into();
        write_manifest(&dir, &fresh).unwrap();
        assert_eq!(
            read_manifest(&dir, "sing-box").unwrap().kernel_version,
            "1.11.0"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecarPinMismatch_rejectsWithoutAck() {
        // 红线：同一资产同版本二次安装换字节 → 拒；显式 ack 后才放行并更新记录
        let dir = tmp_dir("pin");
        let spec = &KERNEL_ASSETS[1]; // xray（与 sing-box 旧行为区分度高的第二内核）
        let zip_v1 = make_zip(&[("xray.exe", b"official-bytes".to_vec())]);
        install_binary_from_zip(spec, &dir, &zip_v1, "26.3.27", false).unwrap();
        let zip_evil = make_zip(&[("xray.exe", b"tampered-bytes".to_vec())]);
        let e = install_binary_from_zip(spec, &dir, &zip_evil, "26.3.27", false).unwrap_err();
        assert!(
            matches!(e, ProxyError::Integrity(_)),
            "同版本字节漂移必须是 Integrity 错，实得 {e:?}"
        );
        // 被拒的坏字节绝不许落盘
        assert_eq!(
            std::fs::read(dir.join("xray.exe")).unwrap(),
            b"official-bytes"
        );
        install_binary_from_zip(spec, &dir, &zip_evil, "26.3.27", true).unwrap();
        assert_eq!(
            std::fs::read(dir.join("xray.exe")).unwrap(),
            b"tampered-bytes"
        );
        // 跨版本升级哈希必变：TOFU 按版本键控，正常升版零 ack（红线不淹没在噪音里）
        let zip_new = make_zip(&[("xray.exe", b"next-release-bytes".to_vec())]);
        install_binary_from_zip(spec, &dir, &zip_new, "26.9.9", false).unwrap();
        assert_eq!(
            std::fs::read(dir.join("xray.exe")).unwrap(),
            b"next-release-bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecarZipSlipEntry_writesFixedNameOnly() {
        // 红线：恶意条目路径（含 ../ 逃逸与绝对化形态）一律不参与落盘名，
        // 写出恒为 bin_dir/sing-box.exe，逃逸目录绝不生成
        let root = std::env::temp_dir();
        let dir = root.join(format!("nf_proxy_sc_slip_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip = make_zip(&[
            ("../../nf_escape_TBB24/sing-box.exe", b"evil".to_vec()),
            ("C:/Windows/sing-box.exe", b"evil2".to_vec()),
            ("deep/nested/dir/sing-box.exe", b"payload".to_vec()),
        ]);
        install_binary_from_zip(&KERNEL_ASSETS[0], &dir, &zip, "1.10.7", false).unwrap();
        // 只存在固定名一处，内容 = 首个后缀命中条目
        assert_eq!(std::fs::read(dir.join("sing-box.exe")).unwrap(), b"evil");
        assert!(
            !root.join("nf_escape_TBB24").exists(),
            "zip-slip 逃逸目录不得被创建"
        );
        assert!(!dir.join("deep").exists());
        // 绝对化条目形态 "C:/…" 不参与落盘的证明方式 = 目录清单精确比对而非
        // dir.join("C:").exists()：Rust 的 push 把盘符前缀视为整体替换（join("C:")
        // 坍缩成驱动器相对路径 "C:" 恒存在），该断言形式在 Windows 上必假红
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "manifest-sing-box.json",
                "pin-sing-box.json",
                "sing-box.exe"
            ],
            "bin_dir 内除固定名 exe + per-kernel manifest + pin 外不得有任何散件"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecarInstall_perKernelBinaries_coexist() {
        // 三内核二进制 + 各自 manifest/pin 互不覆写（同目录共存是换核的前提）
        let dir = tmp_dir("coexist");
        install_binary_from_zip(
            &KERNEL_ASSETS[0],
            &dir,
            &singbox_zip(b"MZ-singbox"),
            "1.10.7",
            false,
        )
        .unwrap();
        install_binary_from_zip(
            &KERNEL_ASSETS[1],
            &dir,
            &make_zip(&[("xray.exe", b"MZ-xray".to_vec())]),
            "26.3.27",
            false,
        )
        .unwrap();
        install_binary_from_zip(
            &KERNEL_ASSETS[2],
            &dir,
            &make_zip(&[("mihomo-windows/mihomo.exe", b"MZ-mihomo".to_vec())]),
            "1.19.31",
            false,
        )
        .unwrap();
        for (id, exe) in [
            ("sing-box", "sing-box.exe"),
            ("xray", "xray.exe"),
            ("mihomo", "mihomo.exe"),
        ] {
            assert!(dir.join(exe).is_file(), "{exe} 必须存在且未被后装者覆写");
            let m = read_manifest(&dir, id).unwrap_or_else(|| panic!("{id} manifest 缺失"));
            assert_eq!(m.kernel_id, id);
        }
        assert_eq!(
            read_manifest(&dir, "sing-box").unwrap().kernel_version,
            "1.10.7"
        );
        assert_eq!(
            read_manifest(&dir, "xray").unwrap().kernel_version,
            "26.3.27"
        );
        assert_eq!(
            read_manifest(&dir, "mihomo").unwrap().kernel_version,
            "1.19.31"
        );
        // 三套 pin 文件独立存在（互不串写）
        for id in ["sing-box", "xray", "mihomo"] {
            assert!(dir.join(format!("pin-{id}.json")).is_file());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- T-B2-10：Binary 原始文件资产通道（09 §5.2 字面回归） ----------------

    #[test]
    fn geoAsset_xrayDatBinaryChannel() {
        // 非 zip 资产直存：字节原样落 bin/geoip.dat（xray 从 exe 同目录解析 dat），
        // manifest/pin 元数据恒栖 geo/ 目录（与数据文件分目录，B9 §3 表行 3 路径约定）
        let dir = tmp_dir("geoxray");
        let spec = artifact_for("xray-geoip").unwrap();
        assert_eq!(
            artifact_dest(spec, &dir),
            dir.join("bin").join("geoip.dat"),
            "xray dat 落点=bin/ 子目录"
        );
        let m = install_raw_asset(spec, &dir, b"dat-bytes-1", "202609202346", false).unwrap();
        assert_eq!(
            std::fs::read(artifact_dest(spec, &dir)).unwrap(),
            b"dat-bytes-1"
        );
        assert_eq!(m.kernel_id, "xray-geoip");
        assert_eq!(m.kernel_version, "202609202346");
        assert_eq!(m.sha256, sha256_hex(b"dat-bytes-1"));
        assert!(artifact_installed(spec, &dir));
        assert_eq!(
            read_artifact_manifest(&dir, "xray-geoip").unwrap().sha256,
            m.sha256
        );
        assert!(artifact_manifest_path(&dir, "xray-geoip").is_file());
        assert!(artifact_pin_path(&dir, "xray-geoip").is_file());
        // 未注册 id 在任何触网前如实拒并点名支持集
        assert!(matches!(
            artifact_for("geoip-magic"),
            Err(ProxyError::Download(msg)) if msg.contains("xray-geoip")
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn artifactInstall_pinMismatch_rejects() {
        // 红线：同版本字节漂移 → Integrity 拒且盘上仍是首装字节；显式 ack 才放行。
        // mihomo-countrymmdb 的 sub_dir="mihomo" ≠ 元数据目录 "geo"——同时钉死
        // 两目录都按需创建（写盘入口收口在 sub_dir 不同根时不缺目录）
        let dir = tmp_dir("geopin");
        let spec = artifact_for("mihomo-countrymmdb").unwrap();
        install_raw_asset(spec, &dir, b"mmdb-original", "latest", false).unwrap();
        let err = install_raw_asset(spec, &dir, b"mmdb-tampered", "latest", false).unwrap_err();
        match err {
            ProxyError::Integrity(msg) => {
                assert!(
                    msg.contains("latest") && msg.contains("sha256"),
                    "须点名版本与哈希面：{msg}"
                );
            }
            other => panic!("同版本换字节必须 Integrity 拒，得 {other:?}"),
        }
        assert_eq!(
            std::fs::read(artifact_dest(spec, &dir)).unwrap(),
            b"mmdb-original",
            "被拒字节物理不落盘"
        );
        install_raw_asset(spec, &dir, b"mmdb-tampered", "latest", true).unwrap();
        assert_eq!(
            std::fs::read(artifact_dest(spec, &dir)).unwrap(),
            b"mmdb-tampered"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
