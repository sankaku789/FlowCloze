//! 位置情報付き解析からcomposeまでを束ねる公開生成入口。

use std::collections::HashMap;
use std::ops::Range;

use crate::compose::{IdentityComposer, QuestionComposer};
use crate::config::FallbackPolicy;
use crate::json::IntermediateDocument;
use crate::observability::{ComposeEvent, ComposeEventKind, EventSink, NoopEventSink, RunContext};
use crate::parser::{parse_markdown_located, MarkdownParseError, ParsedDocument};
use crate::planner::{ComposeExecutionPolicy, ComposePlanError, FailureReason};
use crate::progress::{FailureClass, NoopProgressSink, ProgressEvent, ProgressSink};
use crate::quota::QuotaProfile;
use crate::scaffold::{ScaffoldDocument, ScaffoldTask};
use crate::validation::{
    validate_generated_documents_with_leakage_baselines, GeneratedDocument,
};

/// Markdown生成入口の設定。出力JSONにはこの情報を混ぜない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerateMarkdownOptions {
    pub source: String,
    pub policy: ComposeExecutionPolicy,
    pub fallback: FallbackPolicy,
    /// provider quotaに応じて初回batchと送信速度を調整する。
    pub quota: Option<QuotaProfile>,
    /// provider taskがある時だけrequestへ渡す追加制約。
    pub extra_constraints: Vec<String>,
}

impl GenerateMarkdownOptions {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            policy: ComposeExecutionPolicy::default(),
            fallback: FallbackPolicy::Error,
            quota: None,
            extra_constraints: Vec::new(),
        }
    }
}

/// Markdownから生成された文書。追跡情報はGeneratedDocumentのwire形式から分離する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerateMarkdownOutcome {
    pub document: GeneratedDocument,
    /// JSON wire形式へ入れない、fallbackしたtaskだけの運用情報。
    pub fallback_summary: Vec<FallbackSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackSummary {
    pub id: String,
    pub reason: FallbackReason,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    Transport,
    Content,
}
/// located生成経路で起きる、provider呼び出し前後の失敗。
#[derive(Debug)]
pub enum GenerateMarkdownError {
    Markdown(MarkdownParseError),
    Compose(ComposePlanError),
}

impl std::fmt::Display for GenerateMarkdownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Markdown(error) => write!(f, "markdown parse error: {error}"),
            Self::Compose(error) => write!(f, "compose error: {error}"),
        }
    }
}

impl std::error::Error for GenerateMarkdownError {}

/// 位置情報を使った安全な標準生成経路。
pub fn generate_markdown_with_composer(
    markdown: &str,
    options: GenerateMarkdownOptions,
    composer: &dyn QuestionComposer,
) -> Result<GenerateMarkdownOutcome, GenerateMarkdownError> {
    let context = RunContext::new();
    let sink = NoopEventSink;
    let progress = NoopProgressSink;
    generate_markdown_with_composer_observed_with_progress(
        markdown, options, composer, &context, &sink, &progress,
    )
}

/// 人間向け進捗を注入できる生成入口。既存の観測JSONとは独立している。
pub fn generate_markdown_with_composer_with_progress(
    markdown: &str,
    options: GenerateMarkdownOptions,
    composer: &dyn QuestionComposer,
    progress: &dyn ProgressSink,
) -> Result<GenerateMarkdownOutcome, GenerateMarkdownError> {
    let context = RunContext::new();
    let events = NoopEventSink;
    generate_markdown_with_composer_observed_with_progress(
        markdown, options, composer, &context, &events, progress,
    )
}

/// 位置情報を使った安全な標準生成経路へ、本文なしの観測hookを加えた版。
pub fn generate_markdown_with_composer_observed(
    markdown: &str,
    options: GenerateMarkdownOptions,
    composer: &dyn QuestionComposer,
    context: &RunContext,
    sink: &dyn EventSink,
) -> Result<GenerateMarkdownOutcome, GenerateMarkdownError> {
    let progress = NoopProgressSink;
    generate_markdown_with_composer_observed_with_progress(
        markdown, options, composer, context, sink, &progress,
    )
}

