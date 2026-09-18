//! Module trait 与模块上下文（docs/impl/01 S3）

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use serde::Serialize;

use crate::error::ModuleError;
use crate::ports::Ports;

/// 模块元信息（注册表 / 侧边栏 / 托盘聚合使用）
#[derive(Clone, Debug, Serialize)]
pub struct ModuleInfo {
    /// 全局唯一标识，如 "clipboard"（错误码前缀为其大写形式）
    pub id: &'static str,
    /// 中文显示名
    pub name: &'static str,
    pub version: &'static str,
    pub icon: Option<&'static str>,
    /// 冲突仲裁优先级（快捷键 / 托盘），小者优先（docs/impl/01 S6.2）
    pub priority: u8,
}

/// 模块状态机：`Uninitialized --init--> Stopped --start--> Running`；
/// 任意态异常 / panic → `Error`；`Error --restart--> Running`（docs/impl/01 S5）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum ModuleState {
    Uninitialized,
    Stopped,
    Running,
    Error,
}

impl ModuleState {
    /// 统一 AtomicU8 编码（D-16：原 13 份模块内复制的魔数即此约定）
    fn code(self) -> u8 {
        match self {
            ModuleState::Uninitialized => 0,
            ModuleState::Stopped => 1,
            ModuleState::Running => 2,
            ModuleState::Error => 3,
        }
    }

    /// 未知编码 fail-safe 归为 Error（原 12/13 模块的 `_ => Running` 会把内存踩踏伪装成"运行中"）
    fn from_code(v: u8) -> Self {
        match v {
            0 => ModuleState::Uninitialized,
            1 => ModuleState::Stopped,
            2 => ModuleState::Running,
            _ => ModuleState::Error,
        }
    }
}

/// 模块状态统一存储单元（D-16）：模块字段与注册表读写**同一原子格**，
/// 消除"模块内 AtomicU8"与"注册表 HashMap"两套状态源。
#[derive(Debug, Default)]
pub struct ModuleStateCell(AtomicU8);

impl ModuleStateCell {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get(&self) -> ModuleState {
        ModuleState::from_code(self.0.load(Ordering::SeqCst))
    }
    pub fn set(&self, state: ModuleState) {
        self.0.store(state.code(), Ordering::SeqCst);
    }
}

/// 未登记 id 的默认仲裁优先级
pub const PRIORITY_DEFAULT: u8 = 50;

/// 模块快捷键/托盘仲裁优先级集中表（D-16，DESIGN §8.3：小者优先）。
/// 散落各 crate 的 `priority:` 字面量改由本表推导，保证仲裁可审计；
/// 表序即阶段规划 docs/impl/07 的模块分组顺序。
pub fn priority_of(id: &str) -> u8 {
    match id {
        // 高频呼出类（全局快捷键主战场）
        "clipboard" | "ocr" | "screenshot" => 10,
        "automation" => 12,
        // 系统级
        "sync" | "sys" => 13,
        "term" => 14,
        "notes" | "proxy" => 15,
        "desktop" => 16,
        // 窗口内操作为主、全局冲突概率低
        "editor" | "kvm" | "vault" => 20,
        "file" => 25,
        _ => PRIORITY_DEFAULT,
    }
}

/// 依赖注入容器。
///
/// 测试时构造 Ports + 临时目录即可获得完整 mock 上下文。
pub struct ModuleContext {
    /// `{appDataDir}`，模块专属库文件应放在其 `db/` 子目录（DESIGN O3）
    pub app_data_dir: PathBuf,
    /// 系统能力端口（O2：模块不得直接依赖 windows crate）
    pub ports: Arc<Ports>,
    /// 事件总线（S4；跨模块通信唯一通道，DESIGN O1）
    pub event_bus: Arc<crate::events::EventBus>,
}

