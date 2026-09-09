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
use crate::validation::GeneratedDocument;

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
    let report = crate::validation::validate_generated_documents(&intermediate, &document);
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
        ComposePlanError::Prompt(_) | ComposePlanError::Json(_) => FailureClass::Content,
        ComposePlanError::Llm(class) => match class.as_str() {
            "authentication" => FailureClass::Authentication,
            "configuration" => FailureClass::Configuration,
            "rate_limited" => FailureClass::RateLimited,
            "timeout" => FailureClass::Timeout,
            "transport" => FailureClass::Transport,
            "content" => FailureClass::Content,
            _ => FailureClass::Api,
        },
        ComposePlanError::Validation { .. } => FailureClass::Validation,
        ComposePlanError::Partial { failed_reasons, .. } => failed_reasons
            .first()
            .map(|reason| match reason {
                FailureReason::Content => FailureClass::Content,
                FailureReason::Transport => FailureClass::Transport,
            })
            .unwrap_or(FailureClass::Validation),
    }
}

fn failure_class_for_execution(error: &crate::executor::ComposeExecutionError) -> FailureClass {
    match error.terminal_cause() {
        Some(cause) => failure_class_for_terminal_cause(cause),
        None => failure_class_for_plan(error.as_public()),
    }
}

fn failure_class_for_terminal_cause(cause: crate::executor::TerminalCause) -> FailureClass {
    match cause {
        crate::executor::TerminalCause::Content => FailureClass::Content,
        crate::executor::TerminalCause::Authentication => FailureClass::Authentication,
        crate::executor::TerminalCause::Configuration => FailureClass::Configuration,
        crate::executor::TerminalCause::RateLimited { .. } => FailureClass::RateLimited,
        crate::executor::TerminalCause::Timeout => FailureClass::Timeout,
        crate::executor::TerminalCause::Transport => FailureClass::Transport,
        crate::executor::TerminalCause::Api { .. } => FailureClass::Api,
    }
}

#[allow(clippy::too_many_arguments)]
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
) -> Result<GeneratedDocument, crate::executor::ComposeExecutionError> {
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
    crate::executor::execute_legacy(
        &selected_intermediate,
        &selected_scaffold,
        policy,
        composer,
        extra_constraints,
        context,
        sink,
        progress,
        Some(leakage_baselines),
        prepared,
    )
}

pub(crate) fn build_blank_scaffold(
    markdown: &str,
    parsed: &ParsedDocument,
) -> Result<(ScaffoldDocument, HashMap<String, Vec<usize>>), MarkdownParseError> {
    let mut tasks = Vec::with_capacity(parsed.qblocks.len());
    let mut leakage_baselines = HashMap::new();
    for qblock in &parsed.qblocks {
        if qblock.qblock.targets.len() != qblock.target_locations.len() {
            return Err(MarkdownParseError::new("target位置を確定できません"));
        }
        for locations in qblock.target_locations.windows(2) {
            if locations[0].raw.end > locations[1].raw.start
                || locations[0].source_text.end > locations[1].source_text.start
            {
                return Err(MarkdownParseError::new("target spanが重複しています"));
            }
        }
        let mut replacements = Vec::new();
        for (index, (target, location)) in qblock
            .qblock
            .targets
            .iter()
            .zip(&qblock.target_locations)
            .enumerate()
        {
            validate_target_location(
                markdown,
                qblock.raw_body.clone(),
                target.answer.as_str(),
                location.raw.clone(),
                location.source_text.clone(),
                qblock.qblock.source_text.as_str(),
            )?;
            replacements.push((location.source_text.clone(), format!("<BLANK_{index}>")));
        }
        let mut scaffold_question = qblock.qblock.source_text.clone();
        for (span, token) in replacements.iter().rev() {
            scaffold_question.replace_range(span.clone(), token);
        }
        let non_target_segments = non_target_segments(&qblock.qblock.source_text, &replacements);
        leakage_baselines.insert(
            qblock.qblock.id.clone(),
            qblock
                .qblock
                .targets
                .iter()
                .map(|target| {
                    non_target_segments
                        .iter()
                        .map(|segment| segment.match_indices(&target.answer).count())
                        .sum()
                })
                .collect(),
        );
        tasks.push(ScaffoldTask {
            id: qblock.qblock.id.clone(),
            source_text: qblock.qblock.source_text.clone(),
            cloze_template: scaffold_question.clone(),
            scaffold_question,
            blank_count: qblock.qblock.targets.len(),
            answers: qblock
                .qblock
                .targets
                .iter()
                .map(|target| target.answer.clone())
                .collect(),
        });
    }
    Ok((ScaffoldDocument { tasks }, leakage_baselines))
}

