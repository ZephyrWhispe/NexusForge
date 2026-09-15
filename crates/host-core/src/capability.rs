//! 模块能力 trait（docs/impl/01 S3 能力接口 + S6.3 托盘聚合）
//!
//! 模块按需实现；注册表经 `Ports::register_multi` 收集多实例，
//! 宿主聚合后驱动托盘菜单与全局快捷键注册。

use crate::module::Module;
use crate::ports::Port;

/// 全局快捷键绑定描述
#[derive(Clone, Debug, serde::Serialize)]
pub struct HotkeyBinding {
    /// 模块内唯一 id，如 "clipboard.quick_panel"
    pub id: String,
    /// 显示标签（设置中心 / 冲突面板用）
    pub label: String,
    /// Win32 修饰键位掩码（MOD_CONTROL|MOD_SHIFT…，由 win-integration 解释）
    pub modifiers: u32,
    /// Win32 虚拟键码
    pub vk: u32,
}

/// 托盘菜单项
#[derive(Clone, Debug, serde::Serialize)]
pub struct TrayMenuItem {
    pub id: String,
    pub label: String,
    pub enabled: bool,
}

/// 提供托盘菜单的模块能力
pub trait TrayProvider: Module + Port {
    fn tray_menu_items(&self) -> Vec<TrayMenuItem>;
}

/// 注册全局快捷键的模块能力
pub trait HotkeyProvider: Module + Port {
    fn global_hotkeys(&self) -> Vec<HotkeyBinding>;
}

/// 托盘菜单聚合段（一个提供能力的模块一段）
#[derive(Clone, Debug, serde::Serialize)]
pub struct TraySection {
    pub module: String,
    pub priority: u8,
    pub items: Vec<TrayMenuItem>,
}

/// 聚合全部 TrayProvider（按模块 priority 升序，docs/impl/01 S6.3）
pub fn aggregate_tray(abilities: &crate::ports::Ports) -> Vec<TraySection> {
    let mut sections: Vec<TraySection> = abilities
        .get_all::<dyn TrayProvider>()
        .into_iter()
        .map(|p| {
            let info = p.info();
            TraySection {
                module: info.id.to_owned(),
                priority: info.priority,
                items: p.tray_menu_items(),
            }
        })
        .collect();
    sections.sort_by_key(|s| s.priority);
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{ModuleContext, ModuleState};
    use crate::ports::Ports;
    use std::sync::Arc;

    struct FakeProvider {
        id: &'static str,
        priority: u8,
    }
    impl Module for FakeProvider {
        fn info(&self) -> crate::module::ModuleInfo {
            crate::module::ModuleInfo {
                id: self.id,
                name: self.id,
                version: "0.1.0",
                icon: None,
                priority: self.priority,
            }
        }
        fn init(&self, _ctx: Arc<ModuleContext>) -> Result<(), crate::error::ModuleError> {
            Ok(())
        }
        fn start(&self) -> Result<(), crate::error::ModuleError> {
            Ok(())
        }
        fn stop(&self) -> Result<(), crate::error::ModuleError> {
            Ok(())
        }
        fn status(&self) -> ModuleState {
            ModuleState::Running
        }
    }
    // Port 由 blanket impl 覆盖，无需显式实现
    impl TrayProvider for FakeProvider {
        fn tray_menu_items(&self) -> Vec<TrayMenuItem> {
            vec![TrayMenuItem { id: "open".into(), label: "打开面板".into(), enabled: true }]
        }
    }

    #[test]
    fn aggregate_sorts_by_priority() {
        let abilities = Ports::new();
        abilities.register_multi::<dyn TrayProvider>(Arc::new(FakeProvider {
            id: "b_module",
            priority: 20,
        }));
        abilities.register_multi::<dyn TrayProvider>(Arc::new(FakeProvider {
            id: "a_module",
            priority: 5,
        }));
        let sections = aggregate_tray(&abilities);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].module, "a_module");
        assert_eq!(sections[1].module, "b_module");
        assert_eq!(sections[0].items[0].label, "打开面板");
    }
}
