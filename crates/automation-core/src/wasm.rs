//! A5 WASM 插件运行时（docs/impl/07 A5）：wasmtime 沙箱。
//!
//! - 内存限额 64MB（StoreLimits）+ fuel 计算限额（防死循环）；超限 → Err，宿主无感（进死信）
//! - 无 WASI：无文件系统/网络/时钟
//! - 宿主函数白名单：`nf.log`（恒给）/ `nf.open_url` / `nf.notify`（按 A6 插件权限逐一注册）
//! - 字符串传递：guest 导出 "memory"，宿主函数按 (ptr: i32, len: i32) 从线性内存读取
//!
//! wat feature 用于测试直接写 wat 样本（恶意样本验收：无限循环/内存炸弹被限额终止）。

use wasmtime::{Caller, Config, Engine, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder};

use crate::error::{AutomationError, Result};

/// 内存限额（docs/impl/07 A5：64MB）
pub const WASM_MEM_LIMIT: u64 = 64 * 1024 * 1024;
/// fuel 计算限额（防死循环；耗尽 → trap）
pub const WASM_FUEL: u64 = 10_000_000;

/// 宿主能力开关（A6：manifest.permissions → 白名单映射；log 恒给，不在权限表）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WasmCaps {
    pub allow_open: bool,
    pub allow_notify: bool,
}

/// 宿主回调（HostActionHandler 实现；测试用 Fake）
pub trait WasmHost: Send + Sync {
    fn log(&self, msg: &str);
    fn open_url(&self, url: &str) -> Result<()>;
    fn notify(&self, title: &str, body: &str) -> Result<()>;
}

struct HostCtx<'a> {
    limits: StoreLimits,
    host: &'a dyn WasmHost,
}

/// Store 数据是 HostCtx（limiter 闭包需要 &mut StoreLimits）；
/// 宿主函数闭包里读 guest 内存需要 Caller（持 store 借用），data 借用不冲突（读 mem 走 caller.get_export）。
pub struct WasmRuntime {
    engine: Engine,
}

impl WasmRuntime {
    pub fn new() -> Result<Self> {
        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        let engine = Engine::new(&cfg)
            .map_err(|e| AutomationError::Action(format!("WASM 引擎初始化失败: {e}")))?;
        Ok(Self { engine })
    }

    /// 执行 wasm 的导出函数（无参无返回）。fuel/内存超限或 guest trap → Err。
    pub fn run(&self, wasm: &[u8], func: &str, caps: WasmCaps, host: &dyn WasmHost) -> Result<()> {
        let module = Module::new(&self.engine, wasm)
            .map_err(|e| AutomationError::Action(format!("WASM 加载失败: {e}")))?;
        let mut store = Store::new(
            &self.engine,
            HostCtx {
                limits: StoreLimitsBuilder::new()
                    .memory_size(WASM_MEM_LIMIT as usize)
                    .memories(1)
                    .tables(1)
                    .build(),
                host,
            },
        );
        store.limiter(|ctx| &mut ctx.limits);
        store
            .set_fuel(WASM_FUEL)
            .map_err(|e| AutomationError::Action(format!("fuel 设置失败: {e}")))?;

        let mut linker: Linker<HostCtx> = Linker::new(&self.engine);
        // nf.log(ptr, len)：恒可用（调试输出）
        linker
            .func_wrap(
                "nf",
                "log",
                |mut caller: Caller<'_, HostCtx<'_>>, ptr: i32, len: i32| -> wasmtime::Result<()> {
                    let msg = read_guest_str(&mut caller, ptr, len)?;
                    (caller.data().host).log(&msg);
                    Ok(())
                },
            )
            .map_err(wasm_err)?;
        // nf.open_url(ptr, len)：权限映射（A6）
        if caps.allow_open {
            linker
                .func_wrap(
                    "nf",
                    "open_url",
                    |mut caller: Caller<'_, HostCtx<'_>>, ptr: i32, len: i32| -> wasmtime::Result<()> {
                        let url = read_guest_str(&mut caller, ptr, len)?;
                        (caller.data().host).open_url(&url).map_err(|e| wasmtime::Error::msg(e.to_string()))
                    },
                )
                .map_err(wasm_err)?;
        }
        // nf.notify(title_ptr, title_len, body_ptr, body_len)：权限映射（A6）
        if caps.allow_notify {
            linker
                .func_wrap(
                    "nf",
                    "notify",
                    |mut caller: Caller<'_, HostCtx<'_>>,
                     t_ptr: i32,
                     t_len: i32,
                     b_ptr: i32,
                     b_len: i32|
                     -> wasmtime::Result<()> {
                        let title = read_guest_str(&mut caller, t_ptr, t_len)?;
                        let body = read_guest_str(&mut caller, b_ptr, b_len)?;
                        (caller.data().host)
                            .notify(&title, &body)
                            .map_err(|e| wasmtime::Error::msg(e.to_string()))
                    },
                )
                .map_err(wasm_err)?;
        }

        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| AutomationError::Action(format!("WASM 实例化失败: {e}")))?;
        let entry = instance
            .get_typed_func::<(), ()>(&mut store, func)
            .map_err(|e| AutomationError::Action(format!("入口函数 {func} 不存在或不匹配: {e}")))?;
        entry.call(&mut store, ()).map_err(|e| {
            // anyhow {:#} 输出整链（含 trap 根因——fuel 耗尽/宿主错误在链尾）
            let msg = format!("{e:#}");
            if msg.contains("fuel") {
                // fuel 耗尽（死循环防护，docs/impl/07 验收：恶意样本被限额终止）
                AutomationError::Action("WASM fuel 耗尽（疑似死循环，已终止）".into())
            } else {
                AutomationError::Action(format!("WASM 执行失败: {msg}"))
            }
        })?;
        Ok(())
    }
}

