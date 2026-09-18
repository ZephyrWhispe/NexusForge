//! automation-core：自动化与拓展（docs/impl/07 A1–A6）。
//!
//! - A1 规则模型：Rule { on: Trigger, when: Expr, then: Vec<Action>, cooldown, enabled }
//! - A2 受限表达式：点路径取值 + 比较/逻辑（禁止任意代码）
//! - A3 执行器：规则内串行、失败 retry(2, 指数) → 死信（可查/重放）；冷却表 + 防自环风暴防护
//! - A4 Task Scheduler：Schedule 规则同步注册 Windows 计划任务 → --run-rule 独立执行（standalone）
//! - A5 WASM 插件运行时：wasmtime 沙箱（64MB 限额 + fuel + 宿主函数白名单）
//! - A6 插件管理器：manifest 校验（api_version/权限/sha256）+ 本地安装/删除

pub mod engine;
pub mod error;
pub mod module;
pub mod plugins;
pub mod rule;
pub mod standalone;
pub mod wasm;

pub use engine::{DeadLetter, RuleEngine};
pub use error::{AutomationError, Result};
pub use module::{AutomationModule, HostActionHandler};
pub use plugins::{PluginInfo, PluginManifest, PluginStore, PLUGIN_API_VERSION};
pub use rule::{Action, CmpOp, Expr, Rule, Trigger};
pub use wasm::{WasmCaps, WasmHost, WasmRuntime, WASM_FUEL, WASM_MEM_LIMIT};
