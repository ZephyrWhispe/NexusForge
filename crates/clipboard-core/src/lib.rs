//! 剪切板中枢（docs/impl/02）
//!
//! 实施步骤：C1 类型 → C2 存储 → C3 捕获管线 → C4 敏感保护 → C5 分类器
//! → C6 查询 → C7 IPC（命令在 src-tauri）→ C8 前端 → C9 清理。
//! 当前：C1–C6 完成；图片捕获与快速面板在下一迭代。

pub mod classifier;
pub mod module;
pub mod pipeline;
pub mod secrets;
pub mod store;
pub mod types;
