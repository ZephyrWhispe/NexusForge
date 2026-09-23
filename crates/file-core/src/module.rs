//! FileModule 模块壳（docs/impl/05 F1–F7）：Module trait 实现。
//!
//! - init：打开 [`FileService`]（pending_ops 在 appData 下；2 worker）
//! - start：崩溃恢复扫描 pending_ops，自动断点续传（docs/impl/01 S6.5）
//! - stop：不再接收新任务；存量任务由 worker 完成或进程退出自然终止

use parking_lot::RwLock;

use std::sync::Arc;

use host_core::error::ModuleError;
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};

use crate::conflict::ConflictPolicy;
use crate::error::FileError;
use crate::service::FileService;

/// 内置 worker 数上限（`max_concurrent` 的天花板）：线程在建队时定死，
/// 配置只收紧不超发——设置项与运行事实同源，禁"配 8 实际 2"的谎
pub const MAX_CONCURRENT_CEILING: usize = 2;

/// file 域配置真源（09 §6.2 T-B6-6：`config_schema()` 五键的唯一读者群，
/// [`FileModule::apply_config`] 的唯一产物）。`upload_kbps` 随 T-B6-7 执行器
/// 一并入表——本行没有上传字节路径，先声明就是自造死键（落地补记①）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FileConfig {
    /// Ask 之外的缺省冲突决议：UI 未逐单询问时队列按它走（预扫描 Ask 重映射）
    pub default_conflict_policy: ConflictPolicy,
    /// 删除默认进回收站；**只约束本地臂**——远端源恒彻底删除（承重⑥入队闸先行）
    pub delete_to_recycle: bool,
    /// 明文总闸：开启才允许非回环 FTP（FILE_REMOTE_006）；存管/会话口令
    /// 不受本闸豁免（FILE_REMOTE_007 恒拒）
    pub insecure_plaintext: bool,
    /// 下载限速（KB/s/腿的总预算，多腿经 `throttle_share_kbps` 摊分）；0 = 不限
    pub download_kbps: u32,
    /// 并发传输数（队列准入门 + 限速摊分母），合法域 `1..=MAX_CONCURRENT_CEILING`
    pub max_concurrent: usize,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            default_conflict_policy: ConflictPolicy::Ask,
            delete_to_recycle: true,
            insecure_plaintext: false,
            download_kbps: 0,
            max_concurrent: MAX_CONCURRENT_CEILING,
        }
    }
}

impl FileConfig {
    fn bad(key: &str, why: String) -> FileError {
        FileError::Config(format!("{key} {why}"))
    }

    /// 现运行态 ⊕ 补丁（**缺键 = 不动运行态**，B4/B5 既定纪律；全有或全无：
    /// 半套配置在跑比配置没生效更难查——一个坏值整批弹回并点名）
    pub fn merged(&self, values: &serde_json::Value) -> Result<FileConfig, FileError> {
        let mut next = self.clone();
        if let Some(v) = values.get("default_conflict_policy") {
            next.default_conflict_policy = serde_json::from_value::<ConflictPolicy>(v.clone())
                .map_err(|_| {
                    FileConfig::bad(
                        "default_conflict_policy",
                        format!("须为 ask/skip/overwrite/rename 之一，收到 {v}"),
                    )
                })?;
        }
        if let Some(v) = values.get("delete_to_recycle") {
            next.delete_to_recycle = v.as_bool().ok_or_else(|| {
                FileConfig::bad("delete_to_recycle", format!("须为布尔值，收到 {v}"))
            })?;
        }
        if let Some(v) = values.get("insecure_plaintext") {
            next.insecure_plaintext = v.as_bool().ok_or_else(|| {
                FileConfig::bad("insecure_plaintext", format!("须为布尔值，收到 {v}"))
            })?;
        }
        if let Some(v) = values.get("download_kbps") {
            let n = v
                .as_u64()
                .filter(|n| u32::try_from(*n).is_ok())
                .ok_or_else(|| {
                    FileConfig::bad(
                        "download_kbps",
                        format!("须为 0..=4294967295 的整数（0=不限），收到 {v}"),
                    )
                })? as u32;
            next.download_kbps = n;
        }
        if let Some(v) = values.get("max_concurrent") {
            let n = v
                .as_u64()
                .ok_or_else(|| FileConfig::bad("max_concurrent", format!("须为整数，收到 {v}")))?
                as usize;
            if !(1..=MAX_CONCURRENT_CEILING).contains(&n) {
                return Err(FileConfig::bad(
                    "max_concurrent",
                    format!(
                        "只许 1..={MAX_CONCURRENT_CEILING}（内置 worker 数 {MAX_CONCURRENT_CEILING}，配置可收紧不可超发），收到 {n}"
                    ),
                ));
            }
            next.max_concurrent = n;
        }
        Ok(next)
    }
}

