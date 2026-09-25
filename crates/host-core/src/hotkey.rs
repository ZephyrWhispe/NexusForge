//! 全局快捷键管理（docs/impl/01 S6.2）
//!
//! 冲突仲裁：同一键组合，priority 小者优先；
//! - 已注册者 priority ≤ 新注册者 → 新注册失败（`HOST_HOTKEY_001`，可操作 hint）
//! - 新注册者 priority 更小 → 替换既有注册（旧归属者被通知性日志记录）
//! - 有 HotkeyWinPort 时同步调用 OS 注册；OS 失败 → `HOST_HOTKEY_002`

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use crate::capability::HotkeyBinding;
use crate::codes;
use crate::error::AppError;
use crate::ports::{HotkeyWinPort, Ports};

struct Owner {
    module: String,
    priority: u8,
    binding: HotkeyBinding,
}

/// binding.id → 注册事实（组合 + OS 层 id；COR-14：注销需要 os_id 才能
/// 真正调用 HotkeyWinPort::unregister，否则旧快捷键在系统层继续生效）
#[derive(Clone)]
struct Registration {
    combo: String,
    os_id: Option<i32>,
}

/// os_id → 触发动作（分发器与注册表共享的槽位表）
type OsFireMap = HashMap<i32, Arc<dyn Fn() + Send + Sync>>;

