//! OCR 识别模块（docs/impl/04）

pub mod engine;
pub mod module;
pub mod pipeline;
pub mod types;

pub use engine::{EngineRegistry, OcrEngine, WinOcrEngine};
pub use module::OcrModule;
