//! A6 插件管理器（docs/impl/07 A6）：manifest 校验 + 沙箱权限映射 + 本地安装。
//!
//! - 目录布局：{appData}/automation/plugins/{id}/manifest.json + {entry}（默认 main.wasm）
//! - 加载校验：api_version 兼容 + entry 防目录穿越 + 权限白名单 + sha256（entry 文件哈希）
//! - 权限 → 宿主函数映射：open → nf.open_url、notify → nf.notify（log 恒给，不入权限表）
//! - 市场客户端 v1 = 本地目录导入（URL 直链安装随 REL4 分发渠道后续里程碑）

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AutomationError, Result};
use crate::wasm::WasmCaps;

/// 插件 API 版本（major 不兼容拒绝加载）
pub const PLUGIN_API_VERSION: u32 = 1;
/// 合法权限值（log 恒给不列入）
pub const PLUGIN_PERMISSIONS: [&str; 2] = ["open", "notify"];

/// 插件清单（docs/impl/07 A6：{id, name, version, api_version, permissions[], sha256}）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// 入口 wasm 相对路径（默认 main.wasm；防目录穿越）
    #[serde(default = "default_entry")]
    pub entry: String,
    /// 入口导出函数名（默认 run）
    #[serde(default = "default_func")]
    pub func: String,
    /// entry 文件 SHA256（hex，安装时校验）
    pub sha256: String,
}

fn default_entry() -> String {
    "main.wasm".into()
}

fn default_func() -> String {
    "run".into()
}

/// manifest 原子写临时文件路径
fn manifest_tmp_path(dir: &Path) -> PathBuf {
    dir.join("manifest.json.tmp")
}

/// 清单校验（docs/impl/07 A6：api_version 兼容矩阵 + 权限逐一映射 + sha256）
pub fn validate_manifest(m: &PluginManifest, wasm: &[u8]) -> Result<()> {
    // id：目录名安全（防穿越/防绝对路径）
    let id_ok =
        !m.id.is_empty() && m.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !id_ok {
        return Err(AutomationError::BadRule(format!(
            "插件 id 只允许字母数字_-：{}",
            m.id
        )));
    }
    if m.api_version != PLUGIN_API_VERSION {
        return Err(AutomationError::BadRule(format!(
            "插件 api_version {} 与当前 {} 不一致",
            m.api_version, PLUGIN_API_VERSION
        )));
    }
    if m.entry.contains("..") || m.entry.contains('\\') || m.entry.contains(':') || Path::new(&m.entry).is_absolute()
    {
        return Err(AutomationError::BadRule(format!("entry 非法（防路径穿越）: {}", m.entry)));
    }
    for p in &m.permissions {
        if !PLUGIN_PERMISSIONS.contains(&p.as_str()) {
            return Err(AutomationError::BadRule(format!(
                "未知权限 {p}（合法值：{PLUGIN_PERMISSIONS:?}）"
            )));
        }
    }
    // sha256 与 entry 文件匹配（大小写不敏感）
    let mut hasher = Sha256::new();
    hasher.update(wasm);
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(&m.sha256) {
        return Err(AutomationError::BadRule(format!(
            "sha256 不匹配（manifest={} 实际={actual}）",
            m.sha256
        )));
    }
    Ok(())
}

impl PluginManifest {
    /// 权限 → 沙箱能力开关（docs/impl/07 A6：权限逐一映射到注入的宿主函数）
    pub fn caps(&self) -> WasmCaps {
        WasmCaps {
            allow_open: self.permissions.iter().any(|p| p == "open"),
            allow_notify: self.permissions.iter().any(|p| p == "notify"),
        }
    }
}

/// 插件清单 + 目录信息（IPC DTO）
#[derive(Clone, Debug, Serialize)]
pub struct PluginInfo {
    #[serde(flatten)]
    pub manifest: PluginManifest,
    /// wasm 文件存在
    pub installed: bool,
}

