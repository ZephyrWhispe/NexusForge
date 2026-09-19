//! V7 模块壳（docs/impl/05 V7）：Module trait 实现。
//!
//! - init：打开 [`VaultService`]（meta/db 在 appData 下；库未创建 = Uninitialized），
//!   绑定端口（V4 Hello/Crypto 经 ctx.ports，D-24）
//! - start：挂 V5 自动锁定看门狗（std 线程 5s tick；空闲/失焦双线，锁定前 30s 预警）
//! - stop：停看门狗并立即锁定（DEK wipe），安全语义优先

use parking_lot::{Condvar, Mutex as PgMutex, RwLock};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use host_core::config::ConfigStore;
use host_core::error::ModuleError;
use host_core::events::Event;
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};

use crate::autolock::{AutolockAction, AutolockPolicy};
use crate::vault::VaultService;

/// 看门狗轮询间隔
const TICK: Duration = Duration::from_secs(5);
/// 配置缺省兜底（与 config_schema default 一致）
const DEFAULT_IDLE_MINS: u64 = 15;
const DEFAULT_BLUR_MINS: u64 = 5;

/// 从 vault 配置段读自动锁策略（分钟→秒；缺省用 schema 默认值）
fn policy_from(config: Option<&ConfigStore>) -> AutolockPolicy {
    let cfg = config.and_then(|c| c.get_module("vault").ok());
    let mins = |key: &str, default: u64| -> u64 {
        cfg.as_ref()
            .and_then(|v| v.get(key))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(default)
    };
    AutolockPolicy {
        idle_secs: mins("auto_lock_idle_mins", DEFAULT_IDLE_MINS) * 60,
        blur_secs: mins("auto_lock_blur_mins", DEFAULT_BLUR_MINS) * 60,
    }
}

/// 单轮评估：锁定 / 30s 预警（warned 去重每锁定周期一次）/ 活动重置清零
fn run_tick(svc: &VaultService, ctx: &ModuleContext, warned: &AtomicBool, policy: &AutolockPolicy) {
    match svc.autolock_action(policy) {
        Some(AutolockAction::Lock) => {
            tracing::info!("V5 自动锁定触发（空闲/失焦超限）");
            if svc.lock().is_ok() {
                warned.store(false, Ordering::SeqCst);
                let _ = ctx.event_bus.publish(Event::new(
                    "vault.state_changed",
                    "vault",
                    serde_json::json!({ "state": "locked", "reason": "auto_lock" }),
                ));
            }
        }
        Some(AutolockAction::Warn { lock_in_secs }) => {
            if !warned.swap(true, Ordering::SeqCst) {
                let _ = ctx.event_bus.publish(Event::new(
                    "vault.auto_lock_warning",
                    "vault",
                    serde_json::json!({ "lock_in_secs": lock_in_secs }),
                ));
            }
        }
        None => warned.store(false, Ordering::SeqCst),
    }
}

pub struct VaultModule {
    service: RwLock<Option<Arc<VaultService>>>,
    state: ModuleStateCell,
    /// V5 策略来源（None = 独立测试构造，不挂看门狗）
    config: Option<Arc<ConfigStore>>,
    ctx: RwLock<Option<Arc<ModuleContext>>>,
    watchdog_stop: Arc<AtomicBool>,
    /// tick 休眠可被 stop 立即唤醒
    watchdog_sleeping: Arc<PgMutex<bool>>,
    watchdog_wake: Arc<Condvar>,
    /// 每个锁定周期只预警一次（None-action 时清零）
    warned: Arc<AtomicBool>,
    watchdog: RwLock<Option<JoinHandle<()>>>,
}

impl VaultModule {
    pub fn new() -> Self {
        Self::with_config(None)
    }

    /// V5 看门狗需要配置注入（ModuleContext 无 config 句柄，D-24 决策②偏差记录）
    pub fn new_with_config(config: Arc<ConfigStore>) -> Self {
        Self::with_config(Some(config))
    }

    fn with_config(config: Option<Arc<ConfigStore>>) -> Self {
        Self {
            service: RwLock::new(None),
            state: ModuleStateCell::new(),
            config,
            ctx: RwLock::new(None),
            watchdog_stop: Arc::new(AtomicBool::new(false)),
            watchdog_sleeping: Arc::new(PgMutex::new(false)),
            watchdog_wake: Arc::new(Condvar::new()),
            warned: Arc::new(AtomicBool::new(false)),
            watchdog: RwLock::new(None),
        }
    }

