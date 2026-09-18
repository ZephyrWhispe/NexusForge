//! WinOps 提权数据面（docs/impl/08 §4 W3）：
//! HelperClient（JSON-RPC over 同步命名管道帧，短连接）+ helper-backed Port 适配器。
//!
//! - 首次调用 ensure_up → HelperSpawnPort::spawn（UAC 弹窗）→ token 经环境变量注入
//! - 短连接：每次 call 连接→握手→请求→关闭（helper 端逐连接服务，v1 无连接复用）
//! - helper 120s 空闲自退后：connect 失败 → 重新 spawn（新 token）→ UAC 再弹一次
//! - RoutingRegistry：HKLM 动作走 helper、其余本地（规格路由契约：HKLM 一律 HelperClient）

use std::sync::{Mutex, OnceLock};

use host_core::error::AppError;
use host_core::ports::{
    HelperSpawnPort, HelperSpec, RegValue, RegistryOps, ServiceCtlPort, ServiceInfo, StartType,
    TaskTogglePort,
};
use rand::RngCore;
use serde_json::{json, Value};
use win_integration::helper as h;

/// token 缓存（与最近一次 spawn 绑定；helper 自退后 spawn 新 token 覆盖）
static TOKEN: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn token_cell() -> &'static Mutex<Option<String>> {
    TOKEN.get_or_init(|| Mutex::new(None))
}

fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 确保 helper 已拉起（token 就绪）；未拉起则 spawn（UAC 弹窗）
pub fn ensure_up(spawner: &dyn HelperSpawnPort) -> Result<(), AppError> {
    if token_cell().lock().unwrap().is_some() {
        return Ok(());
    }
    spawn_new(spawner)
}

fn spawn_new(spawner: &dyn HelperSpawnPort) -> Result<(), AppError> {
    let pid = std::process::id();
    let exe = std::env::current_exe()
        .map_err(|e| AppError::module("SYS_HELPER_006", format!("current_exe 失败: {e}"), None))?;
    let helper_exe = exe
        .parent()
        .ok_or_else(|| AppError::module("SYS_HELPER_006", "主 exe 无父目录", None))?
        .join(h::HELPER_EXE_NAME);
    if !helper_exe.exists() {
        return Err(AppError::module(
            "SYS_HELPER_006",
            format!("Helper 不存在: {}", helper_exe.display()),
            Some("请重新安装应用"),
        ));
    }
    let token = new_token();
    let spec = HelperSpec {
        pipe_name: h::pipe_name_for(pid),
        token: token.clone(),
        parent_pid: pid,
        helper_exe,
        idle_exit_secs: 120,
    };
    spawner.spawn(&spec)?; // UAC 弹窗；取消/失败 → SYS_HELPER_002
    *token_cell().lock().unwrap() = Some(token);
    Ok(())
}

/// 单次调用错误分类：Unreachable = helper 不在（可重 spawn）；Rejected = helper 明确拒绝
enum CallErr {
    Unreachable(AppError),
    Rejected(AppError),
}

/// 单连接：connect → hello(token) → request → close
fn call_once(pipe: &str, token: &str, method: &str, params: &Value) -> Result<Value, CallErr> {
    let hnd = h::pipe_connect(pipe, 5000).map_err(CallErr::Unreachable)?;
    // 握手
    let hello = json!({"jsonrpc":"2.0","id":0,"method":"hello","params":{"token": token}});
    let send = |msg: &Value| h::write_frame(hnd, msg.to_string().as_bytes());
    let read = || -> Result<Value, CallErr> {
        let resp = h::read_frame(hnd).map_err(CallErr::Unreachable)?;
        serde_json::from_slice(&resp)
            .map_err(|e| CallErr::Unreachable(AppError::module("SYS_HELPER_007", format!("响应解析失败: {e}"), None)))
    };
    if let Err(e) = send(&hello) {
        h::close_handle(hnd);
        return Err(CallErr::Unreachable(e));
    }
    let v = match read() {
        Ok(v) => v,
        Err(e) => {
            h::close_handle(hnd);
            return Err(e);
        }
    };
    if v.get("error").is_some() {
        h::close_handle(hnd);
        return Err(CallErr::Rejected(AppError::module(
            "SYS_HELPER_008",
            "Helper 握手被拒绝（token 或调用方校验失败）",
            None,
        )));
    }
    // 请求（响应读出后立即关连接；helper 侧读到 EOF 即清理）
    let req = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
    if let Err(e) = send(&req) {
        h::close_handle(hnd);
        return Err(CallErr::Unreachable(e));
    }
    let resp = h::read_frame(hnd);
    h::close_handle(hnd);
    let resp = resp.map_err(CallErr::Unreachable)?;
    let v: Value = serde_json::from_slice(&resp)
        .map_err(|e| CallErr::Unreachable(AppError::module("SYS_HELPER_007", format!("响应解析失败: {e}"), None)))?;
    if let Some(err) = v.get("error") {
        return Err(CallErr::Rejected(AppError::module(
            "SYS_HELPER_009",
            format!("Helper 执行失败: {}", err["message"].as_str().unwrap_or("?")),
            None,
        )));
    }
    Ok(v["result"].clone())
}

