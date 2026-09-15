//! 模块注册表与生命周期管理（docs/impl/01 S5）
//!
//! 核心保证：
//! - **panic 隔离**：模块 init/start/stop 均在 `spawn_blocking` 中执行，
//!   panic 被 join 错误捕获并转为 [`ModuleError::Panicked`]，宿主存活；
//! - **状态机**：`Uninitialized → Stopped → Running`；异常/panic → `Error`；可 restart；
//! - **启动不互相阻断**：某模块 init 失败只记录结果，其余模块继续；
//! - **停止限时**：stop 超过 [`STOP_TIMEOUT`] 视为失败并标记 Error。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::time::timeout;

use crate::codes;
use crate::error::{AppError, ModuleError};
use crate::events::Event;
use crate::events::EventBus;
use crate::module::{Module, ModuleContext, ModuleInfo, ModuleState};
use crate::ports::{Port, Ports};

/// 单次 stop 允许的最长耗时
pub const STOP_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ModuleRegistry {
    bus: Arc<EventBus>,
    modules: RwLock<HashMap<String, Arc<dyn Module>>>,
    order: RwLock<Vec<String>>,
    states: RwLock<HashMap<String, ModuleState>>,
    ctx: RwLock<Option<Arc<ModuleContext>>>,
    /// 能力注册表（TrayProvider / HotkeyProvider 等多实例收集，S6.3/S7 消费）
    abilities: Ports,
}

impl ModuleRegistry {
    pub fn new(bus: Arc<EventBus>) -> Self {
        Self {
            bus,
            modules: RwLock::new(HashMap::new()),
            order: RwLock::new(Vec::new()),
            states: RwLock::new(HashMap::new()),
            ctx: RwLock::new(None),
            abilities: Ports::new(),
        }
    }

    /// 注册模块能力（多实例：多个模块可同时实现 TrayProvider 等）
    pub fn register_ability<T: ?Sized + Port>(&self, impl_: Arc<T>) {
        self.abilities.register_multi(impl_);
    }

    /// 能力注册表只读访问（托盘聚合 / 快捷键批量注册用）
    pub fn abilities(&self) -> &Ports {
        &self.abilities
    }

    /// 全部模块元信息（按注册顺序；IPC host_modules_status 数据源）
    pub fn infos(&self) -> Vec<ModuleInfo> {
        self.order
            .read()
            .expect("注册序读锁")
            .iter()
            .filter_map(|id| self.get(id).map(|m| m.info()))
            .collect()
    }

