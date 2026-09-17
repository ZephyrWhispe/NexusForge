//! desktop-core 桌面效率（docs/impl/05 D1–D4）
//!
//! - D1 快速启动器：开始菜单(.lnk) + PATH 可执行 + 内置动作索引；呼出经全局快捷键
//! - D2 模糊匹配打分：0.5*前缀命中 + 0.3*子序列连续度 + 0.2*频次（30 天半衰期衰减）
//! - D3 桌面格子整理：v1 安全实现 = 普通文件按扩展名归类生成分类文件夹（lnk/目录不动），
//!   移动映射落 manifest 供"还原"
//! - D4 待办与随记：`{appData}/db/desktop.db`（WAL）；#标签 + 明天/周几提醒解析；
//!   提醒由模块后台线程轮询到期发事件（Task Scheduler 注册列为后续）
//!
//! 风险标注（docs/impl/05）：启动器禁止在 UI 线程同步枚举（build 由调用方 spawn_blocking）；
//! 动作执行 ShellExecuteW（win-integration 收敛）。

pub mod error;
pub mod index;
pub mod module;
pub mod note;
pub mod score;
pub mod tidy;

pub use error::{DesktopError, Result};
pub use index::{IndexItem, ItemKind, LauncherIndex, LauncherHit};
pub use module::DesktopModule;
pub use note::{Note, NoteStore};
pub use score::FuzzyHit;
pub use tidy::{DesktopItem, TidyPlan, TidyPlanner, CATEGORY_NAMES};
