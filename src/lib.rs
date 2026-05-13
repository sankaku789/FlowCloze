//! FlowClozeのコアライブラリ。

pub mod gemini;
pub mod models;
pub mod parser;
pub mod prompt;
pub mod validation;
pub mod yaml;

pub use gemini::{GeminiClient, GeminiError};
pub use models::{QBlock, Target, ALLOWED_TARGET_TYPES};
pub use parser::{parse_markdown, parse_qblocks, MarkdownParseError};
pub use prompt::build_generation_prompt;
pub use validation::{validate_generated_yaml, ValidationError, ValidationReport};
pub use yaml::{to_intermediate_yaml, IntermediateDocument, IntermediateMeta, IntermediateQBlock};
