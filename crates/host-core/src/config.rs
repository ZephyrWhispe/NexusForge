//! 配置中心（docs/impl/01 S6.1）
//!
//! - 全局配置 `{config}/global.json` + 每模块配置 `{config}/{module}.json`
//! - 每文件带 `schema_version`，升级时先备份（保留 3 份）再执行迁移链，失败回滚
//! - `set_*` 必须先过注册的 JSON Schema（jsonschema crate）再原子写，随后广播 `host.config_changed`
//! - 文件写入一律"临时文件 + rename"（docs/IMPLEMENTATION.md 通用规约 5）

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::codes;
use crate::error::AppError;
use crate::events::{Event, EventBus};

/// 当前全局配置 schema 版本
pub const GLOBAL_SCHEMA_VERSION: u32 = 1;
/// 备份保留份数
const BACKUP_KEEP: usize = 3;

/// 全局配置（强类型投影）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GlobalConfig {
    pub schema_version: u32,
    /// 启用的模块 id 列表（未列出的模块不注册）
    pub enabled_modules: Vec<String>,
    /// UI 语言（BCP-47）
    pub language: String,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            schema_version: GLOBAL_SCHEMA_VERSION,
            enabled_modules: vec!["clipboard".into(), "screenshot".into(), "ocr".into()],
            language: "zh-CN".into(),
        }
    }
}

pub struct ConfigStore {
    dir: PathBuf,
    bus: Arc<EventBus>,
    /// 各模块注册的 JSON Schema（模块 init 时调用 register_schema）
    schemas: RwLock<std::collections::HashMap<String, serde_json::Value>>,
}

impl ConfigStore {
    pub fn new(dir: PathBuf, bus: Arc<EventBus>) -> Self {
        Self { dir, bus, schemas: RwLock::new(std::collections::HashMap::new()) }
    }

    // ---------------- 生命周期 ----------------

    /// 启动加载：缺失则写默认；版本落后则备份 + 迁移
    pub fn load(&self) -> Result<GlobalConfig, AppError> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| AppError::Storage { code: codes::host::HOST_CONFIG_002.into(), message: format!("创建配置目录失败: {e}") })?;
        let path = self.global_path();
        if !path.exists() {
            let cfg = GlobalConfig::default();
            let v = serde_json::to_value(&cfg)
                .map_err(|e| cfg_err(codes::host::HOST_CONFIG_002, e))?;
            write_atomic(&path, &v)?;
            return Ok(cfg);
        }
        let mut v = read_json(&path)?;
        let ver = v.get("schema_version").and_then(|x| x.as_u64()).unwrap_or(0);
        if ver < GLOBAL_SCHEMA_VERSION as u64 {
            backup(&path)?;
        }
        migrate_global(&mut v)?;
        write_atomic(&path, &v)?;
        let cfg: GlobalConfig = serde_json::from_value(v)
            .map_err(|e| cfg_err(codes::host::HOST_CONFIG_001, e))?;
        Ok(cfg)
    }

    // ---------------- 全局配置 ----------------

    pub fn get_global(&self) -> Result<GlobalConfig, AppError> {
        let v = read_json(&self.global_path())?;
        serde_json::from_value(v).map_err(|e| cfg_err(codes::host::HOST_CONFIG_001, e))
    }

    pub fn set_global(&self, values: serde_json::Value) -> Result<(), AppError> {
        let schema = global_schema();
        validate(&schema, &values)?;
        let mut v = values;
        migrate_global(&mut v)?;
        backup(&self.global_path())?;
        write_atomic(&self.global_path(), &v)?;
        self.publish_changed("global");
        Ok(())
    }

    // ---------------- 模块配置 ----------------

    /// 模块 init 时登记自己的配置 schema（设置中心自动渲染的数据源）
    pub fn register_schema(&self, module: &str, schema: serde_json::Value) {
        self.schemas
            .write()
            .expect("schema 表写锁")
            .insert(module.to_owned(), schema);
    }

    pub fn schema_of(&self, module: &str) -> Option<serde_json::Value> {
        self.schemas.read().expect("schema 表读锁").get(module).cloned()
    }

    pub fn get_module(&self, id: &str) -> Result<serde_json::Value, AppError> {
        let p = self.module_path(id);
        if !p.exists() {
            return Ok(serde_json::json!({}));
        }
        read_json(&p)
    }

    pub fn set_module(&self, id: &str, values: serde_json::Value) -> Result<(), AppError> {
        let schema = self.schema_of(id).ok_or_else(|| {
            AppError::module(
                codes::host::HOST_CONFIG_001,
                format!("模块 {id} 未注册配置 schema"),
                None,
            )
        })?;
        validate(&schema, &values)?;
        let p = self.module_path(id);
        backup(&p)?;
        write_atomic(&p, &values)?;
        self.publish_changed(id);
        Ok(())
    }

    // ---------------- 内部 ----------------

    fn global_path(&self) -> PathBuf {
        self.dir.join("global.json")
    }
    fn module_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }
    fn publish_changed(&self, module: &str) {
        self.bus
            .publish(Event::new(
                "host.config_changed",
                "host",
                serde_json::json!({ "module": module }),
            ))
            .ok();
    }
}

// ---------------- 校验 / 迁移 / IO ----------------

fn cfg_err(code: &str, e: impl std::fmt::Display) -> AppError {
    AppError::Config { code: code.into(), message: e.to_string() }
}

fn validate(schema: &serde_json::Value, instance: &serde_json::Value) -> Result<(), AppError> {
    let validator = jsonschema::validator_for(schema)
        .map_err(|e| cfg_err(codes::host::HOST_CONFIG_001, e))?;
    let errs: Vec<String> = validator.iter_errors(instance).map(|e| e.to_string()).collect();
    if errs.is_empty() {
        Ok(())
    } else {
        Err(AppError::Config {
            code: codes::host::HOST_CONFIG_002.into(),
            message: format!("配置校验失败: {}", errs.join("; ")),
        })
    }
}