/// JSON Lines観測と人間向け進捗を同時に注入する入口。
pub fn generate_markdown_with_composer_observed_with_progress(
    markdown: &str,
    options: GenerateMarkdownOptions,
    composer: &dyn QuestionComposer,
    context: &RunContext,
    sink: &dyn EventSink,
    progress: &dyn ProgressSink,
) -> Result<GenerateMarkdownOutcome, GenerateMarkdownError> {
    let parsed = match parse_markdown_located(markdown) {
        Ok(parsed) => parsed,
        Err(error) => {
            progress.emit(ProgressEvent::Failed {
                stage: crate::progress::ProgressStage::Parse,
                class: FailureClass::InvalidInput,
            });
            return Err(GenerateMarkdownError::Markdown(error));
        }
    };
    let qblocks = parsed
        .qblocks
        .iter()
        .map(|qblock| qblock.qblock.clone())
        .collect::<Vec<_>>();
    let intermediate = IntermediateDocument::from_qblocks(options.source, &qblocks);
    let (scaffold, leakage_baselines) = match build_blank_scaffold(markdown, &parsed) {
        Ok(value) => value,
        Err(error) => {
            progress.emit(ProgressEvent::Failed {
                stage: crate::progress::ProgressStage::Parse,
                class: FailureClass::InvalidInput,
            });
            return Err(GenerateMarkdownError::Markdown(error));
        }
    };
    progress.emit(ProgressEvent::Parsed {
        tasks: scaffold.tasks.len(),
    });
    let task_indexes = (0..scaffold.tasks.len()).collect::<Vec<_>>();
    let rewrite_plan = match prepare_selected_plan(
        &scaffold,
        &task_indexes,
        options.policy,
        options.quota.as_ref(),
    ) {
        Ok(count) => count,
        Err(error) => {
            progress.emit(ProgressEvent::Failed {
                stage: crate::progress::ProgressStage::Plan,
                class: failure_class_for_plan(&error),
            });
            return Err(GenerateMarkdownError::Compose(error));
        }
    };
    let identity_batches = 0;
    let rewrite_batches = rewrite_plan.batch_count();
    let initial_batches = identity_batches + rewrite_batches;
    progress.emit(ProgressEvent::Planned {
        initial_batches,
        provider_tasks: task_indexes.len(),
        identity_tasks: 0,
    });
    let mut questions = Vec::new();
    let mut fallback_summary = Vec::new();
    if !task_indexes.is_empty() {
        let batch_progress = BatchProgressSink::new(progress, identity_batches, initial_batches);
        match compose_indexes(
            &intermediate,
            &scaffold,
            &task_indexes,
            options.policy,
            composer,
            context,
            sink,
            &batch_progress,
            Some(&rewrite_plan),
            &options.extra_constraints,
            &leakage_baselines,
        ) {
            Ok(document) => questions.extend(document.questions),
            Err(error)
                if options.fallback == FallbackPolicy::Draft
                    && matches!(error.as_public(), ComposePlanError::Partial { .. }) =>
            {
                let (public_error, _, fallback_causes) = error.into_parts();
                let ComposePlanError::Partial {
                    document,
                    failed_ids,
                    failed_reasons,
                } = public_error
                else {
                    unreachable!("guard ensures a partial error")
                };
                questions.extend(document.questions);
                for ((id, failure_reason), terminal_cause) in failed_ids
                    .into_iter()
                    .zip(failed_reasons)
                    .zip(fallback_causes)
                {
                    let index = task_indexes
                        .iter()
                        .copied()
                        .find(|index| scaffold.tasks[*index].id == id)
                        .expect("planner failure must refer to a selected task");
                    let draft = compose_indexes(
                        &intermediate,
                        &scaffold,
                        &[index],
                        options.policy,
                        &IdentityComposer,
                        context,
                        sink,
                        &NoopProgressSink,
                        None,
                        &[],
                        &leakage_baselines,
                    )
                    .map_err(|error| GenerateMarkdownError::Compose(error.into_public()))?;
                    fallback_summary.push(FallbackSummary {
                        id: scaffold.tasks[index].id.clone(),
                        reason: match failure_reason {
                            FailureReason::Content => FallbackReason::Content,
                            FailureReason::Transport => FallbackReason::Transport,
                        },
                    });
                    progress.emit(ProgressEvent::Fallback {
                        task_id: scaffold.tasks[index].id.clone(),
                        reason: failure_class_for_terminal_cause(terminal_cause),
                    });
                    let mut event = ComposeEvent::new(ComposeEventKind::Fallback, context);
                    event.task_id = Some(scaffold.tasks[index].id.clone());
                    event.fallback_reason = Some(
                        match failure_reason {
                            FailureReason::Content => "content",
                            FailureReason::Transport => "transport",
                        }
                        .to_string(),
                    );
                    sink.emit(event);
                    questions.extend(draft.questions);
                }
            }
            Err(error) => {
                let class = failure_class_for_execution(&error);
                if error.terminal_cause().is_some() {
                    progress.emit(ProgressEvent::ProviderError {
                        class,
                        status: error.provider_status(),
                        rate_limit: error.rate_limit_kind(),
                    });
                }
                progress.emit(ProgressEvent::Failed {
                    stage: crate::progress::ProgressStage::Generate,
                    class,
                });
                return Err(GenerateMarkdownError::Compose(error.into_public()));
            }
        }
    }
    questions.sort_by_key(|question| {
        intermediate
            .qblocks
            .iter()
            .position(|qblock| qblock.id == question.id)
            .unwrap_or(usize::MAX)
    });
    let document = GeneratedDocument { questions };
    // qblockごとのaccept/retry判定と同じ契約を最終文書にも適用する。
    // AnswerLeakageのような非構造的な疑いを最後だけhard failureへ戻さない。
    let report = validate_generated_documents_with_leakage_baselines(
        &intermediate,
        &document,
        &leakage_baselines,
    );
    if let Some(error) = report.errors.first() {
        progress.emit(ProgressEvent::Failed {
            stage: crate::progress::ProgressStage::Validate,
            class: FailureClass::Validation,
        });
        return Err(GenerateMarkdownError::Compose(
            ComposePlanError::Validation {
                id: "document".to_string(),
                errors: vec![error.to_string()],
            },
        ));
    }
    progress.emit(ProgressEvent::Validated {
        tasks: scaffold.tasks.len(),
    });
    Ok(GenerateMarkdownOutcome {
        document,
        fallback_summary,
    })
}