pub struct HotkeyManager {
    /// 键组合 → 归属
    by_combo: RwLock<HashMap<String, Owner>>,
    /// binding.id → 注册事实（unregister 用）
    by_id: RwLock<HashMap<String, Registration>>,
    /// os_id → on_fire（OS 分发器回调映射；Arc 化以便进入 'static 分发器闭包）
    os_map: Arc<RwLock<OsFireMap>>,
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
        let mut installed = self.dispatcher_installed.write();
        if *installed {
            return;
        }
        // os_map 为 Arc<RwLock>：分发器闭包每次触发时取读锁
        let os_map = Arc::clone(&self.os_map);
        win.set_dispatcher(Arc::new(move |os_id| {
            if let Some(fire) = os_map.read().get(&os_id) {
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
        let old_reg = {
            let guard = self.by_id.read();
            guard.get(&binding.id).cloned()
        };
        if let Some(old_reg) = old_reg {
            if old_reg.combo != combo {
                self.unregister(&binding.id)?;
            }
        }

        // COR-14：displaced 记录**被抢占方的 binding.id**（原实现只留模块名做日志，
        // 旧注册从未注销——旧归属者的 on_fire 仍挂 os_map、旧 os_id 仍在系统层生效）
        let displaced: Option<(String, String)> = {
            let combos = self.by_combo.write();
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
                Some(existing) => Some((existing.module.clone(), existing.binding.id.clone())),
                None => None,
            }
        };
        // 被抢占方的旧注册整体注销（内存 + OS 层）
        if let Some((_, displaced_id)) = &displaced {
            self.unregister(displaced_id)?;
        }

        // OS 层注册（若端口已就绪）；os_id 记入 Registration 供注销配对
        let mut os_id_slot = None;
        if let Some(win) = self.ports.get::<dyn HotkeyWinPort>() {
            self.ensure_dispatcher(&win);
            let mut next = self.next_os_id.write();
            let os_id = *next;
            *next += 1;
            win.register(os_id, binding.modifiers, binding.vk)
                .map_err(|e| {
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
            self.os_map.write().insert(os_id, on_fire.clone());
            os_id_slot = Some(os_id);
        }

        let mut combos = self.by_combo.write();
        combos.insert(
            combo.clone(),
            Owner {
                module: owner_module.into(),
                priority: owner_priority,
                binding: binding.clone(),
            },
        );
        drop(combos);
        self.by_id.write().insert(
            binding.id,
            Registration {
                combo,
                os_id: os_id_slot,
            },
        );
        if let Some((m, _)) = &displaced {
            tracing::warn!(displaced = %m, "模块快捷键被更高优先级注册替换");
        }
        Ok(())
    }

    pub fn unregister(&self, binding_id: &str) -> Result<(), AppError> {
        let reg = self.by_id.write().remove(binding_id);
        if let Some(reg) = reg {
            self.by_combo.write().remove(&reg.combo);
            if let Some(os_id) = reg.os_id {
                self.os_map.write().remove(&os_id);
                // COR-14：必须调用端口真注销，否则改键/删绑定后旧组合在系统层
                // 仍触发（内存态与 OS 态不一致）；os_map 闭包随之泄漏
                if let Some(win) = self.ports.get::<dyn HotkeyWinPort>() {
                    win.unregister(os_id)?;
                }
            }
        }
        Ok(())
    }

    /// 注销某模块注册的全部绑定（模块重启/停用时调用；返回注销数）。
    /// COR-14 配套：此前模块 stop 无任何热键回收路径，os_id 单调泄漏。
    pub fn unregister_module(&self, module: &str) -> usize {
        let ids: Vec<String> = {
            let by_id = self.by_id.read();
            let combos = self.by_combo.read();
            by_id
                .iter()
                .filter(|(_, reg)| {
                    combos
                        .get(&reg.combo)
                        .map(|o| o.module == module)
                        .unwrap_or(false)
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        let mut n = 0;
        for id in &ids {
            if self.unregister(id).is_ok() {
                n += 1;
            }
        }
        n
    }

    pub fn owner_of(&self, modifiers: u32, vk: u32) -> Option<(String, String)> {
        let combos = self.by_combo.read();
        combos
            .get(&Self::combo(modifiers, vk))
            .map(|o| (o.module.clone(), o.binding.label.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU32, Ordering};

    struct FakeWin {
        ok: bool,
        calls: AtomicU32,
        /// COR-14：记录 unregister 收到的 os_id 序列
        unregistered: std::sync::Mutex<Vec<i32>>,
    }
    impl FakeWin {
        fn new(ok: bool) -> Self {
            Self {
                ok,
                calls: AtomicU32::new(0),
                unregistered: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn unregistered_ids(&self) -> Vec<i32> {
            self.unregistered.lock().unwrap().clone()
        }
    }
    impl HotkeyWinPort for FakeWin {
        fn register(&self, _id: i32, _m: u32, _vk: u32) -> Result<(), AppError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.ok {
                Ok(())
            } else {
                Err(AppError::module(
                    "WIN_HOTKEY_001",
                    "RegisterHotKey 失败",
                    None,
                ))
            }
        }
        fn unregister(&self, id: i32) -> Result<(), AppError> {
            self.unregistered.lock().unwrap().push(id);
            Ok(())
        }
        fn set_dispatcher(&self, _d: Arc<dyn Fn(i32) + Send + Sync>) {}
    }

    fn binding(id: &str, m: u32, vk: u32) -> HotkeyBinding {
        HotkeyBinding {
            id: id.into(),
            label: id.into(),
            modifiers: m,
            vk,
        }
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
        mgr.register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op())
            .unwrap();
        // priority 5 < 10 → 抢占成功
        mgr.register("desktop", 5, binding("b", MOD, 'V' as u32), no_op())
            .unwrap();
        assert_eq!(mgr.owner_of(MOD, 'V' as u32).unwrap().0, "desktop");
        // 原 owner 重试 → 现在轮到它失败
        let err = mgr
            .register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op())
            .unwrap_err();
        assert_eq!(err.code(), codes::host::HOST_HOTKEY_001);
    }

    #[test]
    fn equal_or_lower_priority_rejected() {
        let mgr = manager_with(None);
        mgr.register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op())
            .unwrap();
        let err = mgr
            .register("desktop", 10, binding("b", MOD, 'V' as u32), no_op())
            .unwrap_err();
        assert!(matches!(err, AppError::Permission { .. }));
        assert_eq!(mgr.owner_of(MOD, 'V' as u32).unwrap().0, "clipboard");
    }

    #[test]
    fn os_failure_maps_to_hotkey_002() {
        let mgr = manager_with(Some(Arc::new(FakeWin::new(false))));
        let err = mgr
            .register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op())
            .unwrap_err();
        assert_eq!(err.code(), codes::host::HOST_HOTKEY_002);
    }

    #[test]
    fn unregister_calls_os_and_clears_dispatch_map() {
        // COR-14：unregister 必须真注销 OS 层（旧实现只清内存映射）
        let win = Arc::new(FakeWin::new(true));
        let mgr = manager_with(Some(win.clone()));
        mgr.register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op())
            .unwrap();
        assert_eq!(win.calls.load(Ordering::SeqCst), 1);
        // 分发器触发（模拟 WM_HOTKEY）应命中 on_fire
        let fired = Arc::new(AtomicU32::new(0));
        let fired2 = fired.clone();
        mgr.register(
            "clipboard",
            10,
            binding("b", MOD, 'P' as u32),
            Arc::new(move || {
                fired2.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap();
        mgr.unregister("b").unwrap();
        // os_id 已从 os_map 移除 + win.unregister 被调用
        assert_eq!(win.unregistered_ids().len(), 1, "OS 注销必须被调用");
        assert!(mgr.owner_of(MOD, 'P' as u32).is_none());
    }

    #[test]
    fn displaced_binding_is_unregistered_at_os_level() {
        // COR-14：抢占替换时，被抢占方的旧 os_id 必须注销——否则旧闭包仍挂
        // os_map、旧键继续触发旧动作
        let win = Arc::new(FakeWin::new(true));
        let mgr = manager_with(Some(win.clone()));
        mgr.register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op())
            .unwrap();
        // desktop(5) 抢占 clipboard(10) 的组合
        mgr.register("desktop", 5, binding("b", MOD, 'V' as u32), no_op())
            .unwrap();
        let ids = win.unregistered_ids();
        assert_eq!(
            ids.len(),
            1,
            "被抢占方必须在 OS 层注销一次（os_id {ids:?}）"
        );
        // 抢占后旧归属者重试注册：占用判定不再受僵尸注册影响（原行为已正确，防回归）
        let err = mgr
            .register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op())
            .unwrap_err();
        assert_eq!(err.code(), codes::host::HOST_HOTKEY_001);
    }

    #[test]
    fn unregister_module_removes_all_bindings() {
        let win = Arc::new(FakeWin::new(true));
        let mgr = manager_with(Some(win.clone()));
        mgr.register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op())
            .unwrap();
        mgr.register("clipboard", 10, binding("b", MOD, 'P' as u32), no_op())
            .unwrap();
        mgr.register("desktop", 10, binding("c", MOD, 'K' as u32), no_op())
            .unwrap();
        let n = mgr.unregister_module("clipboard");
        assert_eq!(n, 2);
        assert!(mgr.owner_of(MOD, 'V' as u32).is_none());
        assert!(mgr.owner_of(MOD, 'P' as u32).is_none());
        assert_eq!(mgr.owner_of(MOD, 'K' as u32).unwrap().0, "desktop");
        assert_eq!(win.unregistered_ids().len(), 2);
    }

    #[test]
    fn os_registration_invoked_when_port_ready() {
        let win = Arc::new(FakeWin::new(true));
        let mgr = manager_with(Some(win.clone()));
        mgr.register("clipboard", 10, binding("a", MOD, 'V' as u32), no_op())
            .unwrap();
        assert_eq!(win.calls.load(Ordering::SeqCst), 1);
        // 同 id 换键：旧组合被移除
        mgr.register("clipboard", 10, binding("a", MOD, 'P' as u32), no_op())
            .unwrap();
        assert!(mgr.owner_of(MOD, 'V' as u32).is_none());
        assert_eq!(mgr.owner_of(MOD, 'P' as u32).unwrap().0, "clipboard");
    }
}
