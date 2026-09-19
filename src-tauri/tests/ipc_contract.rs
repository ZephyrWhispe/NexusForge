//! S6 IPC 契约回归（批次 2 审查项）：宿主命令面的纯函数层与序列化契约。
//!
//! 只锁三件事：DTO 的 JSON 键形状（前端按此解析）、命令包装函数对 core 语义的
//! 透传（密码生成 / TOTP）、错误码字符串（前端 parseAppError 的分派键）。

use std::sync::Arc;

use host_core::error::ModuleError;
use host_core::events::EventBus;
use host_core::module::{
    priority_of, Module, ModuleContext, ModuleInfo, ModuleState, ModuleStateCell,
};
use host_core::registry::ModuleRegistry;
use nexusforge_lib::commands::{
    build_modules_status, host_system_accent, vault_generate_password, vault_totp_now,
};
use vault_core::PasswordPolicy;

struct MockModule {
    state: ModuleStateCell,
}

impl MockModule {
    fn new() -> Self {
        Self {
            state: ModuleStateCell::new(),
        }
    }
}

impl Module for MockModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "ipcmock",
            name: "契约模拟",
            version: "0.1.0",
            icon: None,
            priority: priority_of("ipcmock"),
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
    fn set_status(&self, s: ModuleState) {
        self.state.set(s);
    }
}

fn registry_with_mock() -> Arc<ModuleRegistry> {
    let registry = Arc::new(ModuleRegistry::new(Arc::new(EventBus::new())));
    registry.register(Arc::new(MockModule::new())).unwrap();
    registry
}

#[test]
fn modules_status_json_shape_is_stable() {
    let registry = registry_with_mock();
    let dtos = build_modules_status(&registry);
    assert_eq!(dtos.len(), 1);
    let json = serde_json::to_value(&dtos).unwrap();
    let m = &json[0];
    // 前端消费键集合 {id,name,version,priority,state}；ModuleState 按 serde 变体名上线
    assert_eq!(m["id"], "ipcmock");
    assert_eq!(m["name"], "契约模拟");
    assert_eq!(m["version"], "0.1.0");
    assert_eq!(m["priority"], 50); // 未登记 id → PRIORITY_DEFAULT
    assert_eq!(m["state"], "Uninitialized");
    assert_eq!(m.as_object().unwrap().len(), 5);
}

#[test]
fn modules_status_reads_the_same_cell_as_module_status() {
    // D-16 单一状态源：DTO 装配必须反映模块 cell 的变化，而非注册表私有快照
    let registry = registry_with_mock();
    let module = registry.get("ipcmock").unwrap();
    module.set_status(ModuleState::Running);
    let dtos = build_modules_status(&registry);
    assert_eq!(dtos[0].state, ModuleState::Running);
}

#[test]
fn generate_password_contract() {
    let pw = vault_generate_password(PasswordPolicy {
        length: 24,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(pw.len(), 24);
    const VALID: &str =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()-_=+[]{};:,.<>?/";
    assert!(pw.chars().all(|c| VALID.contains(c)));
    // 每选中类至少 1 个（生成器保证覆盖）
    assert!(pw.chars().any(|c| c.is_ascii_uppercase()));
    assert!(pw.chars().any(|c| c.is_ascii_lowercase()));
    assert!(pw.chars().any(|c| c.is_ascii_digit()));
    assert!(pw.chars().any(|c| "!@#$%^&*()-_=+[]{};:,.<>?/".contains(c)));
}

#[test]
fn generate_password_avoid_ambiguous_excludes_confusable_chars() {
    const AMBIGUOUS: &str = "0O1lI|'`\"";
    let pw = vault_generate_password(PasswordPolicy {
        length: 64,
        avoid_ambiguous: true,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(pw.len(), 64);
    assert!(!pw.chars().any(|c| AMBIGUOUS.contains(c)), "{pw}");
}

#[test]
fn generate_password_policy_errors_use_vault_gen_001() {
    for policy in [
        // 无任何字符类
        PasswordPolicy {
            length: 12,
            upper: false,
            lower: false,
            digits: false,
            symbols: false,
            avoid_ambiguous: false,
        },
        // 长度小于字符类数
        PasswordPolicy {
            length: 3,
            ..Default::default()
        },
    ] {
        let e = vault_generate_password(policy).unwrap_err();
        assert_eq!(e.code(), "VAULT_GEN_001");
    }
}

#[test]
fn totp_now_returns_six_digit_code_and_step_remaining() {
    // RFC 6238 测试密钥
    let (code, remaining) = vault_totp_now("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ".to_string()).unwrap();
    assert_eq!(code.len(), 6);
    assert!(code.chars().all(|c| c.is_ascii_digit()), "{code}");
    assert!((1..=30).contains(&remaining));

    let e = vault_totp_now("not base32 !!!".to_string()).unwrap_err();
    assert_eq!(e.code(), "VAULT_TOTP_001");
}

#[test]
fn system_accent_returns_hex_or_typed_error() {
    // 真机数据面：无强调色主题 / Server SKU 可能失败，两种结果都按契约放行
    match host_system_accent() {
        Ok(hex) => {
            assert_eq!(hex.len(), 7);
            assert!(hex.starts_with('#'));
            assert!(hex[1..].chars().all(|c| c.is_ascii_hexdigit()));
        }
        Err(e) => assert_eq!(e.code(), "HOST_SYSTEM_001"),
    }
}