pub(crate) fn prepare_selected_plan(
    scaffold: &ScaffoldDocument,
    indexes: &[usize],
    policy: ComposeExecutionPolicy,
    quota: Option<&QuotaProfile>,
) -> Result<crate::planner::PreparedComposePlan, ComposePlanError> {
    let selected = ScaffoldDocument {
        tasks: indexes
            .iter()
            .map(|index| scaffold.tasks[*index].clone())
            .collect(),
    };
    crate::planner::prepare_compose_plan_with_quota(&selected, policy, quota)
}

struct BatchProgressSink<'a> {
    inner: &'a dyn ProgressSink,
    offset: usize,
    total: usize,
}

impl<'a> BatchProgressSink<'a> {
    fn new(inner: &'a dyn ProgressSink, offset: usize, total: usize) -> Self {
        Self {
            inner,
            offset,
            total,
        }
    }
}

impl ProgressSink for BatchProgressSink<'_> {
    fn emit(&self, event: ProgressEvent) {
        match event {
            ProgressEvent::BatchComplete {
                number,
                successes,
                retries,
                ..
            } => self.inner.emit(ProgressEvent::BatchComplete {
                number: self.offset + number,
                total: self.total,
                successes,
                retries,
            }),
            event => self.inner.emit(event),
        }
    }
}

fn failure_class_for_plan(error: &ComposePlanError) -> FailureClass {
    match error {
        ComposePlanError::Configuration { .. } => FailureClass::Configuration,
        ComposePlanError::Prompt(_) => FailureClass::Configuration,
        ComposePlanError::Llm(_) => FailureClass::Content,
        ComposePlanError::Json(_) => FailureClass::Content,
        ComposePlanError::Validation { .. } => FailureClass::Validation,
        ComposePlanError::Partial { .. } => FailureClass::Content,
    }
}

fn failure_class_for_execution(error: &crate::generation::ComposeExecutionError) -> FailureClass {
    match error.terminal_cause() {
        Some(crate::generation::TerminalCause::Authentication) => FailureClass::Authentication,
        Some(crate::generation::TerminalCause::Configuration) => FailureClass::Configuration,
        Some(crate::generation::TerminalCause::RateLimited { .. }) => FailureClass::RateLimited,
        Some(crate::generation::TerminalCause::Timeout) => FailureClass::Timeout,
        Some(crate::generation::TerminalCause::Transport) => FailureClass::Transport,
        Some(crate::generation::TerminalCause::Api { .. }) => FailureClass::Api,
        Some(crate::generation::TerminalCause::Content) | None => FailureClass::Content,
    }
}

fn failure_class_for_terminal_cause(
    cause: crate::generation::TerminalCause,
) -> FailureClass {
    match cause {
        crate::generation::TerminalCause::Authentication => FailureClass::Authentication,
        crate::generation::TerminalCause::Configuration => FailureClass::Configuration,
        crate::generation::TerminalCause::RateLimited { .. } => FailureClass::RateLimited,
        crate::generation::TerminalCause::Timeout => FailureClass::Timeout,
        crate::generation::TerminalCause::Transport => FailureClass::Transport,
        crate::generation::TerminalCause::Api { .. } => FailureClass::Api,
        crate::generation::TerminalCause::Content => FailureClass::Content,
    }
}

#[derive(Debug)]
pub(crate) struct BuildScaffoldError {
    message: String,
}

