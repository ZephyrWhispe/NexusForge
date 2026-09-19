//! Windows Hello 校验门（docs/impl/05 V4 / D-24）：UserConsentVerifier。
//!
//! HelloPort 契约同步：调用方（vault 命令层）已在 spawn_blocking 线程，
//! 与 WinOcr 同一模式 WinRT `.get()` 阻塞等待，无 STA 死锁风险。
//! 注意：DPAPI 无法"由 Hello 派生"（D-24 决策①偏差记录）——本端口只是
//! 前置校验门，密钥账户域绑定由 CryptoPort(DPAPI) + DEK 自证 verifier 完成。

use windows::core::HSTRING;
use windows::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};

use host_core::error::AppError;
use host_core::ports::HelloPort;

use host_core::util::app_err as err;

pub struct WindowsHello;

impl WindowsHello {
    pub fn new() -> Self {
        Self
    }

    /// 本机是否具备可用 Hello（PIN/指纹/面部）；供 UI 决定是否展示免密开关
    pub fn available() -> bool {
        UserConsentVerifier::CheckAvailabilityAsync()
            .and_then(|op| op.get())
            .map(|a| a == UserConsentVerifierAvailability::Available)
            .unwrap_or(false)
    }
}

impl Default for WindowsHello {
    fn default() -> Self {
        Self::new()
    }
}

impl HelloPort for WindowsHello {
    /// 弹出系统 Hello 校验窗口；任何非 Verified 结果（取消/指纹失败/超时）都算失败
    fn verify(&self, reason: &str) -> Result<(), AppError> {
        let availability = UserConsentVerifier::CheckAvailabilityAsync()
            .and_then(|op| op.get())
            .map_err(|e| err("HELLO_STATE_001", format!("Hello 可用性查询失败: {e}")))?;
        if availability != UserConsentVerifierAvailability::Available {
            return Err(err(
                "HELLO_STATE_002",
                format!("Windows Hello 不可用（{availability:?}）"),
            ));
        }
        let result = UserConsentVerifier::RequestVerificationAsync(&HSTRING::from(reason))
            .and_then(|op| op.get())
            .map_err(|e| err("HELLO_VERIFY_001", format!("Hello 校验调用失败: {e}")))?;
        if result == UserConsentVerificationResult::Verified {
            Ok(())
        } else {
            Err(err(
                "HELLO_VERIFY_002",
                format!("Hello 校验未通过（{result:?}）"),
            ))
        }
    }
}
