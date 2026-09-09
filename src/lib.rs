//! FlowCloze CLIが使う解析・生成支援・検証・出力のコア機能．

pub mod application;
pub mod config;
pub mod core;
pub mod generation;
pub mod output;
pub mod providers;
pub mod runtime;

pub use application::generate as orchestration;
pub use config::quota;
pub use core::composition as compose;
pub use core::model as models;
pub use core::parser;
pub use core::scaffold;
pub use core::task;
pub use core::validation;
pub use generation::executor;
pub use generation::planner;
pub use generation::prompt;
pub use output::csv;
pub use output::json;
pub use output::pdf;
pub use runtime::http;
pub use runtime::labeled_progress;
pub use runtime::observability;
pub use runtime::progress;
pub use runtime::rate_limit;

pub use application::plan::{
    plan_markdown, PlanBatchSummary, PlanIdentitySummary, PlanMarkdownOptions, PlanMarkdownOutcome,
    PlanQBlockSummary,
};
pub use application::{GenerateUseCase, PlanUseCase};
pub use compose::{
    parse_compose_output, ComposeBatchOutput, ComposeBatchRequest, ComposeError, ComposeMetadata,
    ComposeTask, ComposedItem, IdentityComposer, QuestionComposer, WritingStyle,
};
pub use config::{BatchPolicyName, CliOverrides, FallbackPolicy, GenerationConfig};
pub use csv::to_ankilot_csv;
pub use executor::{ExecutionContext, ExecutionError, Executor};
pub use json::{to_intermediate_json, IntermediateDocument, IntermediateMeta, IntermediateQBlock};
pub use labeled_progress::LabeledProgressSink;
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
pub use prompt::build_compose_request_prompt;
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