impl BuildScaffoldError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BuildScaffoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for BuildScaffoldError {}

pub(crate) fn build_blank_scaffold(
    markdown: &str,
    parsed: &ParsedDocument,
) -> Result<(ScaffoldDocument, HashMap<String, Vec<usize>>), MarkdownParseError> {
    let mut tasks = Vec::new();
    let mut baselines = HashMap::new();

    for qblock in &parsed.qblocks {
        let source = markdown
            .get(qblock.source_range.clone())
            .ok_or_else(|| MarkdownParseError::InvalidSyntax {
                line: 0,
                message: "qblock source range is not on UTF-8 boundaries".to_string(),
            })?;
        let mut replacements = qblock
            .target_spans
            .iter()
            .enumerate()
            .map(|(index, span)| {
                let relative = relative_range(&qblock.source_range, &span.range)?;
                Ok((relative, format!("<BLANK_{index}>")))
            })
            .collect::<Result<Vec<_>, MarkdownParseError>>()?;
        replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
        let mut scaffold = source.to_string();
        for (range, placeholder) in replacements {
            scaffold.replace_range(range, &placeholder);
        }

        let leakage_baseline = qblock
            .qblock
            .targets
            .iter()
            .map(|target| count_occurrences(&scaffold, &target.answer))
            .collect::<Vec<_>>();
        baselines.insert(qblock.qblock.id.clone(), leakage_baseline);
        tasks.push(ScaffoldTask {
            id: qblock.qblock.id.clone(),
            source_text: source.to_string(),
            cloze_template: scaffold.clone(),
            scaffold_question: scaffold,
            blank_count: qblock.qblock.targets.len(),
            answers: qblock
                .qblock
                .targets
                .iter()
                .map(|target| target.answer.clone())
                .collect(),
        });
    }

    Ok((ScaffoldDocument { tasks }, baselines))
}

fn count_occurrences(text: &str, needle: &str) -> usize {
    if needle.is_empty() {
        0
    } else {
        text.match_indices(needle).count()
    }
}

fn relative_range(
    block: &Range<usize>,
    target: &Range<usize>,
) -> Result<Range<usize>, MarkdownParseError> {
    if target.start < block.start || target.end > block.end || target.start > target.end {
        return Err(MarkdownParseError::InvalidSyntax {
            line: 0,
            message: "target span is outside qblock range".to_string(),
        });
    }
    Ok(target.start - block.start..target.end - block.start)
}

fn compose_indexes(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    indexes: &[usize],
    policy: ComposeExecutionPolicy,
    composer: &dyn QuestionComposer,
    context: &RunContext,
    sink: &dyn EventSink,
    progress: &dyn ProgressSink,
    prepared: Option<&crate::planner::PreparedComposePlan>,
    extra_constraints: &[String],
    leakage_baselines: &HashMap<String, Vec<usize>>,
) -> Result<GeneratedDocument, crate::generation::ComposeExecutionError> {
    let selected_intermediate = IntermediateDocument {
        meta: intermediate.meta.clone(),
        qblocks: indexes
            .iter()
            .map(|index| intermediate.qblocks[*index].clone())
            .collect(),
    };
    let selected_scaffold = ScaffoldDocument {
        tasks: indexes
            .iter()
            .map(|index| scaffold.tasks[*index].clone())
            .collect(),
    };
    let selected_baselines = indexes
        .iter()
        .filter_map(|index| {
            let id = &scaffold.tasks[*index].id;
            leakage_baselines
                .get(id)
                .cloned()
                .map(|baseline| (id.clone(), baseline))
        })
        .collect::<HashMap<_, _>>();
    crate::generation::execute_legacy(
        &selected_intermediate,
        &selected_scaffold,
        policy,
        composer,
        extra_constraints,
        context,
        sink,
        progress,
        Some(&selected_baselines),
        prepared,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_markdown_located;

    #[test]
    fn blank_placeholders_are_numbered_per_qblock() {
        let markdown = "<!-- qblock -->\nA[one]B[two]\n<!-- /qblock -->\n\n<!-- qblock -->\nC[three]D\n<!-- /qblock -->";
        let parsed = parse_markdown_located(markdown).unwrap();
        let (scaffold, _) = build_blank_scaffold(markdown, &parsed).unwrap();
        assert_eq!(scaffold.tasks.len(), 2);
        assert!(scaffold.tasks[0].scaffold_question.contains("<BLANK_0>"));
        assert!(scaffold.tasks[0].scaffold_question.contains("<BLANK_1>"));
        assert!(scaffold.tasks[1].scaffold_question.contains("<BLANK_0>"));
        assert!(!scaffold.tasks[1].scaffold_question.contains("<BLANK_1>"));
    }
}