pub struct FileModule {
    service: RwLock<Option<Arc<FileService>>>,
    /// 配置镜像：init 前的 apply_config 暂存位 + init 时的推送源。
    /// 真源在 [`FileService`] 的 Arc 锁里（各消费点现场读），两值恒同步。
    config: RwLock<FileConfig>,
    state: ModuleStateCell,
}

impl FileModule {
    pub fn new() -> Self {
        Self {
            service: RwLock::new(None),
            config: RwLock::new(FileConfig::default()),
            state: ModuleStateCell::new(),
        }
    }

    /// IPC 层入口（全部命令经此取服务；未 init 返回 None）
    pub fn service(&self) -> Option<Arc<FileService>> {
        self.service.read().clone()
    }
}

impl Default for FileModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for FileModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "file",
            name: "文件与存储",
            version: "0.1.0",
            icon: Some("file"),
            priority: priority_of("file"),
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let svc = FileService::open(&ctx.app_data_dir, ctx.event_bus.clone(), ctx.ports.clone())
            .map_err(|e| ModuleError::Storage(e.to_string()))?;
        // init 前到达的配置（首启派发序不保证模块先 init）在此推送进真源；
        // 默认值推送同样无害且让 max_concurrent 的准入门状态恒与镜像同源
        svc.set_config(self.config.read().clone());
        *self.service.write() = Some(Arc::new(svc));
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        // 崩溃恢复：pending_ops 断点续传（失败仅告警，不阻断启动）
        if let Some(svc) = self.service() {
            let resumed = svc.resume_pending();
            if resumed > 0 {
                tracing::info!(count = resumed, "文件操作崩溃恢复重入队");
            }
        }
        self.state.set(ModuleState::Running);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        // worker 随服务句柄存活到进程退出；暂停全部活跃操作保证断点落盘
        if let Some(svc) = self.service() {
            for p in svc.ops_active() {
                if p.state == crate::ops::OpState::Running || p.state == crate::ops::OpState::Queued
                {
                    let _ = svc.op_pause(&p.op_id);
                }
            }
        }
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "default_conflict_policy": {
                    "type": "string", "title": "默认同名冲突策略",
                    "enum": ["ask", "skip", "overwrite", "rename"],
                    "default": "ask"
                },
                "delete_to_recycle": {
                    "type": "boolean", "title": "删除默认进回收站",
                    "description": "关闭为永久删除（docs/impl/05 F 风险项）；只约束本地臂，远端源恒彻底删除（承重⑥）",
                    "default": true
                },
                "insecure_plaintext": {
                    "type": "boolean", "title": "允许明文远程连接（危险）",
                    "description": "开启即允许非回环 FTP 明文过网——口令以裸文本穿越网络（FILE_REMOTE_006 闸）。存管/会话口令在明文链路恒拒（FILE_REMOTE_007），本闸开不了那个豁免。默认关",
                    "default": false
                },
                "download_kbps": {
                    "type": "integer", "title": "下载限速（KB/s）",
                    "description": "所有并发下载腿共享的总预算（逐腿摊分）；0 = 不限",
                    "minimum": 0, "default": 0
                },
                "max_concurrent": {
                    "type": "integer", "title": "并发传输数",
                    "description": "上限 = 内置 worker 数 2：配置只收紧不超发（线程在建队时定死）",
                    "minimum": 1, "maximum": 2, "default": 2
                }
            }
        })
    }

    /// 派发口（T-B6-6 起有真消费者）：先经 [`FileConfig::merged`] 全有或全无
    /// 校验，坏值弹 `ModuleError::Config` → 总线 `host.config_rejected`，
    /// 运行态一个键都不动；好值即刻写进服务真源（**不重启即生效**）
    fn apply_config(&self, values: serde_json::Value) -> Result<(), ModuleError> {
        let base = match self.service() {
            Some(s) => s.config(),
            None => self.config.read().clone(),
        };
        let next = base
            .merged(&values)
            .map_err(|e| ModuleError::Config(e.to_string()))?;
        if next.insecure_plaintext && !base.insecure_plaintext {
            tracing::warn!(
                "insecure_plaintext 总闸开启：非回环明文 FTP 连接被允许（口令裸奔过网），FILE_REMOTE_006 闸已开"
            );
        }
        if let Some(s) = self.service() {
            s.set_config(next.clone());
        }
        *self.config.write() = next;
        Ok(())
    }

    fn status(&self) -> ModuleState {
        self.state.get()
    }

    fn set_status(&self, state: ModuleState) {
        self.state.set(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_core::events::EventBus;
    use host_core::ports::Ports;
    use std::path::PathBuf;

    #[test]
    fn module_lifecycle_with_tmp_appdata() {
        let dir = std::env::temp_dir().join(format!("nf_file_mod_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let m = FileModule::new();
        assert_eq!(m.status(), ModuleState::Uninitialized);
        let ctx = Arc::new(ModuleContext {
            app_data_dir: dir.clone(),
            ports: Arc::new(Ports::new()),
            event_bus: Arc::new(EventBus::new()),
        });
        m.init(ctx).unwrap();
        assert_eq!(m.status(), ModuleState::Stopped);
        assert!(m.service().is_some());
        m.start().unwrap();
        assert_eq!(m.status(), ModuleState::Running);
        m.stop().unwrap();
        assert_eq!(m.status(), ModuleState::Stopped);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 任务书（09 §6.2 T-B6-6）字面测试名：缺键 = 不动运行态（B4/B5 同纪律），
    /// 含正对照——**显式填默认值确改值**（"没填"与"填了默认"是两件事）
    #[test]
    #[allow(non_snake_case)]
    fn fileConfig_merged_absentKeysKeepRuntimeState() {
        let runtime = FileConfig {
            default_conflict_policy: ConflictPolicy::Skip,
            delete_to_recycle: false,
            insecure_plaintext: true,
            download_kbps: 500,
            max_concurrent: 1,
        };
        // 全缺省补丁 ⇒ 逐值原样
        assert_eq!(runtime.merged(&serde_json::json!({})).unwrap(), runtime);
        // 单键补丁 ⇒ 其余四键保持现值（现值故意全部≠默认，缺键回默认在这里必炸）
        let patched = runtime
            .merged(&serde_json::json!({ "download_kbps": 90 }))
            .unwrap();
        assert_eq!(patched.download_kbps, 90);
        assert_eq!(
            (
                patched.default_conflict_policy,
                patched.delete_to_recycle,
                patched.insecure_plaintext,
                patched.max_concurrent
            ),
            (ConflictPolicy::Skip, false, true, 1)
        );
        // 正对照：显式写回默认值 = 真改值（0 从 500 归位、true 总闸不动仍 true）
        let explicit_default = runtime
            .merged(&serde_json::json!({ "download_kbps": 0 }))
            .unwrap();
        assert_eq!(explicit_default.download_kbps, 0, "填了 0 就必须看见 0");
        // 未知键（含旧盘残留）零影响，也不写"已迁移"
        assert_eq!(
            runtime
                .merged(&serde_json::json!({ "upload_kbps": 66, "an_old_key": 7 }))
                .unwrap(),
            runtime
        );
    }

    /// 任务书（09 §6.2 T-B6-6）字面测试名：一个坏值整批弹回——消息点名坏键，
    /// 同批的好值一个都不许偷偷进运行态（半套配置在跑比配置没生效更难查）
    #[test]
    #[allow(non_snake_case)]
    fn fileConfig_badValue_rejectsNamingKeyAndWholeSet() {
        let d = FileConfig::default();
        // 逐键点名：坏类型 / 越界值各弹各的
        let e = d
            .merged(&serde_json::json!({ "download_kbps": "fast" }))
            .expect_err("字符串不是合法限速值");
        assert!(e.to_string().contains("download_kbps"), "须点名坏键: {e}");
        let e = d
            .merged(&serde_json::json!({ "max_concurrent": 8 }))
            .expect_err("超发（>内置 worker 数）必须拒");
        assert!(e.to_string().contains("max_concurrent"), "须点名坏键: {e}");
        let e = d
            .merged(&serde_json::json!({ "default_conflict_policy": "yolo" }))
            .expect_err("策略枚举外的值必须拒");
        assert!(
            e.to_string().contains("ask/skip/overwrite/rename"),
            "拒语要点名合法集: {e}"
        );
        // 全有或全无：坏键混在好键批里 ⇒ 整批不落地
        assert!(d
            .merged(&serde_json::json!({
                "delete_to_recycle": false,
                "download_kbps": -1
            }))
            .is_err());
        // 模块口：apply_config 弹回后镜像一个键都不动（init 前形态）
        let m = FileModule::new();
        assert!(m
            .apply_config(serde_json::json!({
                "delete_to_recycle": false,
                "max_concurrent": 3
            }))
            .is_err());
        assert_eq!(
            *m.config.read(),
            FileConfig::default(),
            "坏批不得染指运行态"
        );
    }

    /// 任务书（09 §6.2 T-B6-6）字面测试名：真 ConfigStore + 真派发循环，
    /// 写盘→apply_config→入队行为三态不重启（服务句柄恒为同一个 Arc）
    #[tokio::test(flavor = "multi_thread")]
    #[allow(non_snake_case)]
    async fn fileConfig_appliedViaConfigStoreDispatch_withoutRestart() {
        let dir = std::env::temp_dir().join(format!("nf_file_mod_cfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bus = Arc::new(host_core::events::EventBus::new());
        let store_cfg = Arc::new(host_core::config::ConfigStore::new(
            dir.join("config"),
            bus.clone(),
        ));
        let module = Arc::new(FileModule::new());
        store_cfg.register_schema("file", module.config_schema());
        let registry = Arc::new(host_core::registry::ModuleRegistry::new(bus.clone()));
        registry.register(module.clone()).unwrap();
        // 订阅早于第一次写：派发循环只认订阅之后的事件，订阅晚了就是"写了没生效"
        let rx = bus.subscribe("host.config_changed").unwrap();
        tokio::spawn(host_core::registry::run_config_feed(
            rx,
            registry.clone(),
            store_cfg.clone(),
        ));
        let ctx = Arc::new(ModuleContext {
            app_data_dir: dir.clone(),
            ports: Arc::new(Ports::new()),
            event_bus: bus.clone(),
        });
        for (_, r) in registry.init_all(ctx).await {
            r.expect("init 应成功");
        }
        let svc0 = module.service().expect("init 后服务在场");

        let poll_until = |why: &str, f: &dyn Fn() -> bool| {
            for _ in 0..200 {
                if f() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            panic!("配置未在 5s 内生效: {why}");
        };

        // 态①默认：删除请求原样通过（回收站开）
        let delete_spec = || crate::ops::OpSpec {
            kind: crate::ops::OpKind::Delete,
            srcs: vec![crate::ops::OpEndpoint::local(PathBuf::from("a.txt"))],
            dst: crate::ops::OpEndpoint::local(PathBuf::from(".")),
            policy: ConflictPolicy::Ask,
            recycle: true,
        };
        assert!(
            FileService::apply_config_to_spec(&svc0.config(), delete_spec()).recycle,
            "默认回收站开 ⇒ 入队请求原样"
        );

        // 态②写盘两键：回收站关 + 并发收紧到 1 —— 不重启即反映到入队行为
        store_cfg
            .set_module(
                "file",
                serde_json::json!({ "delete_to_recycle": false, "max_concurrent": 1 }),
            )
            .unwrap();
        poll_until("delete_to_recycle=false 到达运行态", &|| {
            !module.service().unwrap().config().delete_to_recycle
        });
        let svc1 = module.service().unwrap();
        assert_eq!(svc1.config().max_concurrent, 1);
        assert!(
            !FileService::apply_config_to_spec(&svc1.config(), delete_spec()).recycle,
            "配置关 ⇒ 入队删除必须被强制直删"
        );

        // 态③只写第三键：前两键保持现值（缺键不动经真派发链成立），
        // 且服务句柄从未换过——这一切发生在同一个进程生命周期内
        store_cfg
            .set_module("file", serde_json::json!({ "insecure_plaintext": true }))
            .unwrap();
        poll_until("insecure_plaintext=true 到达运行态", &|| {
            module.service().unwrap().config().insecure_plaintext
        });
        let svc2 = module.service().unwrap();
        assert!(!svc2.config().delete_to_recycle, "上轮现值必须还在");
        assert_eq!(svc2.config().max_concurrent, 1, "上轮现值必须还在");
        assert!(
            Arc::ptr_eq(&svc0, &svc2),
            "配置生效不得以重启服务为代价（不重启即生效）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 任务书（09 §6.2 T-B6-6）字面测试名：`config_schema()` 声明的每个键
    /// 必须有 module.rs 之外的读者（防"为过门禁而加死键"）；schema 键集与
    /// [`FileConfig`] 序列化键集恰等；schema `default` 与 `Default` 逐值同源
    #[test]
    #[allow(non_snake_case)]
    fn dead_config_keys_are_revived_or_removed() {
        let src_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut reader_lines: Vec<Vec<String>> = Vec::new();
        for entry in walkdir::WalkDir::new(&src_root).into_iter() {
            let entry = entry.expect("walkdir 不应失败");
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            // module.rs 是声明现场（schema + 结构体 + merged），读者必须在别处
            if path.file_name() == Some(std::ffi::OsStr::new("module.rs")) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            // 判据按行找真读者：注释行里的键名不算消费（防"写句注释过门禁"）
            reader_lines.push(
                text.lines()
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .map(|l| l.to_owned())
                    .collect(),
            );
        }
        let key_has_reader = |key: &str| {
            reader_lines
                .iter()
                .any(|lines| lines.iter().any(|l| l.contains(key)))
        };

        let schema = FileModule::new().config_schema();
        let props = schema["properties"].as_object().expect("schema 应为对象");
        let default_ser = serde_json::to_value(FileConfig::default()).unwrap();
        let default_obj = default_ser.as_object().expect("FileConfig 可序列化");

        let mut keys: Vec<&String> = props.keys().collect();
        keys.sort();
        let mut fields: Vec<&String> = default_obj.keys().collect();
        fields.sort();
        assert_eq!(keys, fields, "schema 键集必须恰等 FileConfig 字段集");
        for key in &keys {
            assert!(
                key_has_reader(key),
                "死键：{key} 在 config_schema 声明却在 module.rs 之外零读者"
            );
            assert_eq!(
                props[*key].get("default"),
                default_obj.get(*key),
                "schema default 与 Default 必须逐值同源: {key}"
            );
        }
    }
}
