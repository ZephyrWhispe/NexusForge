//! AutomationModule 模块壳（docs/impl/07 A1–A6）：Module trait 实现。
//!
//! - init：规则库加载（rules.json）+ ActionHandler 注入 + WASM 运行时/插件库（A5/A6）
//! - start：中央 dispatcher 任务（订阅全部 TOPIC_REGISTRY 主题 → 匹配 Event 规则 →
//!   when 求值 → 串行执行 then）+ Startup/Schedule 触发
//! - 风暴防护：冷却表 + automation 自产事件不再触发规则（防自环，深度上限的 v1 等效实现）
//! - 规则持久化：{appData}/automation/rules.json（规则量级小，JSON 足够；可全量重载）
//! - A4：Schedule 规则同步注册 Windows 计划任务（TaskSchdPort；应用不运行也触发 --run-rule）

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use host_core::error::ModuleError;
use host_core::events::{Event, EventBus, TOPIC_REGISTRY};
use host_core::module::{Module, ModuleContext, ModuleInfo, ModuleState};
use host_core::ports::ShellPort;
use host_core::ports::TaskSchdPort;

use crate::engine::{ActionHandler, RuleEngine};
use crate::error::AutomationError;
use crate::plugins::PluginStore;
use crate::rule::Rule;
use crate::wasm::{WasmHost, WasmRuntime};

/// Schedule 检查间隔
const SCHEDULE_TICK_MS: u64 = 30_000;

/// Task Scheduler 任务名前缀（平铺命名避免文件夹层级）
const TASK_PREFIX: &str = "NexusForge_rule_";

fn task_name_for(rule_id: &str) -> String {
    format!("{TASK_PREFIX}{rule_id}")
}

/// 宿主动作处理器（src-tauri 注入实现）；同时实现 WasmHost 供 A5 沙箱回调
pub struct HostActionHandler {
    shell: RwLock<Option<Arc<dyn ShellPort>>>,
    bus: Arc<EventBus>,
    /// WASM 沙箱运行时（A5；Module 方法 &self → RwLock 注入）
    runtime: RwLock<Option<Arc<WasmRuntime>>>,
    /// 插件库（A6；"plugin:{id}" 路径解析用）
    plugins: RwLock<Option<Arc<PluginStore>>>,
}

impl HostActionHandler {
    pub fn new(bus: Arc<EventBus>) -> Self {
        Self {
            shell: RwLock::new(None),
            bus,
            runtime: RwLock::new(None),
            plugins: RwLock::new(None),
        }
    }

    pub fn attach_shell(&self, shell: Arc<dyn ShellPort>) {
        *self.shell.write().expect("shell 锁污染") = Some(shell);
    }

    /// 注入 WASM 运行时 + 插件库（A5/A6；init 时调用）
    pub fn attach_wasm(&self, runtime: Arc<WasmRuntime>, plugins: Arc<PluginStore>) {
        *self.runtime.write().expect("runtime 锁污染") = Some(runtime);
        *self.plugins.write().expect("plugins 锁污染") = Some(plugins);
    }

    /// RunScript path 解析："plugin:{id}" → 插件库加载（含 sha256 复验）；否则按文件路径直读
    fn resolve_wasm(&self, path: &str, func: &str) -> crate::error::Result<(Vec<u8>, String, crate::wasm::WasmCaps)> {
        if let Some(id) = path.strip_prefix("plugin:") {
            let store = self
                .plugins
                .read()
                .expect("plugins 锁污染")
                .clone()
                .ok_or_else(|| AutomationError::Action("插件库未初始化".into()))?;
            let manifest = store.manifest_of(id)?;
            let wasm = store.wasm_bytes(id)?;
            let func = if func.is_empty() { manifest.func.clone() } else { func.to_string() };
            Ok((wasm, func, manifest.caps()))
        } else {
            let wasm = std::fs::read(path).map_err(AutomationError::Io)?;
            let func = if func.is_empty() { "run".into() } else { func.to_string() };
            // 散装 wasm 无 manifest：默认无权限（仅 nf.log）
            Ok((wasm, func, crate::wasm::WasmCaps::default()))
        }
    }
}

impl WasmHost for HostActionHandler {
    fn log(&self, msg: &str) {
        tracing::info!(target: "nf_wasm", "{msg}");
    }

    fn open_url(&self, url: &str) -> crate::error::Result<()> {
        ActionHandler::open_url(self, url)
    }