/// 带自愈的调用：helper 不在 → 重新 spawn（UAC）→ 重试一次；明确拒绝则直接报错
fn call_with_recovery(spawner: &dyn HelperSpawnPort, method: &str, params: &Value) -> Result<Value, AppError> {
    let pipe = h::pipe_name_for(std::process::id());
    if let Some(token) = token_cell().lock().unwrap().clone() {
        match call_once(&pipe, &token, method, params) {
            Ok(v) => return Ok(v),
            Err(CallErr::Rejected(e)) => return Err(e),
            Err(CallErr::Unreachable(e)) => {
                tracing::warn!("Helper 调用不可达（将重新拉起）: {e}");
            }
        }
    }
    spawn_new(spawner)?;
    let token = token_cell().lock().unwrap().clone().unwrap_or_default();
    match call_once(&pipe, &token, method, params) {
        Ok(v) => Ok(v),
        Err(CallErr::Rejected(e)) | Err(CallErr::Unreachable(e)) => Err(e),
    }
}

/// 直连调用（不重 spawn——适配器内部用；调用方须先 ensure_up）
fn helper_call(method: &str, params: Value) -> Result<Value, AppError> {
    let token = token_cell()
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| AppError::module("SYS_HELPER_001", "Helper 未初始化", None))?;
    let pipe = h::pipe_name_for(std::process::id());
    match call_once(&pipe, &token, method, &params) {
        Ok(v) => Ok(v),
        Err(CallErr::Rejected(e)) | Err(CallErr::Unreachable(e)) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// helper-backed Port 适配器（实现 host-core trait；sys-core 引擎零感知）
// ---------------------------------------------------------------------------

/// HKLM → helper / 其余 → 本地的路由注册表（apply/rollback 提权数据面）
pub struct RoutingRegistry {
    local: std::sync::Arc<dyn RegistryOps>,
    helper: HelperRegistry,
}

impl RoutingRegistry {
    pub fn new(local: std::sync::Arc<dyn RegistryOps>) -> Self {
        Self { local, helper: HelperRegistry }
    }
}

impl RegistryOps for RoutingRegistry {
    fn read_value(&self, key: &str, value_name: &str) -> Result<(RegValue, bool), AppError> {
        if key.starts_with("HKLM") {
            self.helper.read_value(key, value_name)
        } else {
            self.local.read_value(key, value_name)
        }
    }
    fn write_value(&self, key: &str, value_name: &str, value: &RegValue) -> Result<(), AppError> {
        if key.starts_with("HKLM") {
            self.helper.write_value(key, value_name, value)
        } else {
            self.local.write_value(key, value_name, value)
        }
    }
    fn delete_value(&self, key: &str, value_name: &str) -> Result<(), AppError> {
        if key.starts_with("HKLM") {
            self.helper.delete_value(key, value_name)
        } else {
            self.local.delete_value(key, value_name)
        }
    }
}

/// 注册表（全经 helper——HKLM 读写）
pub struct HelperRegistry;

impl RegistryOps for HelperRegistry {
    fn read_value(&self, key: &str, value_name: &str) -> Result<(RegValue, bool), AppError> {
        let v = helper_call("registry.read", json!({"key": key, "value_name": value_name}))?;
        let existed = v["existed"].as_bool().unwrap_or(false);
        let value: Option<RegValue> = serde_json::from_value(v["value"].clone()).unwrap_or(None);
        Ok((value.unwrap_or(RegValue::Dword(0)), existed))
    }
    fn write_value(&self, key: &str, value_name: &str, value: &RegValue) -> Result<(), AppError> {
        helper_call("registry.write", json!({"key": key, "value_name": value_name, "value": value}))?;
        Ok(())
    }
    fn delete_value(&self, key: &str, value_name: &str) -> Result<(), AppError> {
        helper_call("registry.delete", json!({"key": key, "value_name": value_name}))?;
        Ok(())
    }
}

/// 计划任务启停（经 helper）
pub struct HelperTasks;

impl TaskTogglePort for HelperTasks {
    fn query_enabled(&self, path: &str) -> Result<Option<bool>, AppError> {
        let v = helper_call("task.query_enabled", json!({ "path": path }))?;
        let enabled = v["enabled"].as_bool();
        Ok(match (enabled, v["enabled"].is_null()) {
            (_, true) => None,
            (b, false) => Some(b.unwrap_or(false)),
        })
    }
    fn set_enabled(&self, path: &str, enabled: bool) -> Result<(), AppError> {
        helper_call("task.set_enabled", json!({ "path": path, "enabled": enabled }))?;
        Ok(())
    }
}

/// 服务控制（经 helper）
pub struct HelperServices;

impl ServiceCtlPort for HelperServices {
    fn query(&self, name: &str) -> Result<ServiceInfo, AppError> {
        let v = helper_call("service.query", json!({ "name": name }))?;
        serde_json::from_value(v).map_err(|e| AppError::module("SYS_HELPER_007", format!("ServiceInfo 解析失败: {e}"), None))
    }
    fn set_start_type(&self, name: &str, st: StartType) -> Result<(), AppError> {
        helper_call("service.set_start", json!({ "name": name, "start_type": st }))?;
        Ok(())
    }
}

/// 备份条目是否需要提权数据面（HKLM 注册表 / 服务 / 计划任务）
pub fn backup_needs_elevation(items: &[sys_core::winops::BackupItem]) -> bool {
    use sys_core::winops::BackupItem;
    items.iter().any(|b| match b {
        BackupItem::Registry(rb) => rb.key.starts_with("HKLM"),
        BackupItem::Service { .. } | BackupItem::Task { .. } => true,
    })
}
