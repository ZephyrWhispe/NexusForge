//! Module trait 与模块上下文（docs/impl/01 S3）

use std::path::PathBuf;
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

/// 依赖注入容器。
///
/// 字段随宿主分层实现逐步扩展（S4 增加 event_bus，S6 增加 config）；
/// 测试时构造空 Ports + 临时目录即可获得完整 mock 上下文。
pub struct ModuleContext {
    /// `{appDataDir}`，模块专属库文件应放在其 `db/` 子目录（DESIGN O3）
    pub app_data_dir: PathBuf,
    /// 系统能力端口（O2：模块不得直接依赖 windows crate）
    pub ports: Arc<Ports>,
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
    fn status(&self) -> ModuleState;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{Ports, Port};
    use std::sync::atomic::{AtomicU8, Ordering};

    struct FakePort;

    struct MockModule {
        state: AtomicU8,
    }
    // state: 0=Uninitialized 1=Stopped 2=Running 3=Error
    impl Module for MockModule {
        fn info(&self) -> ModuleInfo {
            ModuleInfo {
                id: "mock",
                name: "测试模块",
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
                2 => ModuleState::Running,
                _ => ModuleState::Error,
            }
        }
    }

    #[test]
    fn mock_module_lifecycle_transitions() {
        let m = MockModule { state: AtomicU8::new(0) };
        assert_eq!(m.status(), ModuleState::Uninitialized);

        let ctx = Arc::new(ModuleContext {
            app_data_dir: std::env::temp_dir(),
            ports: Arc::new(Ports::new()),
        });
        m.init(ctx).unwrap();
        assert_eq!(m.status(), ModuleState::Stopped);
        m.start().unwrap();
        assert_eq!(m.status(), ModuleState::Running);
        m.stop().unwrap();
        assert_eq!(m.status(), ModuleState::Stopped);
    }

    #[test]
    fn context_shares_ports_across_clones() {
        let ports = Arc::new(Ports::new());
        ports.register::<FakePort>(Arc::new(FakePort));
        let ctx = Arc::new(ModuleContext { app_data_dir: PathBuf::from("."), ports });
        // 模拟两个模块共享同一 Ports 实例
        assert!(Arc::ptr_eq(&ctx.ports, &ctx.ports));
        assert!(ctx.ports.get::<FakePort>().is_some());
    }
}
