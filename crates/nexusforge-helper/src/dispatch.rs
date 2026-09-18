//! helper 方法白名单（docs/impl/08 §4.3 v1 集：registry / service / task 窄面）。
//!
//! 数据面 = win-integration 本地实现（helper 已是提权进程，HKLM/服务/系统任务直执行）。
//! exec/dns/hosts/maintenance/appx 等 W4+ 方法族按文档白名单逐步扩充，未收录一律拒绝。

use host_core::ports::{RegValue, RegistryOps, ServiceCtlPort, StartType, TaskTogglePort};
use serde_json::{json, Value};
use win_integration::registry::RegistryOpsWin;
use win_integration::service::ServiceOps;
use win_integration::taskschd::TaskSchdOps;

/// 数据面（构造一次，进程内复用）
pub struct Ops {
    registry: RegistryOpsWin,
    services: ServiceOps,
    tasks: TaskSchdOps,
}

impl Ops {
    pub fn new() -> Self {
        Self { registry: RegistryOpsWin::new(), services: ServiceOps::new(), tasks: TaskSchdOps::new() }
    }
}

impl Default for Ops {
    fn default() -> Self {
        Self::new()
    }
}

fn req_str<'a>(p: &'a Value, k: &str) -> Result<&'a str, String> {
    p[k].as_str().ok_or_else(|| format!("缺少参数 {k}"))
}

/// 白名单分发（未知方法一律拒绝；错误信息回传 error.message）
pub fn handle_request(ops: &Ops, method: &str, p: Value) -> Result<Value, String> {
    match method {
        "helper.ping" => Ok(json!({ "version": env!("CARGO_PKG_VERSION") })),
        "registry.read" => {
            let (v, existed) = ops
                .registry
                .read_value(req_str(&p, "key")?, req_str(&p, "value_name")?)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "existed": existed, "value": if existed { Some(v) } else { None } }))
        }
        "registry.write" => {
            let value: RegValue =
                serde_json::from_value(p["value"].clone()).map_err(|e| format!("value 解析失败: {e}"))?;
            ops.registry
                .write_value(req_str(&p, "key")?, req_str(&p, "value_name")?, &value)
                .map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "registry.delete" => {
            ops.registry
                .delete_value(req_str(&p, "key")?, req_str(&p, "value_name")?)
                .map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "service.query" => {
            let info = ops.services.query(req_str(&p, "name")?).map_err(|e| e.to_string())?;
            serde_json::to_value(info).map_err(|e| e.to_string())
        }
        "service.set_start" => {
            let st: StartType =
                serde_json::from_value(p["start_type"].clone()).map_err(|e| format!("start_type 解析失败: {e}"))?;
            ops.services.set_start_type(req_str(&p, "name")?, st).map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "task.query_enabled" => {
            let enabled = ops.tasks.query_enabled(req_str(&p, "path")?).map_err(|e| e.to_string())?;
            Ok(json!({ "enabled": enabled }))
        }
        "task.set_enabled" => {
            let enabled = p["enabled"].as_bool().ok_or("缺少参数 enabled")?;
            ops.tasks.set_enabled(req_str(&p, "path")?, enabled).map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        other => Err(format!("未知方法（白名单外）: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const K: &str = "HKCU\\Software\\NexusForgeHelperTest";

    #[test]
    fn unknown_method_rejected() {
        let ops = Ops::new();
        assert!(handle_request(&ops, "exec.dism", json!({})).is_err());
        assert!(handle_request(&ops, "registry.drop_table", json!({})).is_err());
        assert!(handle_request(&ops, "", json!({})).is_err());
    }

    #[test]
    fn ping_whitelisted() {
        let ops = Ops::new();
        let r = handle_request(&ops, "helper.ping", json!({})).unwrap();
        assert!(r["version"].is_string());
    }

    /// HKCU 测试键读写删回环（helper 数据面 = win-integration 本地实现）
    #[test]
    fn registry_whitelist_roundtrip() {
        let ops = Ops::new();
        handle_request(&ops, "registry.write", json!({"key": K, "value_name": "v", "value": {"dword": 7}})).unwrap();
        let r = handle_request(&ops, "registry.read", json!({"key": K, "value_name": "v"})).unwrap();
        assert_eq!(r["existed"], true);
        assert_eq!(r["value"]["dword"], 7);
        handle_request(&ops, "registry.delete", json!({"key": K, "value_name": "v"})).unwrap();
        let r = handle_request(&ops, "registry.read", json!({"key": K, "value_name": "v"})).unwrap();
        assert_eq!(r["existed"], false);
    }

    /// service.set_start 参数解析（snake_case 三态）
    #[test]
    fn service_start_type_parse() {
        let st: StartType = serde_json::from_value(json!("disabled")).unwrap();
        assert_eq!(st, StartType::Disabled);
        assert!(serde_json::from_value::<StartType>(json!("always")).is_err());
    }
}