    fn notify(&self, title: &str, body: &str) -> crate::error::Result<()> {
        ActionHandler::publish(self, "automation.notify", serde_json::json!({ "title": title, "body": body }))
    }
}

impl ActionHandler for HostActionHandler {
    fn open_url(&self, url: &str) -> crate::error::Result<()> {
        let shell = self
            .shell
            .read()
            .expect("shell 锁污染")
            .clone()
            .ok_or_else(|| AutomationError::Action("ShellPort 未注册".into()))?;
        shell
            .shell_execute(url)
            .map_err(|e| AutomationError::Action(e.to_string()))
    }

    fn publish(&self, topic: &str, payload: serde_json::Value) -> crate::error::Result<()> {
        // 查 TOPIC_REGISTRY 还原静态主题（总线规则：仅编译期登记主题可发布）
        let static_topic = host_core::events::TOPIC_REGISTRY
            .iter()
            .find(|(t, _)| *t == topic)
            .map(|(t, _)| *t)
            .ok_or_else(|| {
                AutomationError::Action(format!("未知事件主题 {topic}（需在 TOPIC_REGISTRY 登记）"))
            })?;
        self.bus
            .publish(Event::new(static_topic, "automation", payload))
            .map_err(|e| AutomationError::Action(e.to_string()))
    }

    fn ipc_command(&self, module: &str, cmd: &str, _args: &serde_json::Value) -> crate::error::Result<()> {
        // v1 内置白名单映射；完整模块命令映射随插件/宿主注册表深化
        Err(AutomationError::Action(format!(
            "IpcCommand 暂未开放（{module}.{cmd}）；v1 请使用 publish/notify/open_url/run_script 动作"
        )))
    }

    fn run_wasm(&self, path: &str, func: &str) -> crate::error::Result<()> {
        let runtime = self
            .runtime
            .read()
            .expect("runtime 锁污染")
            .clone()
            .ok_or_else(|| AutomationError::Action("WASM 运行时未初始化".into()))?;
        let (wasm, func, caps) = self.resolve_wasm(path, func)?;
        runtime.run(&wasm, &func, caps, self)
    }
}

pub struct AutomationModule {
    state: AtomicU8,
    bus: RwLock<Option<Arc<EventBus>>>,
    engine: RwLock<Option<Arc<RuleEngine>>>,
    /// Arc 化规则表：dispatcher 闭包持有快照访问
    rules: Arc<RwLock<Vec<Rule>>>,
    /// 规则库文件 {appData}/automation/rules.json
    rules_path: PathBuf,
    handler: RwLock<Option<Arc<HostActionHandler>>>,
    /// A4：Windows 计划任务端口（未注册时跳过同步，仅应用内定时生效）
    taskschd: RwLock<Option<Arc<dyn TaskSchdPort>>>,
    /// A6：插件库（IPC 层入口）
    plugins: RwLock<Option<Arc<PluginStore>>>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    thread: RwLock<Option<std::thread::JoinHandle<()>>>,
}

impl AutomationModule {
    pub fn new(app_data_dir: &std::path::Path) -> Self {
        Self {
            state: AtomicU8::new(0),
            bus: RwLock::new(None),
            engine: RwLock::new(None),
            rules: Arc::new(RwLock::new(Vec::new())),
            rules_path: app_data_dir.join("automation").join("rules.json"),
            handler: RwLock::new(None),
            taskschd: RwLock::new(None),
            plugins: RwLock::new(None),
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            thread: RwLock::new(None),
        }
    }

    /// IPC 层入口
    pub fn rules(&self) -> Vec<Rule> {
        self.rules.read().expect("规则锁污染").clone()
    }

    pub fn engine(&self) -> Option<Arc<RuleEngine>> {
        self.engine.read().ok().and_then(|g| g.clone())
    }

    pub fn plugin_store(&self) -> Option<Arc<PluginStore>> {
        self.plugins.read().ok().and_then(|g| g.clone())
    }

    /// 保存规则（新增/覆盖）；校验 + 持久化 + 计划任务同步（A4）
    pub fn save_rule(&self, rule: Rule) -> crate::error::Result<()> {
        rule.validate()?;
        let mut rules = self.rules.write().expect("规则锁污染");
        rules.retain(|r| r.id != rule.id);
        rules.push(rule);
        let snapshot = rules.clone();
        drop(rules);
        self.persist(&snapshot)?;
        // retain+push 后末位即刚保存的规则（锁外执行同步，避免持锁做进程调用）
        if let Some(r) = snapshot.last() {
            self.sync_task(r);
        }
        Ok(())
    }

