//! 模块能力 trait（docs/impl/01 S3 能力接口 + S6.3 托盘聚合）
//!
//! 模块按需实现；注册表经 `Ports::register_multi` 收集多实例，
//! 宿主聚合后驱动托盘菜单与全局快捷键注册。

use std::sync::Arc;

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

/// 托盘菜单点击动作（D-26：与 [`HotkeyAction`] 同型，宿主以 `{module}:{item_id}` 查表调用）
pub struct TrayAction {
    pub item_id: String,
    /// 触发动作（宿主在托盘菜单事件回调线程调用；动作须快速返回，耗时逻辑自行转线程）
    pub action: Arc<dyn Fn() + Send + Sync>,
}

/// 提供托盘菜单的模块能力
pub trait TrayProvider: Module + Port {
    fn tray_menu_items(&self) -> Vec<TrayMenuItem>;
    /// 菜单项 id → 动作闭包（默认无动作 = 仅展示项；Error 置灰由宿主聚合层负责）
    fn tray_actions(&self) -> Vec<TrayAction> {
        Vec::new()
    }
}

/// 注册全局快捷键的模块能力
pub trait HotkeyProvider: Module + Port {
    fn global_hotkeys(&self) -> Vec<HotkeyBinding>;
    /// 绑定 id → 触发动作（宿主在 OS 热键到达时调用；动作须快速返回，耗时逻辑自行转线程）
    fn hotkey_actions(&self) -> Vec<HotkeyAction>;
}

/// 热键触发动作
pub struct HotkeyAction {
    pub binding_id: String,
    pub action: Arc<dyn Fn() + Send + Sync>,
}

/// 托盘菜单聚合段（一个提供能力的模块一段）
#[derive(Clone, Debug, serde::Serialize)]
pub struct TraySection {
    pub module: String,
    pub priority: u8,
    pub items: Vec<TrayMenuItem>,
}

