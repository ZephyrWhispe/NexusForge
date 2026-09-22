//! OCR 识别模块（docs/impl/04）

pub mod engine;
pub mod export;
pub mod module;
pub mod pipeline;
pub mod tesseract;
pub mod translate;
pub mod types;

pub use engine::{resolve_langs, EngineRegistry, OcrEngine, WinOcrEngine};
pub use export::{export_file_name, write_export, EXPORT_FORMATS};
pub use module::OcrModule;
pub use tesseract::{CmdOutput, CommandRunner, SystemRunner, TesseractEngine, TesseractSettings};
pub use translate::{
    NullTranslateProvider, TranslateProvider, TranslateRegistry, NULL_PROVIDER_ID,
};