/// 模块统一接口 —— 13 个功能模块与未来 WASM 插件的宿主侧契约。
///
/// 全部方法 `&self`：模块内部可变状态一律用 `RwLock` / `Mutex`，
/// 以便注册表以 `Arc<dyn Module>` 持有（docs/impl/01 S5 潜在问题 3）。
pub trait Module: Send + Sync {
    fn info(&self) -> ModuleInfo;
    /// 初始化：建库 / 读配置 / 注册 IPC 命令；失败不阻断其它模块（S5）
    fn init(&self, ctx: Arc<ModuleContext>) -> Result<(), ModuleError>;
    fn start(&self) -> Result<(), ModuleError>;
    fn stop(&self) -> Result<(), ModuleError>;
    /// JSON Schema（设置中心自动渲染，docs/impl/01 S6.1）
    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    /// 应用配置：实现方必须先经 schema 校验语义合法性再落盘
    fn apply_config(&self, _values: serde_json::Value) -> Result<(), ModuleError> {
        Ok(())
    }
    /// 统一状态读取（D-16：唯一状态源 = 实现方持有的 [`ModuleStateCell`]）
    fn status(&self) -> ModuleState;
    /// 统一状态写入（注册表在 panic / stop 超时 / init 失败等模块自感知的
    /// 边界之外补记 Error 态；实现方直接代理到同一 [`ModuleStateCell`]）
    fn set_status(&self, state: ModuleState);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Ports;

    struct FakePort;

    struct MockModule {
        state: ModuleStateCell,
    }
    impl Module for MockModule {
        fn info(&self) -> ModuleInfo {
            ModuleInfo {
                id: "mock",
                name: "测试模块",
                version: "0.1.0",
                icon: None,
                priority: priority_of("mock"),
            }
        }
        fn init(&self, _ctx: Arc<ModuleContext>) -> Result<(), ModuleError> {
            self.state.set(ModuleState::Stopped);
            Ok(())
        }
        fn start(&self) -> Result<(), ModuleError> {
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

    #[test]
    fn mock_module_lifecycle_transitions() {
        let m = MockModule {
            state: ModuleStateCell::new(),
        };
        assert_eq!(m.status(), ModuleState::Uninitialized);

        let ctx = Arc::new(ModuleContext {
            app_data_dir: std::env::temp_dir(),
            ports: Arc::new(Ports::new()),
            event_bus: Arc::new(crate::events::EventBus::new()),
        });
        m.init(ctx).unwrap();
        assert_eq!(m.status(), ModuleState::Stopped);
        m.start().unwrap();
        assert_eq!(m.status(), ModuleState::Running);
        m.stop().unwrap();
        assert_eq!(m.status(), ModuleState::Stopped);
    }

    #[test]
    fn state_cell_roundtrip_and_unknown_failsafe() {
        let cell = ModuleStateCell::new();
        assert_eq!(cell.get(), ModuleState::Uninitialized);
        for s in [
            ModuleState::Stopped,
            ModuleState::Running,
            ModuleState::Error,
            ModuleState::Uninitialized,
        ] {
            cell.set(s);
            assert_eq!(cell.get(), s, "四态读写必须无损往返");
        }
        // 未知魔数（模拟内存踩踏 / 旧代码写 3 之外的值）不得伪装成 Running
        cell.0.store(9, Ordering::SeqCst);
        assert_eq!(cell.get(), ModuleState::Error);
    }

    #[test]
    fn priority_table_covers_all_shipped_modules() {
        // D-16：14 个出厂模块必须逐名登记（新增模块忘记入表会静默吃默认值）
        for id in [
            "clipboard",
            "ocr",
            "screenshot",
            "automation",
            "sync",
            "sys",
            "term",
            "notes",
            "proxy",
            "desktop",
            "editor",
            "kvm",
            "vault",
            "file",
        ] {
            assert_ne!(
                priority_of(id),
                PRIORITY_DEFAULT,
                "模块 {id} 未登记优先级表"
            );
        }
        assert_eq!(priority_of("ghost"), PRIORITY_DEFAULT);
        // 仲裁语义复核：小者优先，高频呼出类必须优于系统级与文档类
        assert!(priority_of("clipboard") < priority_of("file"));
        assert!(priority_of("screenshot") < priority_of("desktop"));
    }

    #[test]
    fn context_shares_ports_across_clones() {
        let ports = Arc::new(Ports::new());
        ports.register::<FakePort>(Arc::new(FakePort));
        let ctx = Arc::new(ModuleContext {
            app_data_dir: PathBuf::from("."),
            ports,
            event_bus: Arc::new(crate::events::EventBus::new()),
        });
        // 模拟两个模块共享同一 Ports 实例
        assert!(Arc::ptr_eq(&ctx.ports, &ctx.ports));
        assert!(ctx.ports.get::<FakePort>().is_some());
    }
}
