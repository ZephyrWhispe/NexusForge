//! 全局快捷键管理（docs/impl/01 S6.2）
//!
//! 冲突仲裁：同一键组合，priority 小者优先；
//! - 已注册者 priority ≤ 新注册者 → 新注册失败（`HOST_HOTKEY_001`，可操作 hint）
//! - 新注册者 priority 更小 → 替换既有注册（旧归属者被通知性日志记录）
//! - 有 HotkeyWinPort 时同步调用 OS 注册；OS 失败 → `HOST_HOTKEY_002`

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::capability::HotkeyBinding;
use crate::codes;
use crate::error::AppError;
use crate::ports::{HotkeyWinPort, Ports};

struct Owner {
    module: String,
    priority: u8,
    binding: HotkeyBinding,
    /// OS 热键触发时执行（快速返回；耗时逻辑自行转线程）
    on_fire: Arc<dyn Fn() + Send + Sync>,
}

pub struct HotkeyManager {
    /// 键组合 → 归属
    by_combo: RwLock<HashMap<String, Owner>>,
    /// binding.id → 键组合（unregister 用）
    by_id: RwLock<HashMap<String, String>>,
    /// os_id → on_fire（OS 分发器回调映射；Arc 化以便进入 'static 分发器闭包）
    os_map: Arc<RwLock<HashMap<i32, Arc<dyn Fn() + Send + Sync>>>>,
    ports: Arc<Ports>,
    /// OS 热键 id 分配器
    next_os_id: RwLock<i32>,
    /// 分发器是否已安装（每 Port 实例仅需一次）
    dispatcher_installed: RwLock<bool>,
}

impl HotkeyManager {
    pub fn new(ports: Arc<Ports>) -> Self {
        Self {
            by_combo: RwLock::new(HashMap::new()),
            by_id: RwLock::new(HashMap::new()),
            os_map: Arc::new(RwLock::new(HashMap::new())),
            ports,
            next_os_id: RwLock::new(1),
            dispatcher_installed: RwLock::new(false),
        }
    }

    /// 安装 OS 分发器（幂等）：WM_HOTKEY → os_id → on_fire
    fn ensure_dispatcher(&self, win: &Arc<dyn HotkeyWinPort>) {
        let mut installed = self.dispatcher_installed.write().expect("dispatcher 写锁");
        if *installed {
            return;
        }
        // os_map 为 Arc<RwLock>：分发器闭包每次触发时取读锁
        let os_map = Arc::clone(&self.os_map);
        win.set_dispatcher(Arc::new(move |os_id| {
            if let Some(fire) = os_map.read().expect("os_map 读锁").get(&os_id) {
                fire();
            }
        }));
        *installed = true;
    }

    fn combo(modifiers: u32, vk: u32) -> String {
        format!("{modifiers:08x}+{vk:04x}")
    }

    pub fn register(
        &self,
        owner_module: &str,
        owner_priority: u8,
        binding: HotkeyBinding,
        on_fire: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), AppError> {
        let combo = Self::combo(binding.modifiers, binding.vk);

        // 同模块同 id 重复注册：先移除旧组合。
        // 注意：读锁必须在块内释放——if-let 判定式的临时守卫会存活到块尾，
        // 块内 unregister 取写锁会造成自锁（本文件曾因此死锁，见 git 历史）。
        let old_combo = {
            let guard = self.by_id.read().expect("by_id 读锁");
            guard.get(&binding.id).cloned()
        };
        if let Some(old_combo) = old_combo {
            if old_combo != combo {
                self.unregister(&binding.id)?;
            }
        }

        let displaced = {
            let mut combos = self.by_combo.write().expect("by_combo 写锁");
            match combos.get(&combo) {
                Some(existing) if existing.priority <= owner_priority => {
                    return Err(AppError::Permission {
                        code: codes::host::HOST_HOTKEY_001.into(),
                        message: format!(
                            "快捷键 [{}] 已被 {} 占用",
                            binding.label, existing.module
                        ),
                        hint: "在设置中心的快捷键面板中更换组合".into(),
                    });
                }
                Some(existing) => (Some(existing.module.clone()), existing.binding.id.clone()),
                None => (None, String::new()),
            }
            .0
        };

        // OS 层注册（若端口已就绪）
        if let Some(win) = self.ports.get::<dyn HotkeyWinPort>() {
            self.ensure_dispatcher(&win);
            let mut next = self.next_os_id.write().expect("os id 写锁");
            let os_id = *next;
            *next += 1;
            win.register(os_id, binding.modifiers, binding.vk).map_err(|e| {
                let hint = match &e {
                    AppError::Module { message, .. } => message.clone(),
                    other => other.to_string(),
                };
                AppError::Permission {
                    code: codes::host::HOST_HOTKEY_002.into(),
                    message: format!("快捷键 [{}] 被系统占用", binding.label),
                    hint,
                }
            })?;
            self.os_map
                .write()
                .expect("os_map 写锁")
                .insert(os_id, on_fire.clone());
        }

        let mut combos = self.by_combo.write().expect("by_combo 写锁");
        combos.insert(
            combo.clone(),
            Owner { module: owner_module.into(), priority: owner_priority, binding: binding.clone(), on_fire },
        );
        drop(combos);
        self.by_id
            .write()
            .expect("by_id 写锁")
            .insert(binding.id, combo);
        if let Some(m) = displaced {
            tracing::warn!(displaced = %m, "模块快捷键被更高优先级注册替换");
        }
        Ok(())
    }

