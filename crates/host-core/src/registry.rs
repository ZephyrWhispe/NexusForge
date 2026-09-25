//! 模块注册表与生命周期管理（docs/impl/01 S5）
//!
//! 核心保证：
//! - **panic 隔离**：模块 init/start/stop 均在 `spawn_blocking` 中执行，
//!   panic 被 join 错误捕获并转为 [`ModuleError::Panicked`]，宿主存活；
//! - **状态机**：`Uninitialized → Stopped → Running`；异常/panic → `Error`；可 restart；
//!   状态读写唯一来源是模块自身的 [`Module`](crate::module::Module)（内部
//!   `ModuleStateCell`），注册表不再另持 HashMap（D-16）；每次状态迁移发布
//!   `host.module_state`（含 init / stop，此前仅 start 发布）；
//! - **启动不互相阻断**：某模块 init 失败只记录结果，其余模块继续；
//! - **停止限时**：stop 超过 [`STOP_TIMEOUT`] 视为失败并标记 Error。

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;

use crate::codes;
use crate::config::ConfigStore;
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
            .iter()
            .filter_map(|id| self.get(id).map(|m| m.info()))
            .collect()
    }

    /// 注册模块（id 去重）。注册后状态为模块 cell 初值 Uninitialized。
    pub fn register(&self, module: Arc<dyn Module>) -> Result<(), AppError> {
        let id = module.info().id.to_owned();
        {
            let mut modules = self.modules.write();
            if modules.contains_key(&id) {
                return Err(AppError::module(
                    codes::host::HOST_REGISTRY_002,
                    format!("模块 {id} 重复注册"),
                    None,
                ));
            }
            modules.insert(id.clone(), module);
        }
        self.order.write().push(id);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Module>> {
        self.modules.read().get(id).cloned()
    }

    /// 按注册顺序初始化全部模块。单个失败不阻断其余模块。
    pub async fn init_all(
        &self,
        ctx: Arc<ModuleContext>,
    ) -> Vec<(String, Result<(), ModuleError>)> {
        *self.ctx.write() = Some(ctx.clone());
        let modules = self.ordered();
        let mut results = Vec::with_capacity(modules.len());
        for (id, module) in modules {
            let prev = module.status();
            let m = module.clone();
            let c = ctx.clone();
            let r = match tokio::task::spawn_blocking(move || m.init(c)).await {
                Ok(r) => r,
                Err(je) if je.is_panic() => Err(ModuleError::Panicked(
                    je.into_panic()
                        .downcast_ref::<String>()
                        .cloned()
                        .unwrap_or_else(|| "init panic".into()),
                )),
                Err(je) => Err(ModuleError::Init(je.to_string())),
            };
            self.apply_state(
                &module,
                &id,
                prev,
                if r.is_ok() {
                    ModuleState::Stopped
                } else {
                    ModuleState::Error
                },
            );
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
        let mut ids = self.order.read().clone();
        ids.reverse();
        for id in ids {
            if let Some(m) = self.get(&id) {
                let _ = self.stop_one(&id, m).await;
            }
        }
    }

    /// 缺陷① 收口（09 §8.1-①）：把 ConfigStore 里的持久值派发给各模块。
    /// 盘上缺文件 / 空对象一律不派发——否则一次 `{}` 就能把运行态打回模块默认值。
    pub async fn apply_configs(
        &self,
        config: &ConfigStore,
    ) -> Vec<(String, Result<(), ModuleError>)> {
        let mut results = Vec::new();
        for (id, module) in self.ordered() {
            let Some(values) = stored_config(config, &id) else {
                continue;
            };
            // PERF-08：apply_config 内含 SQLite/文件 IO（sync 的 DELETE 等），
            // 不得占用运行时工作线程（与 init/start/stop 同纪律）
            let r = match tokio::task::spawn_blocking(move || module.apply_config(values)).await {
                Ok(r) => r,
                Err(e) => Err(ModuleError::Config(format!("配置派发任务失败: {e}"))),
            };
            if let Err(e) = &r {
                self.report_failure(&id, e).await;
            }
            results.push((id, r));
        }
        results
    }

    /// 单模块派发（运行期 `host.config_changed` 消费端）；模块未注册 → None
    pub async fn apply_one(
        &self,
        id: &str,
        config: &ConfigStore,
    ) -> Option<Result<(), ModuleError>> {
        let module = self.get(id)?;
        let values = stored_config(config, id)?;
        // PERF-08：同 apply_configs——派发是阻塞 IO，进 blocking 池
        Some(
            match tokio::task::spawn_blocking(move || module.apply_config(values)).await {
                Ok(r) => r,
                Err(e) => Err(ModuleError::Config(format!("配置派发任务失败: {e}"))),
            },
        )
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
        let ctx = self.ctx.read().clone().ok_or_else(|| {
            AppError::module(
                codes::host::HOST_CONFIG_001,
                "宿主尚未初始化，无法重启模块",
                None,
            )
        })?;
        // restart 中 stop 失败不阻断：继续 init/start 重建该模块
        let _ = self.stop_one(id, module.clone()).await;
        self.init_one(id, module.clone(), ctx).await?;
        self.start_one(id, module).await.map_err(AppError::from)?;
        Ok(())
    }

    /// 全部模块状态（按注册顺序；直接读模块 cell，与 Module::status() 同源）
    pub fn status_all(&self) -> Vec<(String, ModuleState)> {
        let modules = self.modules.read();
        self.order
            .read()
            .iter()
            .filter_map(|id| modules.get(id).map(|m| (id.clone(), m.status())))
            .collect()
    }

    // ------------------------------------------------------------------

    fn ordered(&self) -> Vec<(String, Arc<dyn Module>)> {
        self.order
            .read()
            .iter()
            .filter_map(|id| self.get(id).map(|m| (id.clone(), m)))
            .collect()
    }

    /// 唯一状态源的写入点：落到模块 cell，发生变化时发布 `host.module_state`
    fn apply_state(
        &self,
        module: &Arc<dyn Module>,
        id: &str,
        prev: ModuleState,
        next: ModuleState,
    ) {
        module.set_status(next);
        if next != prev {
            self.bus
                .publish(Event::new(
                    "host.module_state",
                    "host",
                    serde_json::json!({ "key": id, "module": id, "state": next }),
                ))
                .ok();
        }
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
        let prev = module.status();
        let m = module.clone();
        let r = match tokio::task::spawn_blocking(move || m.init(ctx)).await {
            Ok(r) => r,
            Err(je) if je.is_panic() => Err(ModuleError::Panicked(
                je.into_panic()
                    .downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_else(|| "init panic".into()),
            )),
            Err(je) => Err(ModuleError::Init(je.to_string())),
        };
        self.apply_state(
            &module,
            id,
            prev,
            if r.is_ok() {
                ModuleState::Stopped
            } else {
                ModuleState::Error
            },
        );
        if let Err(e) = &r {
            self.report_failure(id, e).await;
        }
        r
    }

    async fn start_one(&self, id: &str, module: Arc<dyn Module>) -> Result<(), ModuleError> {
        let prev = module.status();
        let m = module.clone();
        let r = match tokio::task::spawn_blocking(move || m.start()).await {
            Ok(r) => r,
            Err(je) if je.is_panic() => Err(ModuleError::Panicked(
                je.into_panic()
                    .downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_else(|| "start panic".into()),
            )),
            Err(je) => Err(ModuleError::Start(je.to_string())),
        };
        self.apply_state(
            &module,
            id,
            prev,
            if r.is_ok() {
                ModuleState::Running
            } else {
                ModuleState::Error
            },
        );
        if let Err(e) = &r {
            self.report_failure(id, e).await;
        }
        r
    }

    async fn stop_one(&self, id: &str, module: Arc<dyn Module>) -> Result<(), ModuleError> {
        let prev = module.status();
        let m = module.clone();
        // 三层嵌套：timeout(Elapsed) → JoinHandle(JoinError) → 模块返回值(Result<(), ModuleError>)
        let r = match timeout(STOP_TIMEOUT, tokio::task::spawn_blocking(move || m.stop())).await {
            Err(_elapsed) => Err(ModuleError::Stop("stop 超时 5s，已强制返回".into())),
            Ok(Err(je)) if je.is_panic() => Err(ModuleError::Panicked(
                je.into_panic()
                    .downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_else(|| "stop panic".into()),
            )),
            Ok(Err(je)) => Err(ModuleError::Stop(je.to_string())),
            Ok(Ok(Err(me))) => Err(me),
            Ok(Ok(Ok(()))) => Ok(()),
        };
        self.apply_state(
            &module,
            id,
            prev,
            if r.is_ok() {
                ModuleState::Stopped
            } else {
                ModuleState::Error
            },
        );
        r
    }
}

/// 盘上是否存有可派发的模块配置（缺文件 / 读失败 / 非对象 / 空对象 → None）
fn stored_config(config: &ConfigStore, id: &str) -> Option<serde_json::Value> {
    config
        .get_module(id)
        .ok()
        .filter(|v| v.as_object().is_some_and(|o| !o.is_empty()))
}

/// 缺陷① 运行期半边：`host.config_changed` 消费循环（宿主负责 spawn）。
/// 事件只带模块名，值一律现读 ConfigStore（D-03 消费端纪律：收事件后拉最新状态）。
pub async fn run_config_feed(
    mut rx: tokio::sync::broadcast::Receiver<Event>,
    registry: Arc<ModuleRegistry>,
    config: Arc<ConfigStore>,
) {
    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        };
        let Some(module) = ev.payload.get("module").and_then(|m| m.as_str()) else {
            tracing::warn!("host.config_changed 载荷缺 module，丢弃");
            continue;
        };
        match registry.apply_one(module, &config).await {
            None => tracing::debug!(module, "模块未注册或盘上无可派发配置，跳过本次派发"),
            Some(Err(e)) => {
                tracing::error!(module, error = %e, "配置派发到运行期模块失败");
                // 值已经写进盘了（schema 校验在写侧、模块级校验在派发侧），只进日志
                // 等于"设置看起来生效了、其实没有"——把同一句真因原样交给 UI 面
                registry
                    .bus
                    .publish(Event::new(
                        "host.config_rejected",
                        "host",
                        serde_json::json!({ "module": module, "error": e.to_string() }),
                    ))
                    .ok();
            }
            Some(Ok(())) => {
                // 派生式 schema 随运行态变（ocr 的引擎词表 enum 来自引擎注册表）：
                // 派发成功后重刷一次，否则"启用第二引擎"后设置中心仍拿旧词表，
                // 新引擎选不到——写侧校验与设置中心必须读同一份 schema。
                if let Some(m) = registry.get(module) {
                    config.register_schema(module, m.config_schema());
                }
                tracing::info!(module, "配置已派发到运行期模块");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::ModuleStateCell;
    use crate::ports::Ports;
    use std::sync::atomic::{AtomicU8, Ordering};

    struct FakeModule {
        state: ModuleStateCell,
        panic_on_start: bool,
        panicked_once: AtomicU8,
    }
    impl FakeModule {
        fn new(panic_on_start: bool) -> Self {
            Self {
                state: ModuleStateCell::new(),
                panic_on_start,
                panicked_once: AtomicU8::new(0),
            }
        }
    }
    impl Module for FakeModule {
        fn info(&self) -> crate::module::ModuleInfo {
            crate::module::ModuleInfo {
                id: "fake",
                name: "假模块",
                version: "0.1.0",
                icon: None,
                priority: crate::module::priority_of("fake"),
            }
        }
        fn init(&self, _ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
            self.state.set(ModuleState::Stopped);
            Ok(())
        }
        fn start(&self) -> Result<(), ModuleError> {
            // panic_on_start=true 时仅首次 start panic（验证重启可恢复）
            if self.panic_on_start && self.panicked_once.swap(1, Ordering::SeqCst) == 0 {
                panic!("故意 panic：验证隔离");
            }
            self.state.set(ModuleState::Running);
            Ok(())
        }
        fn stop(&self) -> Result<(), ModuleError> {
            self.state.set(ModuleState::Stopped);
            Ok(())
        }
        fn status(&self) -> ModuleState {
            self.state.get()
        }
        fn set_status(&self, state: ModuleState) {
            self.state.set(state);
        }
    }

    fn registry_with(
        modules: Vec<FakeModule>,
    ) -> (Arc<ModuleRegistry>, Arc<EventBus>, Vec<Arc<FakeModule>>) {
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
            event_bus: Arc::new(EventBus::new()),
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

    // D-16 回归①：单一状态源——注册表视图与模块自身视图必须恒等，
    // 旧实现注册表另持 HashMap，panic 后模块自称 Running 而宿主记 Error，两套真相
    #[tokio::test]
    async fn registry_and_module_share_single_state_source() {
        let (reg, _bus, mods) = registry_with(vec![FakeModule::new(true)]);
        reg.init_all(ctx()).await;
        assert_eq!(reg.status_all()[0].1, mods[0].status());
        reg.start_all().await; // 首次 start panic
        assert_eq!(reg.status_all()[0].1, ModuleState::Error);
        assert_eq!(
            mods[0].status(),
            ModuleState::Error,
            "panic 补记的 Error 必须落在模块 cell（唯一状态源）"
        );
        reg.stop_all().await;
        assert_eq!(reg.status_all()[0].1, mods[0].status());
    }

    // D-16 回归②：init / stop 迁移同样发布 host.module_state（旧实现仅 start 发布，
    // 前端状态流在"init 成功待 start"与"已停止"两个区间失真）
    #[tokio::test]
    async fn module_state_events_cover_init_and_stop() {
        let (reg, bus, _mods) = registry_with(vec![FakeModule::new(false)]);
        let mut rx = bus.subscribe("host.module_state").unwrap();
        reg.init_all(ctx()).await;
        let ev = rx.recv().await.expect("init 成功应发布 Stopped");
        assert_eq!(ev.payload["module"], "fake");
        assert_eq!(ev.payload["state"], "Stopped");
        reg.start_all().await;
        assert_eq!(
            rx.recv().await.expect("start 应发布 Running").payload["state"],
            "Running"
        );
        reg.stop_all().await;
        assert_eq!(
            rx.recv().await.expect("stop 应发布 Stopped").payload["state"],
            "Stopped"
        );
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