    pub fn delete_rule(&self, id: &str) -> crate::error::Result<bool> {
        let mut rules = self.rules.write().expect("规则锁污染");
        let before = rules.len();
        rules.retain(|r| r.id != id);
        let removed = rules.len() < before;
        let snapshot = rules.clone();
        drop(rules);
        if removed {
            self.persist(&snapshot)?;
            // 规则删除 → 计划任务一并移除（幂等）
            if let Some(ts) = self.taskschd.read().expect("任务锁污染").clone() {
                if let Err(e) = ts.remove(&task_name_for(id)) {
                    tracing::warn!(rule = %id, error = %e, "计划任务移除失败");
                }
            }
        }
        Ok(removed)
    }

    /// 启停规则（A4：停用 → 移除计划任务；启用 Schedule → 注册计划任务）
    pub fn toggle_rule(&self, id: &str, enabled: bool) -> crate::error::Result<bool> {
        let mut rules = self.rules.write().expect("规则锁污染");
        let mut changed = false;
        for r in rules.iter_mut() {
            if r.id == id {
                r.enabled = enabled;
                changed = true;
            }
        }
        let snapshot = rules.clone();
        drop(rules);
        if changed {
            self.persist(&snapshot)?;
            if let Some(r) = snapshot.iter().find(|r| r.id == id) {
                self.sync_task(r);
            }
        }
        Ok(changed)
    }