/// 聚合全部 TrayProvider（按模块 priority 升序，docs/impl/01 S6.3；
/// D-26：模块处于 Error 态时其全部条目置灰——S6.3 的"置灰"条款在此收敛）
pub fn aggregate_tray(abilities: &crate::ports::Ports) -> Vec<TraySection> {
    let mut sections: Vec<TraySection> = abilities
        .get_all::<dyn TrayProvider>()
        .into_iter()
        .map(|p| {
            let info = p.info();
            let errored = p.status() == crate::module::ModuleState::Error;
            TraySection {
                module: info.id.to_owned(),
                priority: info.priority,
                items: p
                    .tray_menu_items()
                    .into_iter()
                    .map(|mut item| {
                        if errored {
                            item.enabled = false;
                        }
                        item
                    })
                    .collect(),
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
        state: crate::module::ModuleStateCell,
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
            self.state.get()
        }
        fn set_status(&self, state: ModuleState) {
            self.state.set(state);
        }
    }
    // Port 由 blanket impl 覆盖，无需显式实现
    impl TrayProvider for FakeProvider {
        fn tray_menu_items(&self) -> Vec<TrayMenuItem> {
            vec![TrayMenuItem {
                id: "open".into(),
                label: "打开面板".into(),
                enabled: true,
            }]
        }
    }

    #[test]
    fn aggregate_sorts_by_priority() {
        let abilities = Ports::new();
        abilities.register_multi::<dyn TrayProvider>(Arc::new(FakeProvider {
            id: "b_module",
            priority: 20,
            state: crate::module::ModuleStateCell::new(),
        }));
        abilities.register_multi::<dyn TrayProvider>(Arc::new(FakeProvider {
            id: "a_module",
            priority: 5,
            state: crate::module::ModuleStateCell::new(),
        }));
        let sections = aggregate_tray(&abilities);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].module, "a_module");
        assert_eq!(sections[1].module, "b_module");
        assert_eq!(sections[0].items[0].label, "打开面板");
    }

    #[test]
    fn aggregate_grays_error_modules_only() {
        // D-26 验收①（S6.3 置灰条款）：Error 模块条目 enabled=false；
        // Running 不受影响；provider 自身声明的 enabled=false 在 Running 下保持
        let err_state = crate::module::ModuleStateCell::new();
        err_state.set(ModuleState::Error);
        let run_state = crate::module::ModuleStateCell::new();
        run_state.set(ModuleState::Running);
        let stopped_state = crate::module::ModuleStateCell::new();
        stopped_state.set(ModuleState::Stopped);
        let abilities = Ports::new();
        abilities.register_multi::<dyn TrayProvider>(Arc::new(FakeProvider {
            id: "err_module",
            priority: 5,
            state: err_state,
        }));
        abilities.register_multi::<dyn TrayProvider>(Arc::new(FakeProvider {
            id: "run_module",
            priority: 10,
            state: run_state,
        }));
        abilities.register_multi::<dyn TrayProvider>(Arc::new(FakeProvider {
            id: "stopped_module",
            priority: 15,
            state: stopped_state,
        }));
        let sections = aggregate_tray(&abilities);
        assert_eq!(sections.len(), 3);
        assert!(!sections[0].items[0].enabled, "Error 模块条目必须置灰");
        assert!(sections[1].items[0].enabled, "Running 条目不受影响");
        assert!(
            sections[2].items[0].enabled,
            "Stopped 不触发置灰（S6.3 字面只灰 Error 态）"
        );
    }

    #[test]
    fn tray_actions_default_empty_and_override_collected() {
        // D-26 决策②：默认实现不破既有使用方；覆写后宿主可收集 id→闭包
        struct ActionProvider {
            state: crate::module::ModuleStateCell,
            fired: Arc<std::sync::atomic::AtomicU32>,
        }
        impl Module for ActionProvider {
            fn info(&self) -> crate::module::ModuleInfo {
                crate::module::ModuleInfo {
                    id: "act",
                    name: "act",
                    version: "0.1.0",
                    icon: None,
                    priority: 5,
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
                self.state.get()
            }
            fn set_status(&self, state: ModuleState) {
                self.state.set(state);
            }
        }
        impl TrayProvider for ActionProvider {
            fn tray_menu_items(&self) -> Vec<TrayMenuItem> {
                vec![
                    TrayMenuItem {
                        id: "go".into(),
                        label: "触发".into(),
                        enabled: true,
                    },
                    TrayMenuItem {
                        id: "view".into(),
                        label: "仅展示".into(),
                        enabled: true,
                    },
                ]
            }
            fn tray_actions(&self) -> Vec<TrayAction> {
                let fired = self.fired.clone();
                vec![TrayAction {
                    item_id: "go".into(),
                    action: Arc::new(move || {
                        fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }),
                }]
            }
        }

        let abilities = Ports::new();
        // 默认实现（FakeProvider 未覆写 tray_actions）→ 空
        abilities.register_multi::<dyn TrayProvider>(Arc::new(FakeProvider {
            id: "dflt",
            priority: 5,
            state: crate::module::ModuleStateCell::new(),
        }));
        assert!(abilities
            .get_all::<dyn TrayProvider>()
            .iter()
            .all(|p| p.tray_actions().is_empty()));

        let fired = Arc::new(std::sync::atomic::AtomicU32::new(0));
        abilities.register_multi::<dyn TrayProvider>(Arc::new(ActionProvider {
            state: crate::module::ModuleStateCell::new(),
            fired: fired.clone(),
        }));
        let actions: Vec<(String, TrayAction)> = abilities
            .get_all::<dyn TrayProvider>()
            .into_iter()
            .flat_map(|p| {
                let module = p.info().id.to_owned();
                p.tray_actions()
                    .into_iter()
                    .map(move |a| (format!("{module}:{}", a.item_id), a))
            })
            .collect();
        assert_eq!(actions.len(), 1, "仅覆写方提供动作");
        assert_eq!(actions[0].0, "act:go");
        (actions[0].1.action)();
        assert_eq!(fired.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
