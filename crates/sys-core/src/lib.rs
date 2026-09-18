//! sys-core：系统管理（docs/impl/06 SY1–SY4）。
//!
//! - SY1 包管理器抽象：winget/scoop/choco 探测与适配（--disable-interactivity + 逐行输出回调）
//! - SY2 已装清单合并视图：多源去重，winget 优先
//! - SY3 系统清理：内置目录清单 + 24h 白名单 + 回收站/直删双模式
//! - SY4 资源监控：PDH 1s 采样 + 300 点环形缓冲 + sys.metrics 事件
//!
//! WinOps Tweak 引擎深化（BAVR/catalog/Helper）见 docs/impl/08-winops.md，独立里程碑。

pub mod clean;
pub mod error;
pub mod metrics;
pub mod module;
pub mod pkg;
pub mod winops;

pub use error::{Result, SysError};
pub use metrics::{MetricsBuffer, MetricsPoint};
pub use module::SysModule;
pub use pkg::{PkgEntry, PkgManager};