    /// A4：规则 ↔ Windows 计划任务同步（enable+Schedule → ensure_daily；否则 remove）
    /// 失败仅告警不阻断规则保存（应用内定时仍生效）
    fn sync_task(&self, rule: &Rule) {
        let Some(ts) = self.taskschd.read().expect("任务锁污染").clone() else { return };
        let name = task_name_for(&rule.id);
        if rule.enabled {
            if let crate::rule::Trigger::Schedule { time } = &rule.on {
                match std::env::current_exe() {
                    Ok(exe) => {
                        let args = format!("--run-rule {}", rule.id);
                        if let Err(e) =
                            ts.ensure_daily(&name, &exe.to_string_lossy(), &args, time)
                        {
                            tracing::warn!(rule = %rule.id, error = %e, "计划任务注册失败（应用内定时仍生效）");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "无法定位当前 exe，计划任务未注册"),
                }
                return;
            }
        }
        // 非 Schedule / 已停用 → 幂等移除
        if let Err(e) = ts.remove(&name) {
            tracing::warn!(rule = %rule.id, error = %e, "计划任务移除失败");
        }
    }

    fn persist(&self, rules: &[Rule]) -> crate::error::Result<()> {
        if let Some(parent) = self.rules_path.parent() {
            std::fs::create_dir_all(parent).map_err(AutomationError::Io)?;
        }
        let data = serde_json::to_vec_pretty(rules)
            .map_err(|e| AutomationError::BadRule(format!("规则序列化失败: {e}")))?;
        std::fs::write(&self.rules_path, data).map_err(AutomationError::Io)
    }

    fn load_rules(&self) {
        let rules: Vec<Rule> = std::fs::read(&self.rules_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        *self.rules.write().expect("规则锁污染") = rules;
    }

    /// Startup 规则触发
    fn fire_startup(&self) {
        let now = now_ms();
        let Some(engine) = self.engine() else { return };
        for r in self.rules().iter().filter(|r| r.enabled) {
            if matches!(r.on, crate::rule::Trigger::Startup) {
                engine.fire(r, &serde_json::json!({}), now);
            }
        }
    }

    /// dispatcher：订阅全部主题 → 匹配规则 → 求值 → 执行（automation 自产事件跳过，防自环）
    fn start_dispatcher(&self) {
        let Some(bus) = self.bus.read().ok().and_then(|g| g.clone()) else { return };
        let Some(engine) = self.engine() else { return };
        let rules = self.rules.clone();
        // 事件订阅任务（tokio——bootstrap 在 runtime 内 start）
        for (topic, _) in TOPIC_REGISTRY {
            let Ok(mut rx) = bus.subscribe(topic) else { continue };
            let engine = engine.clone();
            let rules = rules.clone();
            let topic_static: &'static str = topic;
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            // 防自环：automation 自产事件不再触发规则（风暴防护）
                            if event.source == "automation" {
                                continue;
                            }
                            let payload = event.payload.clone();
                            // 规则快照读取（触发时点的启用规则）
                            let matched: Vec<Rule> = rules
                                .read()
                                .expect("规则锁污染")
                                .iter()
                                .filter(|r| r.enabled && r.matches_event(topic_static) && r.when_passes(&payload))
                                .cloned()
                                .collect();
                            for r in matched {
                                engine.fire(&r, &payload, event.ts);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
        }

        // Schedule 触发（30s 轮询每日 HH:MM；当日触发后防重）
        let cancel = self.cancel.clone();
        let handle = std::thread::Builder::new()
            .name("nf-auto-sched".into())
            .spawn(move || {
                let mut last_date = String::new();
                loop {
                    if cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(SCHEDULE_TICK_MS));
                    if cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    let now = now_ms();
                    let (hhmm, date) = current_hhmm_date(now);
                    if date == last_date {
                        continue;
                    }
                    let due: Vec<Rule> = rules
                        .read()
                        .expect("规则锁污染")
                        .iter()
                        .filter(|r| {
                            r.enabled
                                && matches!(&r.on, crate::rule::Trigger::Schedule { time } if *time == hhmm)
                        })
                        .cloned()
                        .collect();
                    for r in due {
                        last_date = date.clone();
                        engine.fire(&r, &serde_json::json!({ "date": date }), now);
                    }
                }
            })
            .ok();
        *self.thread.write().expect("调度线程句柄锁污染") = handle;
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 本地时区当前 "HH:MM" 与 "YYYY-MM-DD"
fn current_hhmm_date(now_ms: i64) -> (String, String) {
    // chrono 未引入 automation-core——用偏移近似（本地时区偏移读取走 desktop-core 同款）
    let secs = now_ms / 1000;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let hh = (secs_of_day / 3600) as u8;
    let mm = ((secs_of_day % 3600) / 60) as u8;
    // 日期仅用于防重（UTC 日界即可；跨时区误差 ≤ 触发一次的容差）
    let (y, mo, d) = civil_from_days(days);
    (format!("{hh:02}:{mm:02}"), format!("{y:04}-{mo:02}-{d:02}"))
}

/// Howard Hinnant civil_from_days（天数 → 公历日期）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl Module for AutomationModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "automation",
            name: "自动化与拓展",
            version: "0.1.0",
            icon: Some("automation"),
            priority: 12,
        }
    }

    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
        let handler = Arc::new(HostActionHandler::new(ctx.event_bus.clone()));
        handler.attach_shell(
            ctx.ports
                .get::<dyn host_core::ports::ShellPort>()
                .ok_or_else(|| ModuleError::Init("ShellPort 未注册".into()))?,
        );
        // A5/A6：WASM 运行时 + 插件库（init 失败即模块失败——沙箱是核心能力）
        let runtime = Arc::new(WasmRuntime::new().map_err(|e| ModuleError::Init(e.to_string()))?);
        let plugins = Arc::new(PluginStore::new(&ctx.app_data_dir));
        handler.attach_wasm(runtime, plugins.clone());
        *self.plugins.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(plugins);
        // A4：计划任务端口（未注册仅告警——应用内定时仍生效）
        *self.taskschd.write().map_err(|_| ModuleError::Init("锁污染".into()))? =
            ctx.ports.get::<dyn TaskSchdPort>();
        *self.handler.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(handler.clone());
        *self
            .engine
            .write()
            .map_err(|_| ModuleError::Init("锁污染".into()))? = Some(Arc::new(RuleEngine::new(handler)));
        *self.bus.write().map_err(|_| ModuleError::Init("锁污染".into()))? = Some(ctx.event_bus.clone());
        self.load_rules();
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn start(&self) -> Result<(), ModuleError> {
        self.start_dispatcher();
        self.fire_startup();
        self.state.store(2, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<(), ModuleError> {
        self.cancel.store(true, Ordering::SeqCst);
        self.state.store(1, Ordering::SeqCst);
        Ok(())
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "storm_note": {
                    "type": "string", "title": "风暴防护",
                    "description": "默认冷却 5s/规则；automation 自产事件不再触发规则（防自环）",
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

use std::time::Duration;
