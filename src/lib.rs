//! FlowCloze CLIが使う解析・生成支援・検証・出力のコア機能．

pub mod csv;
pub mod gemini;
pub mod json;
pub mod models;
pub mod parser;
pub mod pdf;
pub mod prompt;
pub mod validation;

pub use csv::to_ankilot_csv;
pub use gemini::{GeminiClient, GeminiError};
pub use json::{to_intermediate_json, IntermediateDocument, IntermediateMeta, IntermediateQBlock};
pub use models::{QBlock, Target, ALLOWED_TARGET_TYPES};
pub use parser::{parse_markdown, parse_qblocks, MarkdownParseError};
pub use pdf::{compile_pdf, default_pdf_output_path, PdfError, PdfOptions};
pub use prompt::build_generation_prompt;
pub use validation::{
    validate_generated_document, validate_generated_json, GeneratedDocument, ValidationError,
    ValidationReport,
};