fn non_target_segments<'a>(
    source_text: &'a str,
    replacements: &[(Range<usize>, String)],
) -> Vec<&'a str> {
    let mut start = 0;
    let mut segments = Vec::with_capacity(replacements.len() + 1);
    for (span, _) in replacements {
        segments.push(&source_text[start..span.start]);
        start = span.end;
    }
    segments.push(&source_text[start..]);
    segments
}

fn validate_target_location(
    markdown: &str,
    raw_body: Range<usize>,
    answer: &str,
    raw: Range<usize>,
    source: Range<usize>,
    source_text: &str,
) -> Result<(), MarkdownParseError> {
    if answer.trim().is_empty() || raw.is_empty() || source.is_empty() {
        return Err(MarkdownParseError::new(
            "空または空白だけのtarget answerは生成できません",
        ));
    }
    if raw.start < raw_body.start
        || raw.end > raw_body.end
        || !markdown.is_char_boundary(raw.start)
        || !markdown.is_char_boundary(raw.end)
        || !source_text.is_char_boundary(source.start)
        || !source_text.is_char_boundary(source.end)
    {
        return Err(MarkdownParseError::new(
            "target spanがUTF-8境界またはqblock範囲外です",
        ));
    }
    if markdown.get(raw) != Some(answer) || source_text.get(source) != Some(answer) {
        return Err(MarkdownParseError::new("target spanとanswerが一致しません"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::compose::IdentityComposer;

    use super::*;

    #[test]
    fn identity_generates_utf8_crlf_document_end_to_end() {
        let markdown = "#qblock{\r\n  [😀答]{term} は [同じ]{term}、[同じ]{term}。\r\n}\r\n";
        let outcome = generate_markdown_with_composer(
            markdown,
            GenerateMarkdownOptions::new("inline.md"),
            &IdentityComposer,
        )
        .expect("located identity path should validate");
        assert_eq!(
            outcome.document.questions[0].question,
            "＿＿＿ は ＿＿＿、＿＿＿。"
        );
        assert_eq!(
            outcome.document.questions[0].answers,
            ["😀答", "同じ", "同じ"]
        );
    }

    #[test]
    fn blank_placeholders_are_numbered_per_qblock() {
        let markdown = "#qblock{\n[alpha]{term} と [beta]{term}\n}\n#qblock{\n[gamma]{term}\n}\n";
        let parsed = parse_markdown_located(markdown).unwrap();
        let (scaffold, _) = build_blank_scaffold(markdown, &parsed).unwrap();
        assert!(scaffold.tasks[0].scaffold_question.contains("<BLANK_0>"));
        assert!(scaffold.tasks[0].scaffold_question.contains("<BLANK_1>"));
        assert!(scaffold.tasks[1].scaffold_question.contains("<BLANK_0>"));
        assert!(!scaffold.tasks[1].scaffold_question.contains("<BLANK_1>"));
    }

    #[test]
    fn identity_composer_generates_all_tasks() {
        let options = GenerateMarkdownOptions::new("inline.md");
        assert!(generate_markdown_with_composer(
            "#qblock{\n[answer]{term}\n}\n",
            options,
            &IdentityComposer
        )
        .is_ok());
    }

    #[test]
    fn draft_fallback_keeps_successful_tasks_and_excludes_metadata_from_document() {
        struct OneFails;
        impl QuestionComposer for OneFails {
            fn compose(
                &self,
                request: &crate::compose::ComposeBatchRequest,
            ) -> Result<crate::compose::ComposeBatchOutput, crate::compose::ComposeError>
            {
                let mut output = IdentityComposer.compose(request).unwrap();
                for item in &mut output.items {
                    if item.id.ends_with("002") {
                        item.question = "invalid".to_string();
                    }
                }
                Ok(output)
            }
        }
        let mut options = GenerateMarkdownOptions::new("inline.md");
        options.fallback = FallbackPolicy::Draft;
        let outcome = generate_markdown_with_composer(
            "#qblock{\n[alpha]{term}\n}\n#qblock{\n[beta]{term}\n}\n",
            options,
            &OneFails,
        )
        .unwrap();
        assert_eq!(outcome.document.questions.len(), 2);
        assert_eq!(outcome.fallback_summary.len(), 1);
        assert_eq!(outcome.fallback_summary[0].reason, FallbackReason::Content);
        assert!(!serde_json::to_string(&outcome.document)
            .unwrap()
            .contains("fallback"));
    }
}