/// 本地插件库（扫描/安装/删除/加载）
pub struct PluginStore {
    root: PathBuf,
}

impl PluginStore {
    pub fn new(app_data_dir: &Path) -> Self {
        Self { root: app_data_dir.join("automation").join("plugins") }
    }

    pub fn plugin_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// 全部插件（损坏 manifest 跳过并告警）
    pub fn list(&self) -> Vec<PluginInfo> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for e in entries.flatten() {
            let dir = e.path();
            if !dir.is_dir() {
                continue;
            }
            let manifest_path = dir.join("manifest.json");
            let Ok(raw) = std::fs::read(&manifest_path) else { continue };
            let Ok(m) = serde_json::from_slice::<PluginManifest>(&raw) else {
                tracing::warn!(dir = %dir.display(), "插件 manifest 损坏，跳过");
                continue;
            };
            let installed = dir.join(&m.entry).is_file();
            out.push(PluginInfo { manifest: m, installed });
        }
        out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        out
    }

    /// 从本地目录导入（读 {src}/manifest.json + entry 文件 → 校验 → 原子写库内）
    pub fn install_from_dir(&self, src: &Path) -> Result<PluginManifest> {
        let manifest_path = src.join("manifest.json");
        let raw = std::fs::read(&manifest_path)
            .map_err(AutomationError::Io)
            .map_err(|e| AutomationError::BadRule(format!("manifest.json 读取失败: {e}")))?;
        let manifest: PluginManifest = serde_json::from_slice(&raw)
            .map_err(|e| AutomationError::BadRule(format!("manifest.json 解析失败: {e}")))?;
        let wasm = std::fs::read(src.join(&manifest.entry))
            .map_err(AutomationError::Io)
            .map_err(|e| AutomationError::BadRule(format!("entry {} 读取失败: {e}", manifest.entry)))?;
        self.install(manifest, &wasm)
    }

    /// 安装（校验 → 原子写 manifest.json + entry 文件）
    pub fn install(&self, manifest: PluginManifest, wasm: &[u8]) -> Result<PluginManifest> {
        validate_manifest(&manifest, wasm)?;
        let dir = self.plugin_dir(&manifest.id);
        std::fs::create_dir_all(&dir).map_err(AutomationError::Io)?;
        // tmp + rename 原子替换（工程惯例；旧 entry 与新 entry 不同名时先删旧文件）
        let entry_tmp = dir.join(format!("{}.tmp", manifest.entry));
        let entry_dst = dir.join(&manifest.entry);
        std::fs::write(&entry_tmp, wasm).map_err(AutomationError::Io)?;
        let manifest_data = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| AutomationError::BadRule(format!("manifest 序列化失败: {e}")))?;
        std::fs::write(&manifest_tmp_path(&dir), manifest_data).map_err(AutomationError::Io)?;
        if entry_dst.exists() && entry_dst != entry_tmp {
            std::fs::remove_file(&entry_dst).map_err(AutomationError::Io)?;
        }
        std::fs::rename(&entry_tmp, &entry_dst).map_err(AutomationError::Io)?;
        std::fs::rename(manifest_tmp_path(&dir), dir.join("manifest.json")).map_err(AutomationError::Io)?;
        Ok(manifest)
    }

    /// 删除插件目录（不存在返回 false）
    pub fn remove(&self, id: &str) -> Result<bool> {
        let dir = self.plugin_dir(id);
        if !dir.is_dir() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&dir).map_err(AutomationError::Io)?;
        Ok(true)
    }

    /// 加载 entry wasm 字节（sha256 复验——库内文件可能被篡改）
    pub fn wasm_bytes(&self, id: &str) -> Result<Vec<u8>> {
        let m = self.manifest_of(id)?;
        let wasm = std::fs::read(self.plugin_dir(id).join(&m.entry)).map_err(AutomationError::Io)?;
        validate_manifest(&m, &wasm)?;
        Ok(wasm)
    }

    pub fn manifest_of(&self, id: &str) -> Result<PluginManifest> {
        // id 同样防穿越（规则 RunScript path 来自用户配置）
        if id.contains("..") || id.contains('/') || id.contains('\\') {
            return Err(AutomationError::BadRule(format!("插件 id 非法: {id}")));
        }
        let raw = std::fs::read(self.plugin_dir(id).join("manifest.json")).map_err(AutomationError::Io)?;
        serde_json::from_slice(&raw)
            .map_err(|e| AutomationError::BadRule(format!("插件 {id} manifest 解析失败: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合法最小 wasm 模块（wat 文本；wasmtime wat feature 在执行时按文本解析）
    fn sample_wasm() -> Vec<u8> {
        br#"(module (memory (export "memory") 1) (func (export "run")))"#.to_vec()
    }

    fn manifest_with_sha(id: &str, wasm: &[u8], api: u32) -> PluginManifest {
        let mut hasher = Sha256::new();
        hasher.update(wasm);
        PluginManifest {
            id: id.into(),
            name: "示例插件".into(),
            version: "0.1.0".into(),
            api_version: api,
            permissions: vec!["notify".into()],
            entry: "main.wasm".into(),
            func: "run".into(),
            sha256: format!("{:x}", hasher.finalize()),
        }
    }

    #[test]
    fn install_list_remove_roundtrip() {
        // tmpdir 带测试 tag 防并发互踩（工程教训）
        let dir = std::env::temp_dir().join(format!("nf_auto_plugin_rt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = PluginStore::new(&dir);
        let wasm = sample_wasm();
        let m = manifest_with_sha("demo", &wasm, PLUGIN_API_VERSION);
        store.install(m.clone(), &wasm).unwrap();

        let list = store.list();
        assert_eq!(list.len(), 1);
        assert!(list[0].installed);
        assert_eq!(list[0].manifest.id, "demo");

        // 加载复验通过
        let loaded = store.wasm_bytes("demo").unwrap();
        assert_eq!(loaded, wasm);
        // 权限映射
        assert_eq!(m.caps(), WasmCaps { allow_open: false, allow_notify: true });

        assert!(store.remove("demo").unwrap());
        assert!(!store.remove("demo").unwrap());
        assert!(store.list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_rejects_bad_manifest() {
        let dir = std::env::temp_dir().join(format!("nf_auto_plugin_bad_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = PluginStore::new(&dir);
        let wasm = sample_wasm();

        // api_version 不兼容
        let m = manifest_with_sha("a", &wasm, 999);
        assert!(store.install(m, &wasm).is_err());
        // 非法 id（路径穿越）
        let mut m = manifest_with_sha("../evil", &wasm, PLUGIN_API_VERSION);
        assert!(store.install(m.clone(), &wasm).is_err());
        // 未知权限
        m.id = "b".into();
        m.permissions = vec!["filesystem".into()];
        assert!(store.install(m.clone(), &wasm).is_err());
        // sha256 不匹配
        m.permissions = vec![];
        m.sha256 = "0".repeat(64);
        assert!(store.install(m.clone(), &wasm).is_err());
        // entry 穿越被拒
        m.sha256 = {
            let mut h = Sha256::new();
            h.update(&wasm);
            format!("{:x}", h.finalize())
        };
        m.entry = "..\\evil.wasm".into();
        assert!(store.install(m, &wasm).is_err());

        assert!(store.list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_tampered_wasm() {
        let dir = std::env::temp_dir().join(format!("nf_auto_plugin_tam_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = PluginStore::new(&dir);
        let wasm = sample_wasm();
        let m = manifest_with_sha("t", &wasm, PLUGIN_API_VERSION);
        store.install(m, &wasm).unwrap();
        // 库内文件被篡改 → sha256 复验拒绝
        let plugin_wasm = store.plugin_dir("t").join("main.wasm");
        std::fs::write(&plugin_wasm, b"tampered").unwrap();
        assert!(store.wasm_bytes("t").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