    /// 注册模块（id 去重）。注册后状态为 Uninitialized。
    pub fn register(&self, module: Arc<dyn Module>) -> Result<(), AppError> {
        let id = module.info().id.to_owned();
        {
            let mut modules = self.modules.write().expect("注册表写锁");
            if modules.contains_key(&id) {
                return Err(AppError::module(
                    codes::host::HOST_REGISTRY_002,
                    format!("模块 {id} 重复注册"),
                    None,
                ));
            }
            modules.insert(id.clone(), module);
        }
        self.order.write().expect("注册序写锁").push(id.clone());
        self.states
            .write()
            .expect("状态表写锁")
            .insert(id, ModuleState::Uninitialized);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Module>> {
        self.modules.read().expect("注册表读锁").get(id).cloned()
    }

    /// 按注册顺序初始化全部模块。单个失败不阻断其余模块。
    pub async fn init_all(&self, ctx: Arc<ModuleContext>) -> Vec<(String, Result<(), ModuleError>)> {
        *self.ctx.write().expect("上下文写锁") = Some(ctx.clone());
        let modules = self.ordered();
        let mut results = Vec::with_capacity(modules.len());
        for (id, module) in modules {
            let m = module.clone();
            let c = ctx.clone();
            let r = match tokio::task::spawn_blocking(move || m.init(c)).await {
                Ok(r) => r,
                Err(je) if je.is_panic() => Err(ModuleError::Panicked(
                    je.into_panic().downcast_ref::<String>().cloned().unwrap_or_else(|| "init panic".into()),
                )),
                Err(je) => Err(ModuleError::Init(je.to_string())),
            };
            self.set_state(&id, if r.is_ok() { ModuleState::Stopped } else { ModuleState::Error });
            if let Err(e) = &r {
                self.report_failure(&id, e).await;
            }
            results.push((id, r));
        }
        results
    }

    /// 按注册顺序启动全部已初始化模块
    pub async fn start_all(&self) -> Vec<(String, Result<(), ModuleError>)> {
        let mut results = Vec::new();
        for (id, module) in self.ordered() {
            let r = self.start_one(&id, module).await;
            results.push((id, r));
        }
        results
    }

    /// 逆序停止全部模块
    pub async fn stop_all(&self) {
        let mut ids = self.order.read().expect("注册序读锁").clone();
        ids.reverse();
        for id in ids {
            if let Some(m) = self.get(&id) {
                let _ = self.stop_one(&id, m).await;
            }
        }
    }

    /// 重启单个模块：stop → init → start
    pub async fn restart(&self, id: &str) -> Result<(), AppError> {
        let module = self.get(id).ok_or_else(|| {
            AppError::module(
                codes::host::HOST_REGISTRY_001,
                format!("模块 {id} 未注册"),
                None,
            )
        })?;
        let ctx = self
            .ctx
            .read()
            .expect("上下文读锁")
            .clone()
            .ok_or_else(|| {
                AppError::module(
                    codes::host::HOST_CONFIG_001,
                    "宿主尚未初始化，无法重启模块",
                    None,
                )
            })?;
        // restart 中 stop 失败不阻断：继续 init/start 重建该模块
        let _ = self.stop_one(id, module.clone()).await;
        self.init_one(id, module.clone(), ctx).await?;
        self.start_one(id, module).await.map_err(|e| AppError::from(e))?;
        Ok(())
    }

    /// 全部模块状态（按注册顺序）
    pub fn status_all(&self) -> Vec<(String, ModuleState)> {
        self.order
            .read()
            .expect("注册序读锁")
            .iter()
            .map(|id| {
                let st = self
                    .states
                    .read()
                    .expect("状态表读锁")
                    .get(id)
                    .copied()
                    .unwrap_or(ModuleState::Uninitialized);
                (id.clone(), st)
            })
            .collect()
    }

    // ------------------------------------------------------------------

    fn ordered(&self) -> Vec<(String, Arc<dyn Module>)> {
        self.order
            .read()
            .expect("注册序读锁")
            .iter()
            .filter_map(|id| self.get(id).map(|m| (id.clone(), m)))
            .collect()
    }

    fn set_state(&self, id: &str, state: ModuleState) {
        self.states
            .write()
            .expect("状态表写锁")
            .insert(id.to_owned(), state);
    }

    async fn report_failure(&self, id: &str, e: &ModuleError) {
        tracing::error!(module = id, error = %e, "模块失败");
        self.bus
            .publish(Event::new(
                "host.module_crashed",
                "host",
                serde_json::json!({ "module": id, "message": e.to_string() }),
            ))
            .ok();
    }

    async fn init_one(
        &self,
        id: &str,
        module: Arc<dyn Module>,
        ctx: Arc<ModuleContext>,
    ) -> Result<(), ModuleError> {
        let r = match tokio::task::spawn_blocking(move || module.init(ctx)).await {
            Ok(r) => r,
            Err(je) if je.is_panic() => Err(ModuleError::Panicked(
                je.into_panic().downcast_ref::<String>().cloned().unwrap_or_else(|| "init panic".into()),
            )),
            Err(je) => Err(ModuleError::Init(je.to_string())),
        };
        self.set_state(id, if r.is_ok() { ModuleState::Stopped } else { ModuleState::Error });
        if let Err(e) = &r {
            self.report_failure(id, e).await;
        }
        r
    }

    async fn start_one(&self, id: &str, module: Arc<dyn Module>) -> Result<(), ModuleError> {
        let r = match tokio::task::spawn_blocking(move || module.start()).await {
            Ok(r) => r,
            Err(je) if je.is_panic() => Err(ModuleError::Panicked(
                je.into_panic().downcast_ref::<String>().cloned().unwrap_or_else(|| "start panic".into()),
            )),
            Err(je) => Err(ModuleError::Start(je.to_string())),
        };
        self.set_state(id, if r.is_ok() { ModuleState::Running } else { ModuleState::Error });
        self.publish_state(id).await;
        if let Err(e) = &r {
            self.report_failure(id, e).await;
        }
        r
    }

    async fn stop_one(&self, id: &str, module: Arc<dyn Module>) -> Result<(), ModuleError> {
        // 三层嵌套：timeout(Elapsed) → JoinHandle(JoinError) → 模块返回值(Result<(), ModuleError>)
        let r = match timeout(STOP_TIMEOUT, tokio::task::spawn_blocking(move || module.stop())).await
        {
            Err(_elapsed) => Err(ModuleError::Stop("stop 超时 5s，已强制返回".into())),
            Ok(Err(je)) if je.is_panic() => Err(ModuleError::Panicked(
                je.into_panic().downcast_ref::<String>().cloned().unwrap_or_else(|| "stop panic".into()),
            )),
            Ok(Err(je)) => Err(ModuleError::Stop(je.to_string())),
            Ok(Ok(Err(me))) => Err(me),
            Ok(Ok(Ok(()))) => Ok(()),
        };
        self.set_state(id, if r.is_ok() { ModuleState::Stopped } else { ModuleState::Error });
        r
    }

    async fn publish_state(&self, id: &str) {
        if let Some(state) = self.states.read().expect("状态表读锁").get(id).copied() {
            self.bus
                .publish(Event::new(
                    "host.module_state",
                    "host",
                    serde_json::json!({ "key": id, "module": id, "state": state }),
                ))
                .ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Ports;
    use std::sync::atomic::{AtomicU8, Ordering};

    // state: 0=Uninitialized 1=Stopped 2=Running
    struct FakeModule {
        state: AtomicU8,
        panic_on_start: bool,
        panicked_once: AtomicU8,
    }
    impl FakeModule {
        fn new(panic_on_start: bool) -> Self {
            Self { state: AtomicU8::new(0), panic_on_start, panicked_once: AtomicU8::new(0) }
        }
    }
    impl Module for FakeModule {
        fn info(&self) -> crate::module::ModuleInfo {
            crate::module::ModuleInfo {
                id: "fake",
                name: "假模块",
                version: "0.1.0",
                icon: None,
                priority: 100,
            }
        }
        fn init(&self, _ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
            self.state.store(1, Ordering::SeqCst);
            Ok(())
        }
        fn start(&self) -> Result<(), ModuleError> {
            // panic_on_start=true 时仅首次 start panic（验证重启可恢复）
            if self.panic_on_start && self.panicked_once.swap(1, Ordering::SeqCst) == 0 {
                panic!("故意 panic：验证隔离");
            }
            self.state.store(2, Ordering::SeqCst);
            Ok(())
        }
        fn stop(&self) -> Result<(), ModuleError> {
            self.state.store(1, Ordering::SeqCst);
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

    fn registry_with(modules: Vec<FakeModule>) -> (Arc<ModuleRegistry>, Arc<EventBus>, Vec<Arc<FakeModule>>) {
        let bus = Arc::new(EventBus::new());
        let reg = Arc::new(ModuleRegistry::new(bus.clone()));
        let mut arcs = Vec::new();
        for m in modules {
            let a: Arc<FakeModule> = Arc::new(m);
            arcs.push(a.clone());
            reg.register(a).unwrap();
        }
        (reg, bus, arcs)
    }

    fn ctx() -> Arc<ModuleContext> {
        Arc::new(ModuleContext {
            app_data_dir: std::env::temp_dir(),
            ports: Arc::new(Ports::new()),
        })
    }

    #[tokio::test]
    async fn full_lifecycle_transitions() {
        let (reg, _bus, mods) = registry_with(vec![FakeModule::new(false)]);
        let results = reg.init_all(ctx()).await;
        assert!(results.iter().all(|(_, r)| r.is_ok()));
        assert_eq!(reg.status_all()[0].1, ModuleState::Stopped);

        let results = reg.start_all().await;
        assert!(results.iter().all(|(_, r)| r.is_ok()));
        assert_eq!(reg.status_all()[0].1, ModuleState::Running);

        reg.stop_all().await;
        assert_eq!(reg.status_all()[0].1, ModuleState::Stopped);
        assert_eq!(mods[0].status(), ModuleState::Stopped);
    }

    #[tokio::test]
    async fn panic_is_isolated_and_reported() {
        let (reg, bus, _mods) = registry_with(vec![FakeModule::new(true)]);
        let mut crash_rx = bus.subscribe("host.module_crashed").unwrap();

        reg.init_all(ctx()).await;
        reg.start_all().await;

        // 模块 panic → 注册表状态 Error，宿主存活
        assert_eq!(reg.status_all()[0].1, ModuleState::Error);
        // 崩溃事件可订阅
        let ev = crash_rx.try_recv().expect("应收到 host.module_crashed");
        assert_eq!(ev.payload["module"], "fake");

        // 重启流程：Error 态可重新拉起（本假模块不再 panic）
        reg.restart("fake").await.unwrap();
        assert_eq!(reg.status_all()[0].1, ModuleState::Running);
    }

    #[tokio::test]
    async fn duplicate_register_rejected() {
        let (reg, _bus, _mods) = registry_with(vec![FakeModule::new(false)]);
        let err = reg.register(Arc::new(FakeModule::new(false)));
        assert!(err.is_err());
        assert_eq!(err.unwrap_err().code(), codes::host::HOST_REGISTRY_002);
    }

    #[tokio::test]
    async fn restart_unknown_module_fails() {
        let (reg, _bus, _mods) = registry_with(vec![]);
        let err = reg.restart("ghost").await;
        assert_eq!(err.unwrap_err().code(), codes::host::HOST_REGISTRY_001);
    }
}