    pub fn unregister(&self, binding_id: &str) -> Result<(), AppError> {
        let combo = self
            .by_id
            .write()
            .expect("by_id 写锁")
            .remove(binding_id);
        if let Some(combo) = combo {
            self.by_combo.write().expect("by_combo 写锁").remove(&combo);
        }
        Ok(())
    }

    pub fn owner_of(&self, modifiers: u32, vk: u32) -> Option<(String, String)> {
        let combos = self.by_combo.read().expect("by_combo 读锁");
        combos
            .get(&Self::combo(modifiers, vk))
            .map(|o| (o.module.clone(), o.binding.label.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Port;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct FakeWin {
        ok: bool,
        calls: AtomicU32,
    }
    impl HotkeyWinPort for FakeWin {
        fn register(&self, _id: i32, _m: u32, _vk: u32) -> Result<(), AppError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.ok {
                Ok(())
            } else {
                Err(AppError::module("WIN_HOTKEY_001", "RegisterHotKey 失败", None))
            }
        }
        fn unregister(&self, _id: i32) -> Result<(), AppError> {
            Ok(())
        }
        fn set_dispatcher(&self, _d: Arc<dyn Fn(i32) + Send + Sync>) {}
    }

    fn binding(id: &str, m: u32, vk: u32) -> HotkeyBinding {
        HotkeyBinding { id: id.into(), label: id.into(), modifiers: m, vk }
    }

    fn no_op() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    fn manager_with(win: Option<Arc<FakeWin>>) -> HotkeyManager {
        let ports = Ports::new();
        if let Some(w) = win {
            ports.register::<dyn HotkeyWinPort>(w);
        }
        HotkeyManager::new(Arc::new(ports))
    }

    const MOD: u32 = 0x0002 | 0x0004; // CONTROL|SHIFT

    #[test]
    fn higher_priority_displaces_lower() {
        let mgr = manager_with(None);
        mgr.register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op()).unwrap();
        // priority 5 < 10 → 抢占成功
        mgr.register("desktop", 5, binding("b", MOD, 'V' as u32), no_op()).unwrap();
        assert_eq!(mgr.owner_of(MOD, 'V' as u32).unwrap().0, "desktop");
        // 原 owner 重试 → 现在轮到它失败
        let err = mgr.register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op()).unwrap_err();
        assert_eq!(err.code(), codes::host::HOST_HOTKEY_001);
    }

    #[test]
    fn equal_or_lower_priority_rejected() {
        let mgr = manager_with(None);
        mgr.register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op()).unwrap();
        let err = mgr.register("desktop", 10, binding("b", MOD, 'V' as u32), no_op()).unwrap_err();
        assert!(matches!(err, AppError::Permission { .. }));
        assert_eq!(mgr.owner_of(MOD, 'V' as u32).unwrap().0, "clipboard");
    }

    #[test]
    fn os_failure_maps_to_hotkey_002() {
        let mgr = manager_with(Some(Arc::new(FakeWin { ok: false, calls: AtomicU32::new(0) })));
        let err = mgr.register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op()).unwrap_err();
        assert_eq!(err.code(), codes::host::HOST_HOTKEY_002);
    }

    #[test]
    fn os_registration_invoked_when_port_ready() {
        let win = Arc::new(FakeWin { ok: true, calls: AtomicU32::new(0) });
        let mgr = manager_with(Some(win.clone()));
        mgr.register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op()).unwrap();
        assert_eq!(win.calls.load(Ordering::SeqCst), 1);
        // 同 id 换键：旧组合被移除
        mgr.register("clipboard", 10, binding("a", MOD, 'P' as u32), no_op()).unwrap();
        assert!(mgr.owner_of(MOD, 'V' as u32).is_none());
        assert_eq!(mgr.owner_of(MOD, 'P' as u32).unwrap().0, "clipboard");
    }
}
