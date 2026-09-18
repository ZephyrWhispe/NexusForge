//! SysModule 模块壳（docs/impl/06 SY）：Module trait 实现。
//!
//! - init：注入 PerfPort / RecycleBinPort + WinOps 数据面快照；构建包管理器集合与清理清单
//! - start：启动 1s 采样线程（sys.metrics）+ WinOps 回归检测（WUB 式防自愈，sys.verify_result）
//! - 无全局快捷键 ability；WinOps Tweak 引擎（docs/impl/08）数据面经 Port 注入

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use host_core::error::ModuleError;
use host_core::events::{Event, EventBus};
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};
use host_core::ports::{AppxPort, MaintenancePort, PerfPort, RecycleBinPort, RegistryOps, ServiceCtlPort, TaskTogglePort};

use crate::clean;
use crate::metrics::{MetricsBuffer, MetricsPoint};
use crate::pkg::PkgManager;

/// 采样间隔
const SAMPLE_INTERVAL_MS: u64 = 1000;

/// WinOps 回归检测数据面快照（init 时从 Ports 取，start 后台线程用）
#[derive(Clone)]
struct WinopsFace {
    registry: Arc<dyn RegistryOps>,
    tasks: Option<Arc<dyn TaskTogglePort>>,
    services: Option<Arc<dyn ServiceCtlPort>>,
    maintenance: Option<Arc<dyn MaintenancePort>>,
    appx: Option<Arc<dyn AppxPort>>,
}

pub struct SysModule {
    state: AtomicU8,
    perf: RwLock<Option<Arc<dyn PerfPort>>>,
    recycle: RwLock<Option<Arc<dyn RecycleBinPort>>>,
    bus: RwLock<Option<Arc<EventBus>>>,
    metrics: Arc<MetricsBuffer>,
    managers: Vec<Box<dyn PkgManager>>,
    targets: Vec<clean::CleanTarget>,
    sample_cancel: Arc<std::sync::atomic::AtomicBool>,
    sample_thread: RwLock<Option<std::thread::JoinHandle<()>>>,
    /// WinOps 数据面（init 注入；start 时回归检测用）
    winops: RwLock<Option<WinopsFace>>,
    /// appData 根（回归检测定位 backup.json 与外置目录）
    app_data_dir: PathBuf,
}

impl SysModule {
    pub fn new(app_data_dir: &std::path::Path) -> Self {
        Self {
            state: AtomicU8::new(0),
            perf: RwLock::new(None),
            recycle: RwLock::new(None),
            bus: RwLock::new(None),
            metrics: Arc::new(MetricsBuffer::new()),
            managers: crate::pkg::builtin_managers(),
            targets: clean::builtin_targets(),
            sample_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sample_thread: RwLock::new(None),
            winops: RwLock::new(None),
            app_data_dir: app_data_dir.to_path_buf(),
        }
    }

    /// IPC 层入口
    pub fn metrics(&self) -> &Arc<MetricsBuffer> {
        &self.metrics
    }

    pub fn managers(&self) -> &[Box<dyn PkgManager>] {
        &self.managers
    }

    pub fn manager(&self, id: &str) -> Option<&dyn PkgManager> {
        self.managers.iter().find(|m| m.id() == id).map(|b| b.as_ref())
    }

    pub fn targets(&self) -> &[clean::CleanTarget] {
        &self.targets
    }

    pub fn target(&self, id: &str) -> Option<&clean::CleanTarget> {
        self.targets.iter().find(|t| t.id == id)
    }

    pub fn recycle(&self) -> Option<Arc<dyn RecycleBinPort>> {
        self.recycle.read().ok().and_then(|g| g.clone())
    }

    fn publish_metrics(&self, point: &MetricsPoint) {
        if let Some(bus) = self.bus.read().ok().and_then(|g| g.clone()) {
            bus.publish(Event::new("sys.metrics", "sys", serde_json::to_value(point).unwrap_or_default()))
                .ok();
        }
    }

