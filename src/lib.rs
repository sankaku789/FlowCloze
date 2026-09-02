//! FlowCloze CLIが使う解析・生成支援・検証・出力のコア機能．

pub mod compose;
pub mod config;
pub mod csv;
pub mod gemini;
pub mod http;
pub mod json;
pub mod local_openai;
pub mod models;
pub mod observability;
pub mod orchestration;
pub mod parser;
pub mod pdf;
pub mod planner;
pub mod progress;
pub mod prompt;
pub mod providers;
pub mod scaffold;
pub mod validation;

pub use compose::{
    parse_compose_output, ComposeBatchOutput, ComposeBatchRequest, ComposeError, ComposeMetadata,
    ComposeTask, ComposedItem, IdentityComposer, QuestionComposer, WritingStyle,
};
pub use config::{
    BatchPolicyName, CliOverrides, FallbackPolicy, GenerationConfig, Provider, RewritePolicy,
};
pub use csv::to_ankilot_csv;
pub use gemini::{GeminiAdapter, StructuredOutputMode};
pub use gemini::{GeminiApi, GeminiClient, GeminiError};
pub use json::{to_intermediate_json, IntermediateDocument, IntermediateMeta, IntermediateQBlock};
pub use local_openai::{
    local_openai_url_candidates, try_local_openai_candidates, LocalOpenAiClient, LocalOpenAiError,
    OpenAiCompatibleAdapter, OpenAiCompatiblePool,
};
pub use models::{QBlock, Target, ALLOWED_TARGET_TYPES};
pub use observability::{
    fnv1a_64, ComposeEvent, ComposeEventKind, EventSink, JsonLinesEventSink, MetricsSummary,
    NoopEventSink, RunContext,
};
pub use orchestration::{
    generate_markdown_with_composer, generate_markdown_with_composer_observed,
    generate_markdown_with_composer_observed_with_progress,
    generate_markdown_with_composer_with_progress, GenerateMarkdownError, GenerateMarkdownOptions,
    GenerateMarkdownOutcome,
};
pub use parser::{parse_markdown, parse_qblocks, MarkdownParseError};
pub use pdf::{compile_pdf, default_pdf_output_path, PdfError, PdfOptions};
pub use planner::{
    compose_with_question_composer, prepare_compose_plan, BatchPolicy, ComposeExecutionPolicy,
    ComposePlanError, PreparedComposePlan,
};
pub use progress::{
    FailureClass, NoopProgressSink, PlainProgressSink, ProgressEvent, ProgressSink, ProgressStage,
    RetryResult,
};
pub use prompt::{build_compose_request_prompt, build_generation_prompt};
pub use validation::{
    validate_generated_document, validate_generated_json, FixedField, GeneratedDocument,
    ValidationError, ValidationReport,
};
