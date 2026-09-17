//! notes-core：笔记与知识管理（docs/impl/06 N1–N5）。
//!
//! - N1 笔记库：磁盘 .md 为真相源，notes.db 仅存可重建索引
//! - N2 双链：`[[目标|别名]]` 索引 + 反链 + 重命名全库引用改写
//! - N3 画布：`.nforge-canvas.json`（目录级，损坏仅丢画布）
//! - N4 复习：SM-2 简化版（cards 表复用 notes.db）
//! - N5 多存储：文件 CRUD 全走 file-core StorageDriver（v1 local 驱动）

pub mod canvas;
pub mod error;
pub mod frontmatter;
pub mod index;
pub mod library;
pub mod model;
pub mod module;
pub mod review;

pub use error::{NoteError, Result};
pub use library::NoteLibrary;
pub use module::NotesModule;