/// 全局配置迁移链：逐版本推进到 [`GLOBAL_SCHEMA_VERSION`]。
/// 新版本在此追加 match 分支（docs/impl/01 S6.1 迁移算法）。
fn migrate_global(v: &mut serde_json::Value) -> Result<(), AppError> {
    let mut version = v.get("schema_version").and_then(|x| x.as_u64()).unwrap_or(0);
    while version < GLOBAL_SCHEMA_VERSION as u64 {
        match version {
            0 => {
                // v0 → v1：补充 language 字段（示例迁移，机制演示）
                let obj = v.as_object_mut().ok_or_else(|| {
                    cfg_err(codes::host::HOST_CONFIG_001, "全局配置不是 JSON 对象")
                })?;
                obj.entry("language").or_insert(serde_json::json!("zh-CN"));
                obj.insert("schema_version".to_owned(), serde_json::json!(1));
            }
            other => {
                return Err(cfg_err(
                    codes::host::HOST_CONFIG_001,
                    format!("无 v{other} → v{} 的迁移脚本", GLOBAL_SCHEMA_VERSION),
                ));
            }
        }
        version += 1;
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<serde_json::Value, AppError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| cfg_err(codes::host::HOST_CONFIG_002, e))?;
    serde_json::from_str(&raw).map_err(|e| cfg_err(codes::host::HOST_CONFIG_001, e))
}

fn write_atomic(path: &Path, v: &serde_json::Value) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| cfg_err(codes::host::HOST_CONFIG_002, e))?;
    }
    let tmp = path.with_extension("json.tmp");
    let raw = serde_json::to_string_pretty(v)
        .map_err(|e| cfg_err(codes::host::HOST_CONFIG_002, e))?;
    std::fs::write(&tmp, raw).map_err(|e| cfg_err(codes::host::HOST_CONFIG_002, e))?;
    std::fs::rename(&tmp, path).map_err(|e| cfg_err(codes::host::HOST_CONFIG_002, e))?;
    Ok(())
}

/// 迁移/覆盖前备份；仅保留最近 [`BACKUP_KEEP`] 份
fn backup(path: &Path) -> Result<(), AppError> {
    if !path.exists() {
        return Ok(());
    }
    let dir = path.parent().unwrap_or(Path::new(".")).join("backups");
    std::fs::create_dir_all(&dir).map_err(|e| cfg_err(codes::host::HOST_CONFIG_002, e))?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("config");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dest = dir.join(format!("{name}.{}.json", ts));
    std::fs::copy(path, &dest).map_err(|e| cfg_err(codes::host::HOST_CONFIG_002, e))?;

    let mut backups: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| cfg_err(codes::host::HOST_CONFIG_002, e))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_stem().and_then(|s| s.to_str()).unwrap_or("").starts_with(name))
        .collect();
    backups.sort();
    while backups.len() > BACKUP_KEEP {
        let oldest = backups.remove(0);
        let _ = std::fs::remove_file(oldest);
    }
    Ok(())
}

/// 全局配置 schema（settings 中心全局页渲染源）
pub fn global_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "schema_version": { "type": "integer", "minimum": 1 },
            "enabled_modules": {
                "type": "array",
                "items": { "type": "string" }
            },
            "language": { "type": "string" }
        },
        "required": ["schema_version", "enabled_modules", "language"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventBus;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nf_cfg_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn store(tag: &str) -> ConfigStore {
        ConfigStore::new(temp_dir(tag), Arc::new(EventBus::new()))
    }

    #[test]
    fn load_creates_default_global() {
        let s = store("default");
        let cfg = s.load().unwrap();
        assert_eq!(cfg.schema_version, GLOBAL_SCHEMA_VERSION);
        assert!(cfg.enabled_modules.contains(&"clipboard".to_string()));
        assert!(s.global_path().exists());
    }

    #[test]
    fn migration_backfills_language_and_backups() {
        let s = store("migrate");
        std::fs::create_dir_all(s.dir.clone()).unwrap();
        // 写一份 v0 旧配置（无 language 字段）
        std::fs::write(
            s.global_path(),
            r#"{"schema_version":0,"enabled_modules":["clipboard"]}"#,
        )
        .unwrap();
        let cfg = s.load().unwrap();
        assert_eq!(cfg.language, "zh-CN");
        assert_eq!(cfg.schema_version, 1);
        // 备份生成
        let backups = std::fs::read_dir(s.dir.join("backups")).unwrap().count();
        assert!(backups >= 1, "迁移前应有备份");
    }

    #[test]
    fn set_module_rejects_invalid_against_schema() {
        let s = store("schema");
        s.register_schema(
            "clipboard",
            serde_json::json!({
                "type": "object",
                "properties": { "max_entries": { "type": "integer", "minimum": 100 } },
                "required": ["max_entries"]
            }),
        );
        // 缺字段 → 拒绝
        assert!(s.set_module("clipboard", serde_json::json!({})).is_err());
        // 越界 → 拒绝
        assert!(s
            .set_module("clipboard", serde_json::json!({ "max_entries": 1 }))
            .is_err());
        // 合法 → 落盘
        s.set_module("clipboard", serde_json::json!({ "max_entries": 5000 }))
            .unwrap();
        assert_eq!(
            s.get_module("clipboard").unwrap()["max_entries"],
            serde_json::json!(5000)
        );
    }

    #[test]
    fn set_module_without_schema_fails() {
        let s = store("noschema");
        assert!(s.set_module("ghost", serde_json::json!({})).is_err());
    }
}
