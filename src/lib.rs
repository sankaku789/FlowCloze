//! FlowCloze CLIが使う解析・生成支援・検証・出力のコア機能．

pub mod application;
pub mod compose;
pub mod config;
pub mod csv;
pub mod executor;
#[cfg(feature = "gemini-native")]
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
pub mod quota;
pub mod rate_limit;
pub mod scaffold;
pub mod task;
pub mod validation;

pub use application::{GenerateUseCase, PlanUseCase};
pub use compose::{
    parse_compose_output, ComposeBatchOutput, ComposeBatchRequest, ComposeError, ComposeMetadata,
    ComposeTask, ComposedItem, IdentityComposer, QuestionComposer, WritingStyle,
};
pub use config::{
    BatchPolicyName, CliOverrides, FallbackPolicy, GenerationConfig, Provider, RewritePolicy,
};
pub use csv::to_ankilot_csv;
pub use executor::{ExecutionContext, ExecutionError, Executor};
#[cfg(feature = "gemini-native")]
pub use gemini::GeminiAdapter;
pub use json::{to_intermediate_json, IntermediateDocument, IntermediateMeta, IntermediateQBlock};
pub use models::{QBlock, Target, ALLOWED_TARGET_TYPES};
pub use observability::{
    fnv1a_64, ComposeEvent, ComposeEventKind, EventSink, JsonLinesEventSink, MetricsSummary,
    NoopEventSink, RunContext,
};
pub use orchestration::{
    generate_markdown_with_composer, generate_markdown_with_composer_observed,
    generate_markdown_with_composer_observed_with_progress,
    generate_markdown_with_composer_with_progress, plan_markdown, GenerateMarkdownError,
    GenerateMarkdownOptions, GenerateMarkdownOutcome, PlanBatchSummary, PlanIdentitySummary,
    PlanMarkdownOptions, PlanMarkdownOutcome, PlanQBlockSummary,
};
pub use parser::{
    parse_markdown, parse_markdown_located, parse_qblocks, MarkdownParseError, ParsedDocument,
};
pub use pdf::{compile_pdf, default_pdf_output_path, PdfError, PdfOptions};
pub use planner::{
    compose_with_question_composer, prepare_compose_plan, BatchPolicy, ComposeExecutionPolicy,
    ComposePlanError, PreparedComposePlan,
};
pub use progress::{
    FailureClass, NoopProgressSink, PlainProgressSink, ProgressEvent, ProgressSink, ProgressStage,
    RetryCause, RetryResult,
};
pub use prompt::{build_compose_request_prompt, build_generation_prompt};
pub use providers::adapter_factory::build_adapter;
pub use providers::builtins::{builtin_models, builtin_providers, DEFAULT_MODEL};
pub use providers::capability::StructuredOutputMode;
pub use providers::catalog::{AuthRequirement, ProviderCatalog, ProviderDefinition, ProviderError};
pub use providers::model_registry::{ModelError, ModelProfile, ModelRegistry, ResolvedModel};
pub use providers::openai_compatible::{
    local_openai_url_candidates, try_local_openai_candidates, OpenAiAuth, OpenAiCompatibleAdapter,
    OpenAiCompatiblePool, OpenAiEndpointConfig,
};
pub use quota::QuotaProfile;
pub use rate_limit::RateLimitKind;
pub use task::{build_generation_tasks, GenerationTask, TaskBuildError};
pub use validation::{
    validate_generated_document, validate_generated_json, FixedField, GeneratedDocument,
    ValidationError, ValidationReport,
};