    /// 采样线程（start 在后台调用；1s 采样 → 缓冲 + 事件节流 1s）
    fn start_sampler(&self) {
        let already = self.sample_cancel.swap(false, Ordering::SeqCst);
        if already && self.sample_thread.read().map(|g| g.is_some()).unwrap_or(false) {
            return; // 线程仍在跑，cancel 复位即可
        }
        let Some(perf) = self.perf.read().ok().and_then(|g| g.clone()) else {
            return;
        };
        let cancel = self.sample_cancel.clone();
        let module_self = unsafe { Self::leak_self(self) };
        let handle = std::thread::Builder::new()
            .name("nf-sys-sampler".into())
            .spawn(move || loop {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(SAMPLE_INTERVAL_MS));
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                if let Some(point) = module_self.metrics.sample(perf.as_ref(), now) {
                    module_self.publish_metrics(&point);
                }
            })
            .ok();
        *self.sample_thread.write().expect("采样线程句柄锁污染") = handle;
    }

    /// 采样线程需要访问 &self 的发布/缓冲（模块生命周期 = 进程级，bootstrap 后常驻）
    unsafe fn leak_self(s: &Self) -> &'static Self {
        &*(s as *const Self)
    }

    /// WinOps 回归检测（docs/impl/08 §3.3 W4：WUB 式防自愈，v1 不做守护任务）。
    /// start 后台执行一次：备份原值 vs 当前值比对，回归 → sys.verify_result 事件（UI 黄条）。
    fn start_regression_check(&self) {
        let face = match self.winops.read().ok().and_then(|g| g.clone()) {
            Some(f) => f,
            None => return,
        };
        let Some(bus) = self.bus.read().ok().and_then(|g| g.clone()) else {
            return;
        };
        let dir = self.app_data_dir.clone();
        std::thread::Builder::new()
            .name("nf-sys-regression".into())
            .spawn(move || {
                let external = dir.join("winops").join("catalog");
                let Ok(tweaks) = crate::winops::load_catalog(Some(&external)) else {
                    return;
                };
                let ports = crate::winops::SysPorts {
                    registry: face.registry.as_ref(),
                    tasks: face.tasks.as_deref(),
                    services: face.services.as_deref(),
                    maintenance: face.maintenance.as_deref(),
                    appx: face.appx.as_deref(),
                };
                let store = crate::winops::BackupStore::open(&dir);
                let regressed = crate::winops::regression_check(&ports, &store, &tweaks);
                if !regressed.is_empty() {
                    tracing::warn!(count = regressed.len(), "WinOps 回归检测发现被系统改回的设置");
                    bus.publish(Event::new(
                        "sys.verify_result",
                        "sys",
                        serde_json::json!({ "regressed": regressed }),
                    ))
                    .ok();
                }
            })
            .ok();
    }
}

impl Module for SysModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "sys",
            name: "系统管理",
            version: "0.1.0",
            icon: Some("sys"),
            priority: 13,
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let perf = ctx
            .ports
            .get::<dyn PerfPort>()
            .ok_or_else(|| ModuleError::Init("PerfPort 未注册".into()))?;
        *self.perf.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(perf);
        *self.recycle.write().map_err(|_| ModuleError::Init("锁污染".into()))? = ctx.ports.get::<dyn RecycleBinPort>();
        *self.bus.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(ctx.event_bus.clone());
        // WinOps 数据面快照（注册缺失容忍——回归检测按 None 跳过对应比对）
        *self.winops.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(WinopsFace {
            registry: ctx
                .ports
                .get::<dyn RegistryOps>()
                .ok_or_else(|| ModuleError::Init("RegistryOps 未注册".into()))?,
            tasks: ctx.ports.get::<dyn TaskTogglePort>(),
            services: ctx.ports.get::<dyn ServiceCtlPort>(),
            maintenance: ctx.ports.get::<dyn MaintenancePort>(),
            appx: ctx.ports.get::<dyn AppxPort>(),
        });
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        self.start_sampler();
        self.start_regression_check();
        self.state.store(2, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        self.sample_cancel.store(true, Ordering::SeqCst);
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "metrics_note": {
                    "type": "string", "title": "监控说明",
                    "description": "1s PDH 采样，缓冲 300 点；WinOps Tweak 引擎见 docs/impl/08（后续里程碑）",
                    "default": ""
                }
            }
        })
    }

    fn apply_config(&self, _values: serde_json::Value) -> Result<(), ModuleError> {
        Ok(())
    }

    fn status(&self) -> ModuleState {
        match self.state.load(Ordering::SeqCst) {
            0 => ModuleState::Uninitialized,
            1 => ModuleState::Stopped,
            _ => ModuleState::Running,
        }
    }
}
