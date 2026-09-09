use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use crate::compose::{
    compose_task_from_scaffold, extract_json_candidate, merge_composed_questions,
    normalize_sentinel_question, preflight_composed_questions, try_merge_composed_questions,
    ComposeBatchRequest, ComposeError, ComposeMergeIssue, ComposedDocument, ComposedQuestion,
    QuestionComposer, WritingStyle,
};
use crate::json::{IntermediateDocument, IntermediateMeta, IntermediateQBlock, IntermediateTarget};
use crate::observability::{fnv1a_64, ComposeEvent, ComposeEventKind, EventSink, RunContext};
use crate::planner::{
    estimate_task_tokens, pack_attempts, plan_batches, validate_port_policy, BatchPolicy,
    CharHeuristicTokenEstimator, ComposeExecutionPolicy, ComposePlanError, FailureReason,
    PreparedComposePlan, TokenEstimator,
};
use crate::progress::{ProgressEvent, ProgressSink, RetryCause, RetryResult};
use crate::prompt::{build_compose_request_prompt, build_question_composer_prompt};
use crate::rate_limit::RateLimitKind;
use crate::scaffold::{ScaffoldDocument, ScaffoldTask};
use crate::task::GenerationTask;
use crate::validation::{
    validate_generated_document, validate_generated_documents,
    validate_generated_documents_with_leakage_baselines, GeneratedDocument, ValidationError,
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
        let leakage = tasks
            .iter()
            .map(|task| (task.id.clone(), task.leakage_baseline.clone()))
            .collect::<HashMap<_, _>>();
        execute_prepared_with_terminal_cause(
            &intermediate,
            &scaffold,
            self.policy,
            composer,
            &[],
            context.run,
            context.events,
            context.progress,
            Some(&leakage),
            Some(plan),
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_legacy(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
    composer: &dyn QuestionComposer,
    extra_constraints: &[String],
    context: &RunContext,
    sink: &dyn EventSink,
    progress: &dyn ProgressSink,
    leakage_baselines: Option<&HashMap<String, Vec<usize>>>,
    prepared: Option<&PreparedComposePlan>,
) -> Result<GeneratedDocument, ComposeExecutionError> {
    execute_prepared_with_terminal_cause(
        intermediate,
        scaffold,
        policy,
        composer,
        extra_constraints,
        context,
        sink,
        progress,
        leakage_baselines,
        prepared,
    )
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
/// taskが現在どのcompose戦略で処理されているかを表す．
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposeMode {
    /// 複数taskをまとめた通常batchで処理する状態．
    Batched,
    /// 失敗後にtask単独で再試行する状態．
    SingleTask,
}

/// content failureがqblock固有か、batch全体の崩れかを区別する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureScope {
    QBlock,
    Batch { group: usize, previous_size: usize },
}

/// retry queue内で追跡するqblockの状態．
#[derive(Debug, Clone)]
pub(crate) struct TaskAttempt {
    /// scaffold.tasks / intermediate.qblocks のindex．
    pub(crate) index: usize,
    /// このqblockを再試行した回数．
    pub(crate) retry_count: u32,
    /// 現在のcompose mode．将来のログ出力にも使う．
    pub(crate) mode: ComposeMode,
    /// 前回失敗時の検証理由．retry promptへ渡す．
    pub(crate) feedback: Vec<String>,
    /// batch全体の失敗を別batch由来のretryと再結合しないためのgroup。
    pub(crate) retry_group: Option<usize>,
    /// batch全体の失敗時に次回batchを縮小するqblock数上限。
    pub(crate) max_batch_size: Option<usize>,
}

/// 1 qblockの生成・検証に失敗した理由．
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
/// 公開エラーへ変換する前だけ、実際に実行を止めた原因を保持する。
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
    leakage_baselines: Option<&HashMap<String, Vec<usize>>>,
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
            leakage_baselines,
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
            // 先行batchでcontent retry待ちだったtaskも、通信断後は再実行しない。
            // feedbackを残してContent failureとしてfallbackへ渡す。
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
            // provider障害後に未実行batchへ通信せず、残りtaskをdraft対象へ渡す。
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
                leakage_baselines,
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
    let owned_baselines;
    let baselines = match leakage_baselines {
        Some(baselines) => baselines,
        None => {
            owned_baselines = scaffold
                .tasks
                .iter()
                .map(|task| {
                    (
                        task.id.clone(),
                        task.answers
                            .iter()
                            .map(|answer| count_occurrences(&task.scaffold_question, answer))
                            .collect(),
                    )
                })
                .collect();
            &owned_baselines
        }
    };
    let report =
        validate_generated_documents_with_leakage_baselines(intermediate, &generated, baselines);
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

fn count_occurrences(text: &str, needle: &str) -> usize {
    if needle.is_empty() {
        0
    } else {
        text.match_indices(needle).count()
    }
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
    // fallbackの対象と理由は、batchやretry queueの順ではなく入力順で安定させる。
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
    leakage_baselines: Option<&HashMap<String, Vec<usize>>>,
) -> Result<Vec<TaskFailure>, ComposeExecutionError> {
    let request = ComposeBatchRequest {
        schema_version: 1,
        // retryはtask単位なので、別batch由来の再試行とも衝突しないIDにする。
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
        style: WritingStyle::PlainJapanese,
        prompt_version: "compose-v2".to_string(),
        extra_constraints: extra_constraints.to_vec(),
        retry_feedback: attempts
            .iter()
            .flat_map(|attempt| attempt.feedback.iter().cloned())
            .collect(),
    };
    // max_concurrent_batchesは設定の検証・観測値であり、この実装は逐次実行する。
    let mut batch_event = ComposeEvent::new(ComposeEventKind::BatchStarted, context);
    batch_event.batch_id = Some(request.batch_id.clone());
    batch_event.max_concurrent_batches = Some(max_concurrent_batches);
    sink.emit(batch_event);
    // adapterへ実際に渡すrequest由来prompt（retry feedbackも含む）をhash化する。
    let prompt_hash = build_compose_request_prompt(&request)
        .ok()
        .map(|prompt| fnv1a_64(&prompt));
    let started = Instant::now();
    let output = match composer.compose(&request) {
        Ok(output) => output,
        // providerが到達して返した内容だけをcontent retryへ送る。
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
        question.question = match normalize_sentinel_question(
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
        // located経路では、target spanを除いた正確な基準で確定前に検証する。
        let report = match leakage_baselines {
            Some(baselines) => {
                validate_generated_documents_with_leakage_baselines(&one, &generated, baselines)
            }
            None => validate_generated_documents(&one, &generated),
        };
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
    // 未知IDは相関不能のstrict failure。既知taskの成功を先に確定し、batch全体を
    // content retryには戻さない。
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
        // 公開payloadはComposer境界の表示と一致させ、詳細な分類は内部で保持する。
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
        ("missing-sentinel", RetryCause::MissingSentinel),
        ("duplicate-sentinel", RetryCause::DuplicateSentinel),
        ("sentinel-order", RetryCause::SentinelOrder),
        ("malformed-sentinel", RetryCause::MalformedSentinel),
        ("unknown-sentinel", RetryCause::UnknownSentinel),
        ("foreign-sentinel", RetryCause::ForeignSentinel),
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
/// 任意のTokenEstimatorでadaptive composeを実行する．
pub fn compose_with_estimator<F, E>(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    policy: BatchPolicy,
    extra_constraints: &[String],
    estimator: &E,
    generate_text: &mut F,
) -> Result<ComposedDocument, ComposePlanError>
where
    F: FnMut(&str) -> Result<String, String>,
    E: TokenEstimator,
{
    let mut completed = HashMap::<String, ComposedQuestion>::new();
    let mut retry_queue = Vec::<TaskAttempt>::new();

    // 初回はpolicyに従って複数taskをbatch化し，成功したtaskから確定する．
    for batch in plan_batches(scaffold, policy, estimator) {
        let failures = run_batch(
            intermediate,
            scaffold,
            &batch,
            extra_constraints,
            &[],
            generate_text,
            &mut completed,
        )?;
        enqueue_failures(&mut retry_queue, failures, policy)?;
    }

    // 失敗taskだけを単独retryし，成功済みtaskは再生成しない．
    while let Some(attempt) = retry_queue.pop() {
        let task = &scaffold.tasks[attempt.index];
        if completed.contains_key(&task.id) {
            continue;
        }
        let mut feedback = attempt.feedback.clone();
        feedback.push(format!(
            "{}: 前回の失敗を踏まえ，このtaskだけを生成してください。",
            task.id
        ));
        let failures = run_batch(
            intermediate,
            scaffold,
            &[attempt],
            extra_constraints,
            &feedback,
            generate_text,
            &mut completed,
        )?;
        enqueue_failures(&mut retry_queue, failures, policy)?;
    }

    // 最終出力は中間表現のqblock順へ戻して安定化する．
    let questions = intermediate
        .qblocks
        .iter()
        .filter_map(|qblock| completed.remove(&qblock.id))
        .collect::<Vec<_>>();

    Ok(ComposedDocument { questions })
}
/// 1 batchをLLMへ投げ，task単位で成功・失敗を分類する．
fn run_batch<F>(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    attempts: &[TaskAttempt],
    extra_constraints: &[String],
    retry_feedback: &[String],
    generate_text: &mut F,
    completed: &mut HashMap<String, ComposedQuestion>,
) -> Result<Vec<TaskFailure>, ComposePlanError>
where
    F: FnMut(&str) -> Result<String, String>,
{
    // このLLM呼び出しに含めるtaskだけのscaffoldを作る．
    let batch_scaffold = ScaffoldDocument {
        tasks: attempts
            .iter()
            .map(|attempt| scaffold.tasks[attempt.index].clone())
            .collect(),
    };
    let prompt = build_question_composer_prompt(&batch_scaffold, extra_constraints, retry_feedback)
        .map_err(|error| ComposePlanError::Prompt(error.to_string()))?;
    let raw = generate_text(&prompt).map_err(ComposePlanError::Llm)?;
    // batch全体がJSONとして読めない場合は，全taskを単独retry候補にする．
    let composed = match parse_composed_document(&raw) {
        Ok(composed) => composed,
        Err(error) => {
            return Ok(attempts
                .iter()
                .map(|attempt| TaskFailure {
                    index: attempt.index,
                    task_id: scaffold.tasks[attempt.index].id.clone(),
                    retry_count: attempt.retry_count,
                    errors: vec![format!("生成結果JSONを読めません: {error}")],
                    reason: FailureReason::Content,
                    terminal_cause: TerminalCause::Content,
                    scope: FailureScope::QBlock,
                })
                .collect());
        }
    };

    let batch_intermediate = IntermediateDocument {
        meta: intermediate.meta.clone(),
        qblocks: attempts
            .iter()
            .map(|attempt| intermediate.qblocks[attempt.index].clone())
            .collect(),
    };
    // HashMap化の前に応答全体を確認し，知らないIDは対応付け不能としてbatchを再試行する．
    let mut preflight_issues = preflight_composed_questions(&batch_intermediate, &composed);
    // 単独retryでも，中間表現全体にある期待ID重複は解消されない．
    let expected_duplicates = preflight_composed_questions(
        intermediate,
        &ComposedDocument {
            questions: Vec::new(),
        },
    )
    .into_iter()
    .filter(|issue| matches!(issue, ComposeMergeIssue::DuplicateExpectedQuestionId { .. }));
    for issue in expected_duplicates {
        if !preflight_issues.contains(&issue) {
            preflight_issues.insert(0, issue);
        }
    }
    let unknown_id = preflight_issues.iter().find_map(|issue| match issue {
        ComposeMergeIssue::UnknownQuestionId { id } => Some(id.clone()),
        _ => None,
    });

    let mut questions_by_id = composed
        .questions
        .into_iter()
        .map(|question| (question.id.clone(), question))
        .collect::<HashMap<_, _>>();
    let mut failures = Vec::new();

    // JSONとして読めた後は，taskごとに不足・検証失敗を分けて扱う．
    for attempt in attempts {
        let _compose_mode = attempt.mode;
        let qblock = &intermediate.qblocks[attempt.index];
        let id_issue = preflight_issues.iter().find(|issue| match issue {
            ComposeMergeIssue::DuplicateExpectedQuestionId { id }
            | ComposeMergeIssue::DuplicateQuestionId { id }
            | ComposeMergeIssue::MissingQuestionId { id } => id == &qblock.id,
            ComposeMergeIssue::UnknownQuestionId { .. } => false,
        });
        if let Some(issue) = id_issue {
            failures.push(TaskFailure {
                index: attempt.index,
                task_id: qblock.id.clone(),
                retry_count: attempt.retry_count,
                errors: vec![format!("{}: {:?}", qblock.id, issue)],
                reason: FailureReason::Content,
                terminal_cause: TerminalCause::Content,
                scope: FailureScope::QBlock,
            });
            continue;
        }
        let Some(question) = questions_by_id.remove(&qblock.id) else {
            failures.push(TaskFailure {
                index: attempt.index,
                task_id: qblock.id.clone(),
                retry_count: attempt.retry_count,
                errors: vec![format!("{}: LLM出力にidが含まれていません", qblock.id)],
                reason: FailureReason::Content,
                terminal_cause: TerminalCause::Content,
                scope: FailureScope::QBlock,
            });
            continue;
        };

        let generated = try_merge_composed_questions(
            &IntermediateDocument {
                meta: intermediate.meta.clone(),
                qblocks: vec![qblock.clone()],
            },
            ComposedDocument {
                questions: vec![question.clone()],
            },
        )
        .map_err(|error| ComposePlanError::Validation {
            id: qblock.id.clone(),
            errors: error
                .issues
                .iter()
                .map(|issue| format!("{issue:?}"))
                .collect(),
        })?;
        let intermediate_json = serde_json::to_string(&IntermediateDocument {
            meta: intermediate.meta.clone(),
            qblocks: vec![qblock.clone()],
        })
        .map_err(|error| ComposePlanError::Json(error.to_string()))?;
        let report = validate_generated_document(&intermediate_json, &generated);

        if report.is_valid() {
            completed.insert(qblock.id.clone(), question);
        } else {
            failures.push(TaskFailure {
                index: attempt.index,
                task_id: qblock.id.clone(),
                retry_count: attempt.retry_count,
                errors: build_retry_feedback(qblock.id.as_str(), &generated, &report.errors),
                reason: FailureReason::Content,
                terminal_cause: TerminalCause::Content,
                scope: FailureScope::QBlock,
            });
        }
    }

    if let Some(id) = unknown_id {
        return Err(ComposePlanError::Validation {
            id,
            errors: vec!["unknown-id".to_string()],
        });
    }
    Ok(failures)
}

/// 失敗taskをretry queueへ戻す．retry上限を超えたらエラーにする．
fn enqueue_failures(
    retry_queue: &mut Vec<TaskAttempt>,
    failures: Vec<TaskFailure>,
    policy: BatchPolicy,
) -> Result<(), ComposePlanError> {
    for failure in failures {
        if failure.retry_count >= policy.max_retry_count {
            return Err(ComposePlanError::Validation {
                id: failure.task_id,
                errors: failure.errors,
            });
        }

        retry_queue.push(TaskAttempt {
            index: failure.index,
            retry_count: failure.retry_count + 1,
            mode: ComposeMode::SingleTask,
            feedback: failure.errors,
            retry_group: None,
            max_batch_size: Some(1),
        });
    }

    Ok(())
}

/// LLM応答からJSON部分を取り出してComposedDocumentとして読む．
fn parse_composed_document(raw: &str) -> Result<ComposedDocument, serde_json::Error> {
    let candidate = extract_json_candidate(raw);
    serde_json::from_str(candidate)
}

/// 検証エラーを次回promptへ渡す日本語フィードバックへ変換する．
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

/// retry promptには問題文・解答値を渡さず、修正すべき分類だけを渡す．
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
#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::compose::{ComposeBatchOutput, ComposeMetadata, ComposedItem, IdentityComposer};
    use crate::json::{
        IntermediateDocument, IntermediateMeta, IntermediateQBlock, IntermediateTarget,
    };
    use crate::planner::{compose_with_question_composer, compose_with_question_composer_observed};
    use crate::scaffold::build_scaffold_document;
    use crate::{ComposeEvent, EventSink, RunContext};

    use super::*;

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<ComposeEvent>>);

    impl EventSink for RecordingSink {
        fn emit(&self, event: ComposeEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    fn intermediate() -> IntermediateDocument {
        IntermediateDocument {
            meta: IntermediateMeta {
                source: "input.md".to_string(),
            },
            qblocks: vec![
                IntermediateQBlock {
                    id: "q1".to_string(),
                    section: None,
                    source_text: "短期記憶はワーキングメモリである。".to_string(),
                    targets: vec![IntermediateTarget {
                        answer: "ワーキングメモリ".to_string(),
                        target_type: "term".to_string(),
                    }],
                    warnings: Vec::new(),
                },
                IntermediateQBlock {
                    id: "q2".to_string(),
                    section: None,
                    source_text: "容量は7±2である。".to_string(),
                    targets: vec![IntermediateTarget {
                        answer: "7±2".to_string(),
                        target_type: "number".to_string(),
                    }],
                    warnings: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn retries_only_failed_task() {
        let intermediate = intermediate();
        let scaffold = build_scaffold_document(&intermediate);
        let mut calls = 0;
        let mut generator = |_prompt: &str| {
            calls += 1;
            if calls == 1 {
                Ok(r#"{"questions":[{"id":"q1","question":"短期記憶は＿＿＿である。"},{"id":"q2","question":"容量は7±2である。"}]}"#.to_string())
            } else {
                Ok(r#"{"questions":[{"id":"q2","question":"容量は＿＿＿である。"}]}"#.to_string())
            }
        };

        let composed = compose_with_estimator(
            &intermediate,
            &scaffold,
            BatchPolicy {
                max_tasks_per_batch: 8,
                max_estimated_input_tokens: 12_000,
                max_estimated_output_tokens: 12_000,
                max_blanks_per_batch: 64,
                max_retry_count: 2,
                max_concurrent_batches: 1,
            },
            &[],
            &CharHeuristicTokenEstimator,
            &mut generator,
        )
        .expect("planner should retry q2");

        assert_eq!(composed.questions.len(), 2);
        assert_eq!(calls, 2);
    }

    #[test]
    fn unknown_id_is_fatal_without_regenerating_known_tasks() {
        let intermediate = intermediate();
        let scaffold = build_scaffold_document(&intermediate);
        let mut calls = 0;
        let mut generator = |_prompt: &str| {
            calls += 1;
            Ok(match calls {
                1 => r#"{"questions":[{"id":"q1","question":"短期記憶は＿＿＿である。"},{"id":"unknown","question":"x"}]}"#,
                2 => r#"{"questions":[{"id":"q2","question":"容量は＿＿＿である。"}]}"#,
                _ => r#"{"questions":[{"id":"q1","question":"短期記憶は＿＿＿である。"}]}"#,
            }
            .to_string())
        };

        let error = compose_with_estimator(
            &intermediate,
            &scaffold,
            BatchPolicy {
                max_tasks_per_batch: 8,
                max_estimated_input_tokens: 12_000,
                max_estimated_output_tokens: 12_000,
                max_blanks_per_batch: 64,
                max_retry_count: 2,
                max_concurrent_batches: 1,
            },
            &[],
            &CharHeuristicTokenEstimator,
            &mut generator,
        )
        .unwrap_err();

        assert!(matches!(error, ComposePlanError::Validation { .. }));
        assert_eq!(calls, 1);
    }

    #[test]
    fn port_entry_runs_identity_composer_end_to_end() {
        let intermediate = intermediate();
        let scaffold = build_scaffold_document(&intermediate);

        let generated = compose_with_question_composer(
            &intermediate,
            &scaffold,
            ComposeExecutionPolicy::default(),
            &IdentityComposer,
        )
        .expect("identity output should validate");

        assert_eq!(generated.questions.len(), 2);
        assert_eq!(generated.questions[0].id, "q1");
        assert_eq!(
            generated.questions[0].question,
            scaffold.tasks[0].scaffold_question
        );
    }

    struct PromptVersionComposer(Mutex<Option<String>>);

    impl QuestionComposer for PromptVersionComposer {
        fn compose(
            &self,
            request: &ComposeBatchRequest,
        ) -> Result<ComposeBatchOutput, ComposeError> {
            *self.0.lock().unwrap() = Some(request.prompt_version.clone());
            Ok(ComposeBatchOutput {
                items: request
                    .tasks
                    .iter()
                    .map(|task| ComposedItem {
                        id: task.id.clone(),
                        question: task.scaffold_question.clone(),
                    })
                    .collect(),
                metadata: ComposeMetadata::default(),
            })
        }
    }

    #[test]
    fn standard_port_uses_compose_v2_prompt() {
        let intermediate = intermediate();
        let scaffold = build_scaffold_document(&intermediate);
        let composer = PromptVersionComposer(Mutex::new(None));

        compose_with_question_composer(
            &intermediate,
            &scaffold,
            ComposeExecutionPolicy::default(),
            &composer,
        )
        .expect("recorded composer output should validate");

        assert_eq!(composer.0.lock().unwrap().as_deref(), Some("compose-v2"));
    }

    struct FirstBatchInvalidThenProvider(Mutex<u32>);

    impl QuestionComposer for FirstBatchInvalidThenProvider {
        fn compose(
            &self,
            request: &ComposeBatchRequest,
        ) -> Result<ComposeBatchOutput, ComposeError> {
            let mut calls = self.0.lock().unwrap();
            *calls += 1;
            Ok(ComposeBatchOutput {
                items: request
                    .tasks
                    .iter()
                    .map(|task| ComposedItem {
                        id: task.id.clone(),
                        question: if *calls == 1 {
                            "invalid".to_string()
                        } else {
                            format!("provider {}", task.scaffold_question)
                        },
                    })
                    .collect(),
                metadata: ComposeMetadata::default(),
            })
        }
    }

    #[test]
    fn content_terminal_runs_later_batches_and_keeps_their_results() {
        let mut intermediate = intermediate();
        for (id, answer) in [("q3", "three"), ("q4", "four")] {
            intermediate.qblocks.push(IntermediateQBlock {
                id: id.to_string(),
                section: None,
                source_text: answer.to_string(),
                targets: vec![IntermediateTarget {
                    answer: answer.to_string(),
                    target_type: "term".to_string(),
                }],
                warnings: Vec::new(),
            });
        }
        let scaffold = build_scaffold_document(&intermediate);
        let composer = FirstBatchInvalidThenProvider(Mutex::new(0));

        let error = compose_with_question_composer(
            &intermediate,
            &scaffold,
            ComposeExecutionPolicy {
                batch_policy: BatchPolicy {
                    max_tasks_per_batch: 2,
                    max_estimated_input_tokens: 12_000,
                    max_estimated_output_tokens: 12_000,
                    max_blanks_per_batch: 64,
                    max_retry_count: 0,
                    max_concurrent_batches: 1,
                },
                max_content_retries: 0,
            },
            &composer,
        )
        .unwrap_err();

        assert_eq!(*composer.0.lock().unwrap(), 2);
        let ComposePlanError::Partial {
            document,
            failed_ids,
            failed_reasons,
        } = error
        else {
            panic!("content terminal must produce a partial result");
        };
        assert_eq!(
            document
                .questions
                .iter()
                .map(|question| question.id.as_str())
                .collect::<Vec<_>>(),
            vec!["q3", "q4"]
        );
        assert!(document
            .questions
            .iter()
            .all(|question| question.question.starts_with("provider")));
        assert_eq!(failed_ids, vec!["q1", "q2"]);
        assert_eq!(failed_reasons, vec![FailureReason::Content; 2]);
    }

    #[test]
    fn port_policy_rejects_zero_limits_but_allows_oversize_singleton() {
        let intermediate = intermediate();
        let scaffold = build_scaffold_document(&intermediate);
        let zero = ComposeExecutionPolicy {
            batch_policy: BatchPolicy {
                max_tasks_per_batch: 0,
                max_estimated_input_tokens: 1,
                max_estimated_output_tokens: 1,
                max_blanks_per_batch: 1,
                max_retry_count: 0,
                max_concurrent_batches: 1,
            },
            max_content_retries: 0,
        };
        assert!(matches!(
            compose_with_question_composer(&intermediate, &scaffold, zero, &IdentityComposer),
            Err(ComposePlanError::Configuration { id }) if id == "policy"
        ));

        let oversize = ComposeExecutionPolicy {
            batch_policy: BatchPolicy {
                max_tasks_per_batch: 1,
                max_estimated_input_tokens: 1,
                max_estimated_output_tokens: 1,
                max_blanks_per_batch: 1,
                max_retry_count: 0,
                max_concurrent_batches: 1,
            },
            max_content_retries: 0,
        };
        let generated =
            compose_with_question_composer(&intermediate, &scaffold, oversize, &IdentityComposer)
                .expect("soft batch budgets must allow an oversized singleton");
        assert_eq!(generated.questions.len(), 2);
    }

    struct EmptyThenValidComposer(Mutex<u32>);

    impl QuestionComposer for EmptyThenValidComposer {
        fn compose(
            &self,
            request: &ComposeBatchRequest,
        ) -> Result<ComposeBatchOutput, ComposeError> {
            let mut calls = self.0.lock().unwrap();
            *calls += 1;
            if *calls == 1 {
                return Err(ComposeError::EmptyResponse);
            }
            Ok(ComposeBatchOutput {
                items: request
                    .tasks
                    .iter()
                    .map(|task| ComposedItem {
                        id: task.id.clone(),
                        question: task.scaffold_question.clone(),
                    })
                    .collect(),
                metadata: ComposeMetadata::default(),
            })
        }
    }

    #[test]
    fn port_retries_only_invalid_or_empty_provider_content() {
        let intermediate = intermediate();
        let scaffold = build_scaffold_document(&intermediate);
        let composer = EmptyThenValidComposer(Mutex::new(0));
        let generated = compose_with_question_composer(
            &intermediate,
            &scaffold,
            ComposeExecutionPolicy {
                batch_policy: BatchPolicy::gemini_default(),
                max_content_retries: 1,
            },
            &composer,
        )
        .unwrap();
        assert_eq!(generated.questions.len(), 2);
        // 初回batchの空応答後は、task単位で再試行する。
        assert_eq!(*composer.0.lock().unwrap(), 3);
    }

    #[test]
    fn batch_level_content_failure_halves_the_first_retry_batch() {
        let mut intermediate = intermediate();
        for (id, answer) in [("q3", "three"), ("q4", "four")] {
            intermediate.qblocks.push(IntermediateQBlock {
                id: id.to_string(),
                section: None,
                source_text: format!("{answer} is a value."),
                targets: vec![IntermediateTarget {
                    answer: answer.to_string(),
                    target_type: "term".to_string(),
                }],
                warnings: Vec::new(),
            });
        }
        let scaffold = build_scaffold_document(&intermediate);
        let composer = EmptyThenValidComposer(Mutex::new(0));

        let generated = compose_with_question_composer(
            &intermediate,
            &scaffold,
            ComposeExecutionPolicy {
                batch_policy: BatchPolicy::gemini_default(),
                max_content_retries: 2,
            },
            &composer,
        )
        .unwrap();

        assert_eq!(generated.questions.len(), 4);
        // 4 qblocks in the failed batch shrink to two 2-qblock retries.
        assert_eq!(*composer.0.lock().unwrap(), 3);
    }

    #[test]
    fn public_composer_error_payload_matches_compose_error_display() {
        let errors = [
            ComposeError::Authentication,
            ComposeError::RateLimited {
                kind: RateLimitKind::Unknown,
            },
            ComposeError::Timeout,
            ComposeError::Transport,
            ComposeError::Api {
                status: 503,
                retryable: true,
            },
            ComposeError::Api {
                status: 400,
                retryable: false,
            },
            ComposeError::InvalidResponse,
            ComposeError::EmptyResponse,
        ];

        for error in errors {
            let expected = error.to_string();
            assert_eq!(map_composer_error(error), ComposePlanError::Llm(expected));
        }
    }

    #[test]
    fn observed_identity_has_no_provider_and_content_retry_increments_attempt() {
        let intermediate = intermediate();
        let scaffold = build_scaffold_document(&intermediate);
        let sink = RecordingSink::default();
        let context = RunContext::default();
        let composer = EmptyThenValidComposer(Mutex::new(0));
        compose_with_question_composer_observed(
            &intermediate,
            &scaffold,
            ComposeExecutionPolicy {
                batch_policy: BatchPolicy::gemini_default(),
                max_content_retries: 1,
            },
            &composer,
            &context,
            &sink,
        )
        .unwrap();
        let events = sink.0.lock().unwrap();
        let q1_attempts = events
            .iter()
            .filter(|event| {
                event.event == ComposeEventKind::Attempt && event.task_id.as_deref() == Some("q1")
            })
            .map(|event| event.attempt.unwrap())
            .collect::<Vec<_>>();
        assert_eq!(q1_attempts, vec![0, 1]);

        let identity_sink = RecordingSink::default();
        compose_with_question_composer_observed(
            &intermediate,
            &scaffold,
            ComposeExecutionPolicy::default(),
            &IdentityComposer,
            &context,
            &identity_sink,
        )
        .unwrap();
        assert!(identity_sink
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.event == ComposeEventKind::Attempt)
            .all(|event| event.provider.is_none() && event.model.is_none()));
    }

    #[test]
    fn first_content_retry_is_rebatched_and_last_retry_is_single_task() {
        let scaffold = ScaffoldDocument {
            tasks: (0..3)
                .map(|index| ScaffoldTask {
                    id: format!("q{index}"),
                    source_text: "短い本文".to_string(),
                    cloze_template: "短い＿＿＿".to_string(),
                    scaffold_question: "短い＿＿＿".to_string(),
                    blank_count: 1,
                    answers: vec!["本文".to_string()],
                })
                .collect(),
        };
        let policy = BatchPolicy {
            max_tasks_per_batch: 3,
            max_estimated_input_tokens: 10_000,
            max_estimated_output_tokens: 10_000,
            max_blanks_per_batch: 32,
            max_retry_count: 2,
            max_concurrent_batches: 1,
        };
        let first = (0..3)
            .map(|index| TaskAttempt {
                index,
                retry_count: 1,
                mode: ComposeMode::Batched,
                feedback: vec!["retry".into()],
                retry_group: None,
                max_batch_size: None,
            })
            .collect();
        assert_eq!(
            plan_retry_attempts(&scaffold, first, policy, &CharHeuristicTokenEstimator)
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            vec![3]
        );
        let last = (0..3)
            .map(|index| TaskAttempt {
                index,
                retry_count: 2,
                mode: ComposeMode::SingleTask,
                feedback: vec!["retry".into()],
                retry_group: None,
                max_batch_size: Some(1),
            })
            .collect();
        assert_eq!(
            plan_retry_attempts(&scaffold, last, policy, &CharHeuristicTokenEstimator)
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            vec![1, 1, 1]
        );
    }
}