    /// IPC 层入口（全部命令经此取服务；未 init 返回 None）
    pub fn service(&self) -> Option<Arc<VaultService>> {
        self.service.read().clone()
    }

    /// 当前生效的自动锁定策略（config 缺省用 schema 默认值；分钟→秒）
    pub fn autolock_policy(&self) -> AutolockPolicy {
        policy_from(self.config.as_deref())
    }

    /// 看门狗单轮评估（公开给回归测试直接驱动，免依赖真实时钟）
    pub fn watchdog_tick(&self) {
        let (Some(svc), Some(ctx)) = (self.service(), self.ctx.read().clone()) else {
            return;
        };
        if self.config.is_none() {
            return;
        }
        run_tick(&svc, &ctx, &self.warned, &self.autolock_policy());
    }

    fn spawn_watchdog(&self, svc: Arc<VaultService>, ctx: Arc<ModuleContext>) {
        let stop = self.watchdog_stop.clone();
        let wake = self.watchdog_wake.clone();
        let sleeping = self.watchdog_sleeping.clone();
        let warned = self.warned.clone();
        let config = self.config.clone().expect("看门狗要求已注入配置");
        let handle = std::thread::Builder::new()
            .name("vault-autolock".into())
            .spawn(move || loop {
                {
                    let mut guard = sleeping.lock();
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    *guard = true;
                    let timed_out = wake.wait_for(&mut guard, TICK).timed_out();
                    *guard = false;
                    if !timed_out {
                        break; // 被 stop 唤醒
                    }
                }
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                run_tick(&svc, &ctx, &warned, &policy_from(Some(config.as_ref())));
            });
        match handle {
            Ok(h) => *self.watchdog.write() = Some(h),
            Err(e) => tracing::warn!("自动锁定看门狗启动失败: {e}"),
        }
    }

    fn stop_watchdog(&self) {
        self.watchdog_stop.store(true, Ordering::Relaxed);
        self.watchdog_wake.notify_all();
        if let Some(h) = self.watchdog.write().take() {
            let _ = h.join();
        }
    }
}

impl Default for VaultModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for VaultModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "vault",
            name: "安全与凭据",
            version: "0.1.0",
            icon: Some("vault"),
            priority: priority_of("vault"),
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let svc = VaultService::open(&ctx.app_data_dir)
            .map_err(|e| ModuleError::Storage(e.to_string()))?;
        // D-24：Hello（校验门）与 Crypto（DPAPI 包裹）端口经宿主注册表注入
        svc.bind_ports(ctx.ports.clone());
        *self.service.write() = Some(Arc::new(svc));
        *self.ctx.write() = Some(ctx);
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        let (Some(svc), Some(ctx)) = (self.service(), self.ctx.read().clone()) else {
            return Err(ModuleError::NotReady);
        };
        self.watchdog_stop.store(false, Ordering::Relaxed);
        self.warned.store(false, Ordering::SeqCst);
        if self.config.is_some() && self.watchdog.read().is_none() {
            self.spawn_watchdog(svc, ctx);
        }
        self.state.set(ModuleState::Running);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        self.stop_watchdog();
        // 停用即锁定：内存 DEK 立即 wipe（SecretKey Drop 兜底）
        if let Some(svc) = self.service() {
            let _ = svc.lock();
        }
        self.state.set(ModuleState::Stopped);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "clear_clipboard_secs": {
                    "type": "integer", "title": "复制密码后自动清除剪贴板",
                    "description": "0 = 不清除（docs/impl/05 V 风险项：默认 90s）",
                    "minimum": 0, "maximum": 600, "default": 90
                },
                "auto_lock_idle_mins": {
                    "type": "integer", "title": "空闲自动锁定（分钟）",
                    "description": "0 = 禁用（D-24 V5 已交付；锁定前 30s 预警事件）",
                    "minimum": 0, "maximum": 120, "default": 15
                },
                "auto_lock_blur_mins": {
                    "type": "integer", "title": "失焦自动锁定（分钟）",
                    "description": "0 = 禁用；密码库窗口失去焦点后计时（D-24 V5）",
                    "minimum": 0, "maximum": 120, "default": 5
                }
            }
        })
    }

    fn status(&self) -> ModuleState {
        self.state.get()
    }

    fn set_status(&self, state: ModuleState) {
        self.state.set(state);
    }
}
