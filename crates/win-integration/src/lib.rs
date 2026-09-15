//! Windows 原生能力适配层（win-integration）
//!
//! 全仓库唯一允许依赖 `windows` crate 的位置（docs/DESIGN.md 优化点 O2）。
//! 各 Port trait 的 Windows 实现按 docs/impl 文档逐步落地：
//! 剪贴板监听（02 C3）、Graphics.Capture（03 P2）、Windows.Media.Ocr（04 O2）等。
//!
//! 已实现：系统强调色（U1-3）。

pub mod accent;
