//! editor-core 文本与 PDF（docs/impl/06 E1–E4）
//!
//! - E1 文件会话：打开/脏标记/编码检测（BOM → UTF-8 校验 → chardetng 猜测）/EOL 统一
//! - E4 PDF：合并/拆分/压缩/文字水印（纯 Rust lopdf；偏离文档 qpdf/mutool sidecar
//!   —— Windows 无官方稳定独立分发且免网络下载，见 pdf.rs 顶部注释）
//! - E2 大文件降级由前端执行（>5MB 关语法高亮、>50MB 只读查看器），阈值由后端
//!   open 返回的 size 字段驱动；E3 Markdown 预览纯前端

pub mod error;
pub mod module;
pub mod pdf;
pub mod session;

pub use error::{EditorError, Result};
pub use module::EditorModule;
pub use pdf::{PdfInfo, PdfOpResult};
pub use session::{EditorSessions, Eol, EncodingKind, SessionInfo};
