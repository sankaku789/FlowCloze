use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use crate::compose::{
    compose_task_from_scaffold, merge_composed_questions, normalize_blank_placeholders,
    preflight_composed_questions, try_merge_composed_questions, ComposeBatchRequest, ComposeError,
    ComposeMergeIssue, ComposedDocument, ComposedQuestion, QuestionComposer,
};
use crate::json::{IntermediateDocument, IntermediateMeta, IntermediateQBlock, IntermediateTarget};
use crate::observability::{fnv1a_64, ComposeEvent, ComposeEventKind, EventSink, RunContext};
use crate::planner::{
    estimate_task_tokens, pack_attempts, plan_batches, validate_port_policy, BatchPolicy,
    CharHeuristicTokenEstimator, ComposeExecutionPolicy, ComposePlanError, FailureReason,
    PreparedComposePlan, TokenEstimator,
};
use crate::progress::{ProgressEvent, ProgressSink, RetryCause, RetryResult};
use crate::prompt::build_compose_request_prompt;
use crate::rate_limit::RateLimitKind;
use crate::scaffold::{ScaffoldDocument, ScaffoldTask};
use crate::task::GenerationTask;
use crate::validation::{
    validate_runtime_generated_documents, GeneratedDocument, ValidationError,
};

pub type ExecutionError = ComposeExecutionError;

pub struct ExecutionContext<'a> {
    pub run: &'a RunContext,
    pub events: &'a dyn EventSink,
    pub progress: &'a dyn ProgressSink,
}

#[derive(Debug, Clone, Copy)]
pub struct Executor {
    policy: ComposeExecutionPolicy,
}

impl Executor {
    pub fn new(policy: ComposeExecutionPolicy) -> Self {
        Self { policy }
    }

    pub fn execute(
        &self,
        tasks: &[GenerationTask],
        plan: &PreparedComposePlan,
        composer: &dyn QuestionComposer,
        context: &ExecutionContext<'_>,
    ) -> Result<GeneratedDocument, ExecutionError> {
        let intermediate = IntermediateDocument {
            meta: IntermediateMeta {
                source: String::new(),
            },
            qblocks: tasks
                .iter()
                .map(|task| IntermediateQBlock {
                    id: task.id.clone(),
                    section: (!task.section.is_empty()).then(|| task.section.clone()),
                    source_text: task.source_text.clone(),
                    targets: task
                        .answers
                        .iter()
                        .zip(&task.target_types)
                        .map(|(answer, target_type)| IntermediateTarget {
                            answer: answer.clone(),
                            target_type: target_type.clone().unwrap_or_else(|| "term-name".into()),
                        })
                        .collect(),
                    warnings: Vec::new(),
                })
                .collect(),
        };
        let scaffold = ScaffoldDocument {
            tasks: tasks
                .iter()
                .map(|task| ScaffoldTask {
                    id: task.id.clone(),
                    source_text: task.source_text.clone(),
                    cloze_template: task.draft_question.clone(),
                    scaffold_question: task.draft_question.clone(),
                    blank_count: task.answers.len(),
                    answers: task.answers.clone(),
                })
                .collect(),
        };
        execute_prepared_with_terminal_cause(
            &intermediate,
            &scaffold,
            self.policy,
            composer,
            &[],
            context.run,
            context.events,
            context.progress,
            Some(plan),
        )
    }
}