/// 从 guest 线性内存读 (ptr, len) 字符串（lossy UTF-8）
fn read_guest_str(caller: &mut Caller<'_, HostCtx<'_>>, ptr: i32, len: i32) -> wasmtime::Result<String> {
    let mem: Memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::Error::msg("guest 未导出 memory"))?;
    let start = ptr.max(0) as usize;
    let size = len.max(0) as usize;
    let mut buf = vec![0u8; size];
    mem.read(caller, start, &mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn wasm_err(e: wasmtime::Error) -> AutomationError {
    AutomationError::Action(format!("WASM 链接失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeHost {
        logs: Mutex<Vec<String>>,
    }
    impl WasmHost for FakeHost {
        fn log(&self, msg: &str) {
            self.logs.lock().unwrap().push(msg.to_string());
        }
        fn open_url(&self, _url: &str) -> Result<()> {
            Err(AutomationError::Action("open 未授权于测试".into()))
        }
        fn notify(&self, _title: &str, _body: &str) -> Result<()> {
            Ok(())
        }
    }

    /// 正常插件：导出 memory + 调 nf.log
    const WAT_OK: &str = r#"
        (module
          (import "nf" "log" (func $log (param i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "hello plugin")
          (func (export "run")
            (call $log (i32.const 0) (i32.const 12))))
    "#;

    /// 恶意样本 1：无限循环（fuel 应终止）
    const WAT_INFINITE: &str = r#"
        (module (func (export "run") (loop br 0)))
    "#;

    /// 恶意样本 2：内存炸弹（memory.grow 超过 64MB 限额后持续循环 → fuel 终止）
    const WAT_MEMBOMB: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run")
            (loop $l
              (drop (memory.grow (i32.const 16)))
              (br $l))))
    "#;

    #[test]
    fn runs_and_reads_guest_string() {
        let rt = WasmRuntime::new().unwrap();
        let host = FakeHost::default();
        // wat feature：Module::new 接受 wat 文本（以 \0asm 开头才按二进制解析）
        rt.run(WAT_OK.as_bytes(), "run", WasmCaps::default(), &host).unwrap();
        assert_eq!(*host.logs.lock().unwrap(), vec!["hello plugin".to_string()]);
    }

    #[test]
    fn unknown_function_rejected() {
        let rt = WasmRuntime::new().unwrap();
        let host = FakeHost::default();
        let err = rt
            .run(WAT_OK.as_bytes(), "no_such_fn", WasmCaps::default(), &host)
            .unwrap_err();
        assert!(err.to_string().contains("不存在"));
    }

    #[test]
    fn infinite_loop_killed_by_fuel() {
        let rt = WasmRuntime::new().unwrap();
        let host = FakeHost::default();
        let err = rt
            .run(WAT_INFINITE.as_bytes(), "run", WasmCaps::default(), &host)
            .unwrap_err();
        assert!(err.to_string().contains("fuel"), "实际错误: {err}");
    }

    #[test]
    fn memory_bomb_killed_by_limit() {
        let rt = WasmRuntime::new().unwrap();
        let host = FakeHost::default();
        // 64MB = 1024 页，每次 grow 16 页：超限 grow 失败（返回 -1）循环继续 → fuel 兜底终止
        let err = rt
            .run(WAT_MEMBOMB.as_bytes(), "run", WasmCaps::default(), &host)
            .unwrap_err();
        assert!(
            err.to_string().contains("fuel") || err.to_string().contains("失败"),
            "实际错误: {err}"
        );
    }

    #[test]
    fn capped_import_is_unresolved() {
        // allow_open=false 时插件 import nf.open_url → 链接失败（未注册），权限即沙箱边界
        let rt = WasmRuntime::new().unwrap();
        let host = FakeHost::default();
        let wat = r#"
            (module
              (import "nf" "open_url" (func (param i32 i32)))
              (memory (export "memory") 1)
              (func (export "run") (call 0 (i32.const 0) (i32.const 0))))
        "#;
        let err = rt
            .run(wat.as_bytes(), "run", WasmCaps { allow_open: false, allow_notify: false }, &host)
            .unwrap_err();
        assert!(err.to_string().contains("实例化失败"));
        // 允许 open → 链接成功，宿主回调（本测试宿主返回 Err → 执行 Err）
        let err = rt
            .run(wat.as_bytes(), "run", WasmCaps { allow_open: true, allow_notify: false }, &host)
            .unwrap_err();
        assert!(err.to_string().contains("open 未授权"));
    }

    /// Arc<dyn WasmHost> 走 trait object（HostActionHandler 场景）
    #[test]
    fn host_via_arc_object_safe() {
        let rt = WasmRuntime::new().unwrap();
        let host: Arc<dyn WasmHost> = Arc::new(FakeHost::default());
        rt.run(WAT_OK.as_bytes(), "run", WasmCaps::default(), host.as_ref()).unwrap();
    }
}
