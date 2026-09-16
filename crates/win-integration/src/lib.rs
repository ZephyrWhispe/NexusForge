//! Windows 原生能力适配层（win-integration）
//!
//! 全仓库唯一允许依赖 `windows` crate 的位置（docs/DESIGN.md 优化点 O2）。
//! 各 Port trait 的 Windows 实现按 docs/impl 文档逐步落地：
//! - 剪贴板监听/回写：clipboard.rs（docs/impl/02 C3）
//! - DPAPI 本机加密：dpapi.rs（docs/impl/02 C4）
//! - 系统强调色：accent.rs（U1-3）
//! - GDI 屏幕捕获：capture.rs（docs/impl/03 P2，v1 主路径）
//! - Windows.Media.Ocr：ocr.rs（docs/impl/04 O2）
//! - Windows.Graphics.Capture（录屏，03 P7）：后续迭代

pub mod accent;
pub mod capture;
pub mod clipboard;
pub mod dpapi;
pub mod hotkey;
pub mod ocr;