/// provider起因の実際の終端分類。公開エラー形状とは分離して内部で保持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalCause {
    Content,
    Authentication,
    Configuration,
    RateLimited { kind: RateLimitKind },
    Timeout,
    Transport,
    Api { status: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposeMode {
    Batched,
    SingleTask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureScope {
    QBlock,
    Batch { group: usize, previous_size: usize },
}

#[derive(Debug, Clone)]
pub(crate) struct TaskAttempt {
    pub(crate) index: usize,
    pub(crate) retry_count: u32,
    pub(crate) mode: ComposeMode,
    pub(crate) feedback: Vec<String>,
    pub(crate) retry_group: Option<usize>,
    pub(crate) max_batch_size: Option<usize>,
}

#[derive(Debug)]
struct TaskFailure {
    index: usize,
    task_id: String,
    retry_count: u32,
    errors: Vec<String>,
    reason: FailureReason,
    terminal_cause: TerminalCause,
    scope: FailureScope,
}

#[derive(Debug)]
pub struct ComposeExecutionError {
    error: ComposePlanError,
    terminal_cause: Option<TerminalCause>,
    fallback_causes: Vec<TerminalCause>,
}

impl ComposeExecutionError {
    fn from_error(error: ComposePlanError) -> Self {
        Self {
            error,
            terminal_cause: None,
            fallback_causes: Vec::new(),
        }
    }

    fn with_cause(error: ComposePlanError, terminal_cause: TerminalCause) -> Self {
        Self {
            error,
            terminal_cause: Some(terminal_cause),
            fallback_causes: Vec::new(),
        }
    }

    pub(crate) fn terminal_cause(&self) -> Option<TerminalCause> {
        self.terminal_cause
    }

    pub(crate) fn provider_status(&self) -> Option<u16> {
        match self.terminal_cause {
            Some(TerminalCause::RateLimited { .. }) => Some(429),
            Some(TerminalCause::Api { status }) => Some(status),
            _ => None,
        }
    }

    pub(crate) fn rate_limit_kind(&self) -> Option<RateLimitKind> {
        match self.terminal_cause {
            Some(TerminalCause::RateLimited { kind }) => Some(kind),
            _ => None,
        }
    }

    pub(crate) fn as_public(&self) -> &ComposePlanError {
        &self.error
    }

    pub(crate) fn into_public(self) -> ComposePlanError {
        self.error
    }

    pub(crate) fn into_parts(
        self,
    ) -> (ComposePlanError, Option<TerminalCause>, Vec<TerminalCause>) {
        (self.error, self.terminal_cause, self.fallback_causes)
    }
}

impl From<ComposePlanError> for ComposeExecutionError {
    fn from(error: ComposePlanError) -> Self {
        Self::from_error(error)
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_prepared_with_terminal_cause(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
    composer: &dyn QuestionComposer,
    extra_constraints: &[String],
    context: &RunContext,
    sink: &dyn EventSink,
    progress: &dyn ProgressSink,
    prepared: Option<&PreparedComposePlan>,
) -> Result<GeneratedDocument, ComposeExecutionError> {
    let effective_batch_policy = prepared
        .map(|plan| plan.effective_policy)
        .unwrap_or(policy.batch_policy);
    let effective_policy = ComposeExecutionPolicy {
        batch_policy: effective_batch_policy,
        ..policy
    };
    validate_port_policy(scaffold, effective_policy).map_err(ComposeExecutionError::from_error)?;
    let estimator = CharHeuristicTokenEstimator;
    let mut completed = HashMap::<String, ComposedQuestion>::new();
    let mut retry_queue = Vec::<TaskAttempt>::new();

    let mut terminal_failures = Vec::new();
    let batches = prepared
        .map(|plan| plan.batches.clone())
        .unwrap_or_else(|| plan_batches(scaffold, effective_batch_policy, &estimator));
    let initial_batch_total = batches.len();
    for (batch_number, batch) in batches.iter().enumerate() {
        let failures = run_port_batch(
            intermediate,
            scaffold,
            batch,
            batch_number,
            composer,
            &mut completed,
            context,
            sink,
            effective_batch_policy.max_concurrent_batches,
            extra_constraints,
        )?;
        let failure_count = failures.len();
        let batch_terminal_failures =
            enqueue_port_failures(&mut retry_queue, failures, policy.max_content_retries);
        progress.emit(ProgressEvent::BatchComplete {
            number: batch_number + 1,
            total: initial_batch_total,
            successes: batch.len() - failure_count,
            retries: failure_count - batch_terminal_failures.len(),
        });
        let has_terminal_transport = batch_terminal_failures
            .iter()
            .any(|failure| failure.reason == FailureReason::Transport);
        let terminal_cause = batch_terminal_failures
            .iter()
            .find(|failure| failure.reason == FailureReason::Transport)
            .map(|failure| failure.terminal_cause);
        terminal_failures.extend(batch_terminal_failures);
        if has_terminal_transport {
            terminal_failures.extend(retry_queue.drain(..).filter_map(|attempt| {
                let task = &scaffold.tasks[attempt.index];
                (!completed.contains_key(&task.id)).then(|| TaskFailure {
                    index: attempt.index,
                    task_id: task.id.clone(),
                    retry_count: attempt.retry_count,
                    errors: attempt.feedback,
                    reason: FailureReason::Content,
                    terminal_cause: TerminalCause::Content,
                    scope: FailureScope::QBlock,
                })
            }));
            terminal_failures.extend(batches[batch_number + 1..].iter().flatten().map(|attempt| {
                TaskFailure {
                    index: attempt.index,
                    task_id: scaffold.tasks[attempt.index].id.clone(),
                    retry_count: attempt.retry_count,
                    errors: vec!["not-attempted-after-terminal".to_string()],
                    reason: FailureReason::Transport,
                    terminal_cause: terminal_cause.expect("transport failure has a cause"),
                    scope: FailureScope::QBlock,
                }
            }));
            return Err(partial_plan_error(
                intermediate,
                &completed,
                terminal_failures,
            ));
        }
    }

    let mut retry_batch_number = initial_batch_total;
    while !retry_queue.is_empty() {
        let pending = std::mem::take(&mut retry_queue);
        let retry_batches =
            plan_retry_attempts(scaffold, pending, effective_batch_policy, &estimator);
        for batch in retry_batches {
            let tracked = batch
                .iter()
                .map(|attempt| {
                    (
                        attempt.index,
                        attempt.retry_count,
                        retry_cause(&attempt.feedback),
                    )
                })
                .collect::<Vec<_>>();
            let failures = run_port_batch(
                intermediate,
                scaffold,
                &batch,
                retry_batch_number,
                composer,
                &mut completed,
                context,
                sink,
                effective_batch_policy.max_concurrent_batches,
                extra_constraints,
            )?;
            retry_batch_number += 1;
            let terminal =
                enqueue_port_failures(&mut retry_queue, failures, policy.max_content_retries);
            for (task_index, retry_count, cause) in tracked {
                let task_id = scaffold.tasks[task_index].id.clone();
                let result = if completed.contains_key(&task_id) {
                    RetryResult::Success
                } else if terminal.iter().any(|failure| failure.index == task_index) {
                    RetryResult::Failed
                } else {
                    RetryResult::Retry
                };
                progress.emit(ProgressEvent::Retry {
                    task_id,
                    attempt: retry_count,
                    cause,
                    result,
                });
            }
            terminal_failures.extend(terminal);
        }
    }

    if !terminal_failures.is_empty() {
        return Err(partial_plan_error(
            intermediate,
            &completed,
            terminal_failures,
        ));
    }

    let composed = ComposedDocument {
        questions: intermediate
            .qblocks
            .iter()
            .filter_map(|qblock| completed.remove(&qblock.id))
            .collect(),
    };
    let generated = try_merge_composed_questions(intermediate, composed)
        .map_err(|error| ComposePlanError::Validation {
            id: error
                .issues
                .first()
                .map(compose_issue_id)
                .unwrap_or_else(|| "batch".to_string()),
            errors: error
                .issues
                .iter()
                .map(|_| "id-mismatch".to_string())
                .collect(),
        })
        .map_err(ComposeExecutionError::from_error)?;
    let report = validate_runtime_generated_documents(intermediate, &generated);
    if let Some(error) = report.errors.first() {
        return Err(ComposeExecutionError::from_error(
            ComposePlanError::Validation {
                id: validation_error_id(error),
                errors: report
                    .errors
                    .iter()
                    .map(validation_error_class)
                    .map(str::to_string)
                    .collect(),
            },
        ));
    }
    Ok(generated)
}

fn partial_plan_error(
    intermediate: &IntermediateDocument,
    completed: &HashMap<String, ComposedQuestion>,
    failures: Vec<TaskFailure>,
) -> ComposeExecutionError {
    let mut seen_ids = HashSet::new();
    let mut failures = failures
        .into_iter()
        .filter(|failure| !completed.contains_key(&failure.task_id))
        .filter(|failure| seen_ids.insert(failure.task_id.clone()))
        .collect::<Vec<_>>();
    failures.sort_by_key(|failure| failure.index);
    let terminal_cause = failures
        .iter()
        .rev()
        .find(|failure| failure.terminal_cause != TerminalCause::Content)
        .or_else(|| failures.last())
        .map(|failure| failure.terminal_cause);
    ComposeExecutionError {
        error: ComposePlanError::Partial {
            document: merge_composed_questions(
                intermediate,
                ComposedDocument {
                    questions: intermediate
                        .qblocks
                        .iter()
                        .filter_map(|qblock| completed.get(&qblock.id).cloned())
                        .collect(),
                },
            ),
            failed_ids: failures
                .iter()
                .map(|failure| failure.task_id.clone())
                .collect(),
            failed_reasons: failures.iter().map(|failure| failure.reason).collect(),
        },
        terminal_cause,
        fallback_causes: failures
            .iter()
            .map(|failure| failure.terminal_cause)
            .collect(),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_port_batch(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    attempts: &[TaskAttempt],
    batch_number: usize,
    composer: &dyn QuestionComposer,
    completed: &mut HashMap<String, ComposedQuestion>,
    context: &RunContext,
    sink: &dyn EventSink,
    max_concurrent_batches: usize,
    extra_constraints: &[String],
) -> Result<Vec<TaskFailure>, ComposeExecutionError> {
    let request = ComposeBatchRequest {
        batch_id: format!(
            "compose-{batch_number}-{}-attempt-{}",
            attempts
                .iter()
                .map(|attempt| scaffold.tasks[attempt.index].id.as_str())
                .collect::<Vec<_>>()
                .join("-"),
            attempts[0].retry_count
        ),
        tasks: attempts
            .iter()
            .map(|attempt| compose_task_from_scaffold(&scaffold.tasks[attempt.index]))
            .collect(),
        prompt_version: "compose-v2".to_string(),
        extra_constraints: extra_constraints.to_vec(),
        retry_feedback: attempts
            .iter()
            .flat_map(|attempt| attempt.feedback.iter().cloned())
            .collect(),
    };
    let mut batch_event = ComposeEvent::new(ComposeEventKind::BatchStarted, context);
    batch_event.batch_id = Some(request.batch_id.clone());
    batch_event.max_concurrent_batches = Some(max_concurrent_batches);
    sink.emit(batch_event);
    let prompt_hash = build_compose_request_prompt(&request)
        .ok()
        .map(|prompt| fnv1a_64(&prompt));
    let started = Instant::now();
    let output = match composer.compose(&request) {
        Ok(output) => output,
        Err(ComposeError::InvalidResponse | ComposeError::EmptyResponse) => {
            emit_attempt_events(
                attempts,
                scaffold,
                &request,
                context,
                sink,
                prompt_hash.as_deref(),
                None,
                started.elapsed().as_millis(),
                Some("content"),
                None,
            );
            return Ok(attempts
                .iter()
                .map(|attempt| TaskFailure {
                    index: attempt.index,
                    task_id: scaffold.tasks[attempt.index].id.clone(),
                    retry_count: attempt.retry_count,
                    errors: vec!["invalid-provider-content".to_string()],
                    reason: FailureReason::Content,
                    terminal_cause: TerminalCause::Content,
                    scope: FailureScope::Batch {
                        group: batch_number,
                        previous_size: attempts.len(),
                    },
                })
                .collect());
        }
        Err(error) if transport_fallbackable(&error) => {
            let terminal_cause = terminal_cause_for_error(&error);
            emit_attempt_events(
                attempts,
                scaffold,
                &request,
                context,
                sink,
                prompt_hash.as_deref(),
                None,
                started.elapsed().as_millis(),
                Some(error_class(&error)),
                None,
            );
            return Ok(attempts
                .iter()
                .map(|attempt| TaskFailure {
                    index: attempt.index,
                    task_id: scaffold.tasks[attempt.index].id.clone(),
                    retry_count: attempt.retry_count,
                    errors: vec![error_class(&error).to_string()],
                    reason: FailureReason::Transport,
                    terminal_cause,
                    scope: FailureScope::QBlock,
                })
                .collect());
        }
        Err(error) => {
            emit_attempt_events(
                attempts,
                scaffold,
                &request,
                context,
                sink,
                prompt_hash.as_deref(),
                None,
                started.elapsed().as_millis(),
                Some(error_class(&error)),
                None,
            );
            let terminal_cause = terminal_cause_for_error(&error);
            return Err(ComposeExecutionError::with_cause(
                map_composer_error(error),
                terminal_cause,
            ));
        }
    };
    let output_chars = output
        .items
        .iter()
        .map(|item| (item.id.clone(), item.question.chars().count()))
        .collect::<HashMap<_, _>>();
    emit_attempt_events(
        attempts,
        scaffold,
        &request,
        context,
        sink,
        (output.metadata.adapter != "identity")
            .then_some(prompt_hash.as_deref())
            .flatten(),
        Some(&output.metadata),
        started.elapsed().as_millis(),
        None,
        Some(&output_chars),
    );
    let composed = ComposedDocument {
        questions: output
            .items
            .into_iter()
            .map(|item| ComposedQuestion {
                id: item.id,
                question: item.question,
            })
            .collect(),
    };
    let batch_intermediate = IntermediateDocument {
        meta: intermediate.meta.clone(),
        qblocks: attempts
            .iter()
            .map(|attempt| intermediate.qblocks[attempt.index].clone())
            .collect(),
    };
    let issues = preflight_composed_questions(&batch_intermediate, &composed);
    let unknown_id = issues.iter().find_map(|issue| match issue {
        ComposeMergeIssue::UnknownQuestionId { id } => Some(id.clone()),
        _ => None,
    });
    let mut by_id = composed
        .questions
        .into_iter()
        .map(|question| (question.id.clone(), question))
        .collect::<HashMap<_, _>>();
    let mut failures = Vec::new();
    for attempt in attempts {
        let qblock = &intermediate.qblocks[attempt.index];
        if issues
            .iter()
            .any(|issue| compose_issue_id(issue) == qblock.id)
        {
            failures.push(TaskFailure {
                index: attempt.index,
                task_id: qblock.id.clone(),
                retry_count: attempt.retry_count,
                errors: vec!["id-mismatch".to_string()],
                reason: FailureReason::Content,
                terminal_cause: TerminalCause::Content,
                scope: FailureScope::QBlock,
            });
            continue;
        }
        let Some(mut question) = by_id.remove(&qblock.id) else {
            failures.push(TaskFailure {
                index: attempt.index,
                task_id: qblock.id.clone(),
                retry_count: attempt.retry_count,
                errors: vec!["missing-id".to_string()],
                reason: FailureReason::Content,
                terminal_cause: TerminalCause::Content,
                scope: FailureScope::QBlock,
            });
            continue;
        };
        question.question = match normalize_blank_placeholders(
            &question.question,
            &compose_task_from_scaffold(&scaffold.tasks[attempt.index]),
        ) {
            Ok(question) => question,
            Err(error) => {
                failures.push(TaskFailure {
                    index: attempt.index,
                    task_id: qblock.id.clone(),
                    retry_count: attempt.retry_count,
                    errors: vec![error.to_string()],
                    reason: FailureReason::Content,
                    terminal_cause: TerminalCause::Content,
                    scope: FailureScope::QBlock,
                });
                continue;
            }
        };
        let one = IntermediateDocument {
            meta: intermediate.meta.clone(),
            qblocks: vec![qblock.clone()],
        };
        let generated = try_merge_composed_questions(
            &one,
            ComposedDocument {
                questions: vec![question.clone()],
            },
        )
        .map_err(|_| {
            ComposeExecutionError::from_error(ComposePlanError::Validation {
                id: qblock.id.clone(),
                errors: vec!["id-mismatch".to_string()],
            })
        })?;
        let report = validate_runtime_generated_documents(&one, &generated);
        if report.is_valid() {
            completed.insert(qblock.id.clone(), question);
            emit_validation_event(
                context,
                sink,
                &request.batch_id,
                qblock.id.as_str(),
                attempt.retry_count,
                true,
            );
        } else {
            emit_validation_event(
                context,
                sink,
                &request.batch_id,
                qblock.id.as_str(),
                attempt.retry_count,
                false,
            );
            failures.push(TaskFailure {
                index: attempt.index,
                task_id: qblock.id.clone(),
                retry_count: attempt.retry_count,
                errors: build_retry_feedback(&qblock.id, &generated, &report.errors),
                reason: FailureReason::Content,
                terminal_cause: TerminalCause::Content,
                scope: FailureScope::QBlock,
            });
        }
    }
    if let Some(id) = unknown_id {
        return Err(ComposeExecutionError::from_error(
            ComposePlanError::Validation {
                id,
                errors: vec!["unknown-id".to_string()],
            },
        ));
    }
    Ok(failures)
}

#[allow(clippy::too_many_arguments)]
fn emit_attempt_events(
    attempts: &[TaskAttempt],
    scaffold: &ScaffoldDocument,
    request: &ComposeBatchRequest,
    context: &RunContext,
    sink: &dyn EventSink,
    prompt_hash: Option<&str>,
    metadata: Option<&crate::compose::ComposeMetadata>,
    latency_ms: u128,
    error_class: Option<&str>,
    output_chars: Option<&HashMap<String, usize>>,
) {
    let estimator = CharHeuristicTokenEstimator;
    for attempt in attempts {
        let task = &scaffold.tasks[attempt.index];
        let mut event = ComposeEvent::new(ComposeEventKind::Attempt, context);
        event.batch_id = Some(request.batch_id.clone());
        event.task_id = Some(task.id.clone());
        event.attempt = Some(attempt.retry_count);
        event.compose_mode = Some(
            match attempt.mode {
                ComposeMode::Batched => "batched",
                ComposeMode::SingleTask => "single_task",
            }
            .to_string(),
        );
        if let Some(metadata) = metadata.filter(|metadata| metadata.adapter != "identity") {
            event.provider = Some(metadata.provider.clone());
            event.model = Some(metadata.model.clone());
        }
        event.prompt_version = Some(request.prompt_version.clone());
        event.prompt_hash = prompt_hash.map(str::to_string);
        event.source_hash = Some(fnv1a_64(&task.source_text));
        event.estimated_tokens = Some(estimate_task_tokens(task, &estimator));
        event.input_chars = Some(task.source_text.chars().count());
        event.output_chars = output_chars.and_then(|chars| chars.get(&task.id).copied());
        event.latency_ms = Some(latency_ms);
        event.error_class = error_class.map(str::to_string);
        sink.emit(event);
    }
}

fn emit_validation_event(
    context: &RunContext,
    sink: &dyn EventSink,
    batch_id: &str,
    task_id: &str,
    attempt: u32,
    valid: bool,
) {
    let mut event = ComposeEvent::new(ComposeEventKind::Validation, context);
    event.batch_id = Some(batch_id.to_string());
    event.task_id = Some(task_id.to_string());
    event.attempt = Some(attempt);
    event.validation_result = Some(if valid { "success" } else { "failure" }.to_string());
    sink.emit(event);
}

fn error_class(error: &ComposeError) -> &'static str {
    match error {
        ComposeError::InvalidResponse | ComposeError::EmptyResponse => "content",
        ComposeError::Configuration | ComposeError::Authentication => "configuration",
        ComposeError::RateLimited { .. } => "rate_limited",
        ComposeError::Timeout => "timeout",
        ComposeError::Transport => "transport",
        ComposeError::Api { .. } => "api",
    }
}

fn terminal_cause_for_error(error: &ComposeError) -> TerminalCause {
    match error {
        ComposeError::InvalidResponse | ComposeError::EmptyResponse => TerminalCause::Content,
        ComposeError::Configuration => TerminalCause::Configuration,
        ComposeError::Authentication => TerminalCause::Authentication,
        ComposeError::RateLimited { kind } => TerminalCause::RateLimited { kind: *kind },
        ComposeError::Timeout => TerminalCause::Timeout,
        ComposeError::Transport => TerminalCause::Transport,
        ComposeError::Api { status, .. } => TerminalCause::Api { status: *status },
    }
}

fn transport_fallbackable(error: &ComposeError) -> bool {
    matches!(
        error,
        ComposeError::RateLimited { .. }
            | ComposeError::Timeout
            | ComposeError::Transport
            | ComposeError::Api {
                retryable: true,
                ..
            }
    )
}

fn compose_issue_id(issue: &ComposeMergeIssue) -> String {
    match issue {
        ComposeMergeIssue::DuplicateExpectedQuestionId { id }
        | ComposeMergeIssue::DuplicateQuestionId { id }
        | ComposeMergeIssue::UnknownQuestionId { id }
        | ComposeMergeIssue::MissingQuestionId { id } => id.clone(),
    }
}

fn map_composer_error(error: ComposeError) -> ComposePlanError {
    match error {
        ComposeError::Configuration => ComposePlanError::Configuration {
            id: "composer".to_string(),
        },
        error => ComposePlanError::Llm(error.to_string()),
    }
}

fn enqueue_port_failures(
    queue: &mut Vec<TaskAttempt>,
    failures: Vec<TaskFailure>,
    max_content_retries: u32,
) -> Vec<TaskFailure> {
    let mut terminal = Vec::new();
    for failure in failures {
        if failure.reason == FailureReason::Transport || failure.retry_count >= max_content_retries
        {
            terminal.push(failure);
            continue;
        }
        let next_retry_count = failure.retry_count + 1;
        let final_retry = next_retry_count >= max_content_retries;
        let (retry_group, max_batch_size) = if final_retry {
            (None, Some(1))
        } else {
            match failure.scope {
                FailureScope::QBlock => (None, None),
                FailureScope::Batch {
                    group,
                    previous_size,
                } => (
                    Some(group),
                    Some((previous_size.saturating_add(1) / 2).max(1)),
                ),
            }
        };
        queue.push(TaskAttempt {
            index: failure.index,
            retry_count: next_retry_count,
            mode: if final_retry {
                ComposeMode::SingleTask
            } else {
                ComposeMode::Batched
            },
            feedback: failure.errors,
            retry_group,
            max_batch_size,
        });
    }
    terminal
}

fn retry_cause(feedback: &[String]) -> RetryCause {
    const CAUSES: &[(&str, RetryCause)] = &[
        (
            "invalid-provider-content",
            RetryCause::InvalidProviderContent,
        ),
        ("id-mismatch", RetryCause::IdMismatch),
        ("missing-id", RetryCause::MissingId),
        ("empty-question", RetryCause::EmptyQuestion),
        ("blank-count-mismatch", RetryCause::BlankCountMismatch),
        ("answer-not-in-targets", RetryCause::AnswerNotInTargets),
        ("answer-leakage", RetryCause::AnswerLeakage),
        ("missing-target-answer", RetryCause::MissingTargetAnswer),
        ("fixed-field-mismatch", RetryCause::FixedFieldMismatch),
        ("duplicate-id", RetryCause::DuplicateId),
        ("unknown-id", RetryCause::UnknownId),
        ("order-mismatch", RetryCause::OrderMismatch),
        ("anonymous-blank", RetryCause::AnonymousBlank),
        ("missing-placeholder", RetryCause::MissingPlaceholder),
        ("duplicate-placeholder", RetryCause::DuplicatePlaceholder),
        ("placeholder-order", RetryCause::PlaceholderOrder),
        ("malformed-placeholder", RetryCause::MalformedPlaceholder),
        ("unknown-placeholder", RetryCause::UnknownPlaceholder),
    ];
    for item in feedback {
        for (marker, cause) in CAUSES {
            if item.contains(marker) {
                return *cause;
            }
        }
    }
    RetryCause::ContentValidation
}

fn plan_retry_attempts<E>(
    scaffold: &ScaffoldDocument,
    attempts: Vec<TaskAttempt>,
    policy: BatchPolicy,
    estimator: &E,
) -> Vec<Vec<TaskAttempt>>
where
    E: TokenEstimator,
{
    let mut singletons = Vec::new();
    let mut ungrouped = Vec::new();
    let mut groups = BTreeMap::<usize, Vec<TaskAttempt>>::new();

    for attempt in attempts {
        if attempt.mode == ComposeMode::SingleTask {
            singletons.push(vec![attempt]);
        } else if let Some(group) = attempt.retry_group {
            groups.entry(group).or_default().push(attempt);
        } else {
            ungrouped.push(attempt);
        }
    }

    let mut batches = pack_attempts(
        scaffold,
        ungrouped,
        policy,
        policy.max_tasks_per_batch,
        estimator,
    );
    for (_, group) in groups {
        let max_qblocks = group
            .iter()
            .filter_map(|attempt| attempt.max_batch_size)
            .min()
            .unwrap_or(policy.max_tasks_per_batch)
            .min(policy.max_tasks_per_batch)
            .max(1);
        batches.extend(pack_attempts(
            scaffold,
            group,
            policy,
            max_qblocks,
            estimator,
        ));
    }
    batches.extend(singletons);
    batches.sort_by_key(|batch| {
        batch
            .iter()
            .map(|attempt| attempt.index)
            .min()
            .unwrap_or(usize::MAX)
    });
    batches
}

fn validation_error_id(error: &ValidationError) -> String {
    match error {
        ValidationError::EmptyQuestion { id }
        | ValidationError::DuplicateQuestionId { id }
        | ValidationError::UnknownQuestionId { id }
        | ValidationError::MissingQuestion { id }
        | ValidationError::FixedFieldMismatch { id, .. }
        | ValidationError::BlankAnswerCountMismatch { id, .. }
        | ValidationError::AnswerNotInTargets { id, .. }
        | ValidationError::AnswerLeakage { id, .. }
        | ValidationError::MissingTargetAnswer { id, .. } => id.clone(),
        ValidationError::QuestionOrderMismatch { .. }
        | ValidationError::InvalidIntermediateJson(_)
        | ValidationError::InvalidGeneratedJson(_) => "batch".to_string(),
    }
}

fn build_retry_feedback(
    id: &str,
    _generated: &GeneratedDocument,
    errors: &[ValidationError],
) -> Vec<String> {
    errors
        .iter()
        .map(|error| format!("{id}: {}", validation_error_class(error)))
        .collect()
}

fn validation_error_class(error: &ValidationError) -> &'static str {
    match error {
        ValidationError::EmptyQuestion { .. } => "empty-question",
        ValidationError::BlankAnswerCountMismatch { .. } => "blank-count-mismatch",
        ValidationError::AnswerNotInTargets { .. } => "answer-not-in-targets",
        ValidationError::AnswerLeakage { .. } => "answer-leakage",
        ValidationError::MissingTargetAnswer { .. } => "missing-target-answer",
        ValidationError::FixedFieldMismatch { .. } => "fixed-field-mismatch",
        ValidationError::DuplicateQuestionId { .. } => "duplicate-id",
        ValidationError::UnknownQuestionId { .. } => "unknown-id",
        ValidationError::MissingQuestion { .. } => "missing-id",
        ValidationError::QuestionOrderMismatch { .. } => "order-mismatch",
        ValidationError::InvalidIntermediateJson(_) => "invalid-intermediate",
        ValidationError::InvalidGeneratedJson(_) => "invalid-generated",
    }
}
