//! OCR 识别模块（docs/impl/04）

pub mod engine;
pub mod module;
pub mod pipeline;
pub mod tesseract;
pub mod types;

pub use engine::{resolve_langs, EngineRegistry, OcrEngine, WinOcrEngine};
pub use module::OcrModule;
pub use tesseract::{CmdOutput, CommandRunner, SystemRunner, TesseractEngine, TesseractSettings};
