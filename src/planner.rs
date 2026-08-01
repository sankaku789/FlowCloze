//! Adaptive Compose Planner.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::compose::{
    compose_task_from_scaffold, extract_json_candidate, merge_composed_questions,
    normalize_sentinel_question, preflight_composed_questions, try_merge_composed_questions,
    ComposeBatchRequest, ComposeError, ComposeMergeIssue, ComposedDocument, ComposedQuestion,
    QuestionComposer, WritingStyle,
};
use crate::json::IntermediateDocument;
use crate::observability::{
    fnv1a_64, ComposeEvent, ComposeEventKind, EventSink, NoopEventSink, RunContext,
};
use crate::progress::{NoopProgressSink, ProgressEvent, ProgressSink, RetryResult};
use crate::prompt::{build_compose_request_prompt, build_question_composer_prompt};
use crate::scaffold::{ScaffoldDocument, ScaffoldTask};
use crate::validation::{
    validate_generated_document, validate_generated_documents,
    validate_generated_documents_with_leakage_baselines, GeneratedDocument, ValidationError,
};

/// prompt長の概算を差し替え可能にするためのtrait．
pub trait TokenEstimator {
    /// 文字列がLLM入力で消費しそうなtoken数を概算する．
    fn estimate(&self, text: &str) -> usize;
}

/// 初期実装で使う文字種ベースの簡易token推定器．
#[derive(Debug, Clone, Copy, Default)]
pub struct CharHeuristicTokenEstimator;

impl TokenEstimator for CharHeuristicTokenEstimator {
    fn estimate(&self, text: &str) -> usize {
        text.chars()
            .map(|ch| {
                if is_japanese_char(ch) || ch.is_ascii_alphanumeric() {
                    1
                } else {
                    0
                }
            })
            .sum::<usize>()
            .max(text.chars().count() / 4)
    }
}

/// backendごとのbatch作成とretry上限を表す設定．
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchPolicy {
    /// 1回のLLM呼び出しに含めるtask数の上限．
    pub max_tasks_per_batch: usize,
    /// 1回のLLM呼び出しに含める入力token概算の上限．
    pub max_estimated_input_tokens: usize,
    /// task単位で再試行する最大回数．
    pub max_retry_count: u32,
    /// 将来の並列実行用の上限値．初期実装では逐次実行する．
    pub max_concurrent_batches: usize,
}

/// port経由の標準実行でのみ使うcontent retry設定．
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposeExecutionPolicy {
    pub batch_policy: BatchPolicy,
    pub max_content_retries: u32,
}

impl Default for ComposeExecutionPolicy {
    fn default() -> Self {
        Self {
            batch_policy: BatchPolicy::gemini_default(),
            max_content_retries: 2,
        }
    }
}

impl BatchPolicy {
    /// Gemini向けの初期policy．API request数を抑えるためbatchを大きめにする．
    pub fn gemini_default() -> Self {
        Self {
            max_tasks_per_batch: 8,
            max_estimated_input_tokens: 12_000,
            max_retry_count: 2,
            max_concurrent_batches: 3,
        }
    }

    /// Local LLM向けの初期policy．安定性を優先して小さめのbatchにする．
    pub fn local_default() -> Self {
        Self {
            max_tasks_per_batch: 2,
            max_estimated_input_tokens: 4_000,
            max_retry_count: 2,
            max_concurrent_batches: 1,
        }
    }
}

/// compose plannerで発生しうる失敗を呼び出し側へ伝えるエラー．
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposePlanError {
    /// port標準入口のpolicyまたはtask設定が不正だった．
    Configuration { id: String },
    /// prompt構築に失敗した．
    Prompt(String),
    /// LLMクライアント呼び出しに失敗した．
    Llm(String),
    /// LLM応答をJSONとして解釈できなかった．
    Json(String),
    /// retry上限後もtaskの検証に失敗した．
    Validation { id: String, errors: Vec<String> },
    /// 一部taskを確定済みのまま、content retry上限に達した。
    Partial {
        document: GeneratedDocument,
        failed_ids: Vec<String>,
        failed_reasons: Vec<FailureReason>,
    },
}

/// fallback時に公開する未解決taskの失敗分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    Content,
    Transport,
}

/// provider起因の実際の終端分類。公開エラー形状とは分離して内部で保持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalCause {
    Content,
    Authentication,
    Configuration,
    RateLimited,
    Timeout,
    Transport,
    Api,
}

impl std::fmt::Display for ComposePlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration { id } => write!(f, "compose configuration error: task={id}"),
            Self::Prompt(_) => write!(f, "compose error: prompt"),
            Self::Llm(_) => write!(f, "compose error: llm"),
            Self::Json(_) => write!(f, "compose error: json"),
            Self::Validation { id, .. } => write!(f, "compose error: validation task={id}"),
            Self::Partial { .. } => write!(f, "compose error: partial validation"),
        }
    }
}

impl std::error::Error for ComposePlanError {}

/// taskが現在どのcompose戦略で処理されているかを表す．
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComposeMode {
    /// 複数taskをまとめた通常batchで処理する状態．
    Batched,
    /// 失敗後にtask単独で再試行する状態．
    SingleTask,
}

/// retry queue内で追跡するtaskの状態．
#[derive(Debug, Clone)]
struct TaskAttempt {
    /// scaffold.tasks / intermediate.qblocks のindex．
    index: usize,
    /// このtaskを再試行した回数．
    retry_count: u32,
    /// 現在のcompose mode．将来のログ出力にも使う．
    mode: ComposeMode,
    /// 前回失敗時の検証理由．単独retry promptへ渡す．
    feedback: Vec<String>,
}

/// 1 taskの生成・検証に失敗した理由．
#[derive(Debug)]
struct TaskFailure {
    /// 失敗したtaskのindex．
    index: usize,
    /// indexではなく公開task IDを失敗報告へ残す．
    task_id: String,
    /// 失敗時点のretry回数．
    retry_count: u32,
    /// 次回promptへ渡すための検証フィードバック．
    errors: Vec<String>,
    reason: FailureReason,
    terminal_cause: TerminalCause,
}

/// 公開エラーへ変換する前だけ、実際に実行を止めた原因を保持する。
#[derive(Debug)]
pub(crate) struct ComposeExecutionError {
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

/// policy 検証済みの初回 batch 計画。開始表示と実行で同じ計画を共有する。
#[derive(Debug, Clone)]
pub struct PreparedComposePlan {
    batches: Vec<Vec<TaskAttempt>>,
}

impl PreparedComposePlan {
    pub fn batch_count(&self) -> usize {
        self.batches.len()
    }
}

/// 既定の文字数heuristicを使ってadaptive composeを実行する．
pub fn compose_with_adaptive_planner<F>(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    policy: BatchPolicy,
    extra_constraints: &[String],
    mut generate_text: F,
) -> Result<ComposedDocument, ComposePlanError>
where
    F: FnMut(&str) -> Result<String, String>,
{
    let estimator = CharHeuristicTokenEstimator;
    compose_with_estimator(
        intermediate,
        scaffold,
        policy,
        extra_constraints,
        &estimator,
        &mut generate_text,
    )
}

/// QuestionComposer portを使う標準のcompose入口．
/// CoreがID照合、retry、検証、固定フィールドの合成を一貫して担当する．
pub fn compose_with_question_composer(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
    composer: &dyn QuestionComposer,
) -> Result<GeneratedDocument, ComposePlanError> {
    let context = RunContext::new();
    let sink = NoopEventSink;
    compose_with_question_composer_observed(
        intermediate,
        scaffold,
        policy,
        composer,
        &context,
        &sink,
    )
}

/// port標準入口に、本文を出力しない観測hookを追加した版。
pub fn compose_with_question_composer_observed(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
    composer: &dyn QuestionComposer,
    context: &RunContext,
    sink: &dyn EventSink,
) -> Result<GeneratedDocument, ComposePlanError> {
    let progress = NoopProgressSink;
    compose_with_question_composer_observed_with_progress(
        intermediate,
        scaffold,
        policy,
        composer,
        context,
        sink,
        &progress,
    )
}

/// port 実行へ本文を含まない進捗 sink を注入する入口。
pub fn compose_with_question_composer_observed_with_progress(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
    composer: &dyn QuestionComposer,
    context: &RunContext,
    sink: &dyn EventSink,
    progress: &dyn ProgressSink,
) -> Result<GeneratedDocument, ComposePlanError> {
    compose_with_question_composer_observed_with_constraints_with_progress(
        intermediate,
        scaffold,
        policy,
        composer,
        &[],
        context,
        sink,
        progress,
    )
}

/// 追加制約をprovider requestだけへ渡す観測付き入口。
#[allow(clippy::too_many_arguments)]
pub fn compose_with_question_composer_observed_with_constraints(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
    composer: &dyn QuestionComposer,
    extra_constraints: &[String],
    context: &RunContext,
    sink: &dyn EventSink,
) -> Result<GeneratedDocument, ComposePlanError> {
    let progress = NoopProgressSink;
    compose_with_question_composer_observed_with_constraints_with_progress(
        intermediate,
        scaffold,
        policy,
        composer,
        extra_constraints,
        context,
        sink,
        &progress,
    )
}

/// 追加制約をprovider requestだけへ渡し、進捗sinkも注入する入口。
#[allow(clippy::too_many_arguments)]
pub fn compose_with_question_composer_observed_with_constraints_with_progress(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
    composer: &dyn QuestionComposer,
    extra_constraints: &[String],
    context: &RunContext,
    sink: &dyn EventSink,
    progress: &dyn ProgressSink,
) -> Result<GeneratedDocument, ComposePlanError> {
    compose_with_question_composer_observed_with_constraints_and_leakage_baselines(
        intermediate,
        scaffold,
        policy,
        composer,
        extra_constraints,
        context,
        sink,
        progress,
        None,
    )
}

/// located経路で確定したtarget外本文の漏洩基準を使う内部入口。
#[allow(clippy::too_many_arguments)]
pub(crate) fn compose_with_question_composer_observed_with_constraints_and_leakage_baselines(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
    composer: &dyn QuestionComposer,
    extra_constraints: &[String],
    context: &RunContext,
    sink: &dyn EventSink,
    progress: &dyn ProgressSink,
    leakage_baselines: Option<&HashMap<String, Vec<usize>>>,
) -> Result<GeneratedDocument, ComposePlanError> {
    compose_with_question_composer_prepared_with_terminal_cause(
        intermediate,
        scaffold,
        policy,
        composer,
        extra_constraints,
        context,
        sink,
        progress,
        leakage_baselines,
        None,
    )
    .map_err(ComposeExecutionError::into_public)
}

/// 初回計画を作成済みの呼び出し側用。retry は計画外の task 単位で実行する。
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub(crate) fn compose_with_question_composer_prepared(
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
) -> Result<GeneratedDocument, ComposePlanError> {
    compose_with_question_composer_prepared_with_terminal_cause(
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
    .map_err(ComposeExecutionError::into_public)
}

/// orchestration用に、公開エラーへ落とす前の終端原因を返す。
#[allow(clippy::too_many_arguments)]
pub(crate) fn compose_with_question_composer_prepared_with_terminal_cause(
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
    validate_port_policy(scaffold, policy).map_err(ComposeExecutionError::from_error)?;
    let estimator = CharHeuristicTokenEstimator;
    let mut completed = HashMap::<String, ComposedQuestion>::new();
    let mut retry_queue = Vec::<TaskAttempt>::new();

    let mut terminal_failures = Vec::new();
    let batches = prepared
        .map(|plan| plan.batches.clone())
        .unwrap_or_else(|| plan_batches(scaffold, policy.batch_policy, &estimator));
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
            policy.batch_policy.max_concurrent_batches,
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
                }
            }));
            return Err(partial_plan_error(
                intermediate,
                &completed,
                terminal_failures,
            ));
        }
    }

    while let Some(attempt) = retry_queue.pop() {
        if completed.contains_key(&scaffold.tasks[attempt.index].id) {
            continue;
        }
        let task_index = attempt.index;
        let retry_count = attempt.retry_count;
        let failures = run_port_batch(
            intermediate,
            scaffold,
            &[attempt],
            0,
            composer,
            &mut completed,
            context,
            sink,
            policy.batch_policy.max_concurrent_batches,
            extra_constraints,
            leakage_baselines,
        )?;
        let task_id = scaffold.tasks[task_index].id.clone();
        let terminal =
            enqueue_port_failures(&mut retry_queue, failures, policy.max_content_retries);
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
            result,
        });
        terminal_failures.extend(terminal);
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

fn validate_port_policy(
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
) -> Result<(), ComposePlanError> {
    if policy.max_content_retries > 2
        || policy.batch_policy.max_tasks_per_batch == 0
        || policy.batch_policy.max_estimated_input_tokens == 0
        || policy.batch_policy.max_concurrent_batches == 0
    {
        return Err(ComposePlanError::Configuration {
            id: "policy".to_string(),
        });
    }
    let estimator = CharHeuristicTokenEstimator;
    if let Some(task) = scaffold.tasks.iter().find(|task| {
        estimate_task_tokens(task, &estimator) > policy.batch_policy.max_estimated_input_tokens
    }) {
        return Err(ComposePlanError::Configuration {
            id: task.id.clone(),
        });
    }
    Ok(())
}

/// policy 検証と単独 task の token 予算検査を、実行前にまとめて行う。
/// 呼び出し側が開始イベントを出す前に使う。
pub fn prepare_initial_batch_count(
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
) -> Result<usize, ComposePlanError> {
    Ok(prepare_compose_plan(scaffold, policy)?.batch_count())
}

/// policy 検証と初回 batch の作成を一度だけ行う。
pub fn prepare_compose_plan(
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
) -> Result<PreparedComposePlan, ComposePlanError> {
    validate_port_policy(scaffold, policy)?;
    Ok(PreparedComposePlan {
        batches: plan_batches(scaffold, policy.batch_policy, &CharHeuristicTokenEstimator),
    })
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
        prompt_version: "compose-v1".to_string(),
        extra_constraints: extra_constraints.to_vec(),
        retry_feedback: attempts[0].feedback.clone(),
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
        ComposeError::RateLimited => "rate_limited",
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
        ComposeError::RateLimited => TerminalCause::RateLimited,
        ComposeError::Timeout => TerminalCause::Timeout,
        ComposeError::Transport => TerminalCause::Transport,
        ComposeError::Api { .. } => TerminalCause::Api,
    }
}

fn transport_fallbackable(error: &ComposeError) -> bool {
    matches!(
        error,
        ComposeError::RateLimited
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
        queue.push(TaskAttempt {
            index: failure.index,
            retry_count: failure.retry_count + 1,
            mode: ComposeMode::SingleTask,
            feedback: failure.errors,
        });
    }
    terminal
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

/// token budgetとtask数上限に従って初回batchを作る．
fn plan_batches<E>(
    scaffold: &ScaffoldDocument,
    policy: BatchPolicy,
    estimator: &E,
) -> Vec<Vec<TaskAttempt>>
where
    E: TokenEstimator,
{
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut current_tokens = 0;

    for (index, task) in scaffold.tasks.iter().enumerate() {
        let estimated_tokens = estimate_task_tokens(task, estimator);
        let would_exceed_tasks = current.len() >= policy.max_tasks_per_batch;
        let would_exceed_tokens = !current.is_empty()
            && current_tokens + estimated_tokens > policy.max_estimated_input_tokens;

        if would_exceed_tasks || would_exceed_tokens {
            batches.push(current);
            current = Vec::new();
            current_tokens = 0;
        }

        current.push(TaskAttempt {
            index,
            retry_count: 0,
            mode: ComposeMode::Batched,
            feedback: Vec::new(),
        });
        current_tokens += estimated_tokens;
    }

    if !current.is_empty() {
        batches.push(current);
    }

    batches
}

/// 初回実行だけのbatch数。retry/fallbackはこの計画に含めない。
pub fn initial_batch_count(scaffold: &ScaffoldDocument, policy: BatchPolicy) -> usize {
    plan_batches(scaffold, policy, &CharHeuristicTokenEstimator).len()
}

/// 初回計画の各batchに入るtask数を返す。
pub fn initial_batch_sizes(scaffold: &ScaffoldDocument, policy: BatchPolicy) -> Vec<usize> {
    plan_batches(scaffold, policy, &CharHeuristicTokenEstimator)
        .iter()
        .map(Vec::len)
        .collect()
}

/// 1 taskがprompt内で消費するtoken数を概算する．
fn estimate_task_tokens<E>(task: &ScaffoldTask, estimator: &E) -> usize
where
    E: TokenEstimator,
{
    estimator.estimate(&task.source_text)
        + estimator.estimate(&task.scaffold_question)
        + task
            .answers
            .iter()
            .map(|answer| estimator.estimate(answer))
            .sum::<usize>()
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

/// 簡易token推定で日本語として重めに数える文字種を判定する．
fn is_japanese_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}'
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::compose::{ComposeBatchOutput, ComposeMetadata, ComposedItem, IdentityComposer};
    use crate::json::{
        IntermediateDocument, IntermediateMeta, IntermediateQBlock, IntermediateTarget,
    };
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
    fn port_policy_rejects_zero_limits_and_oversize_before_composer_call() {
        let intermediate = intermediate();
        let scaffold = build_scaffold_document(&intermediate);
        let zero = ComposeExecutionPolicy {
            batch_policy: BatchPolicy {
                max_tasks_per_batch: 0,
                max_estimated_input_tokens: 1,
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
                max_retry_count: 0,
                max_concurrent_batches: 1,
            },
            max_content_retries: 0,
        };
        assert!(matches!(
            compose_with_question_composer(&intermediate, &scaffold, oversize, &IdentityComposer),
            Err(ComposePlanError::Configuration { id }) if id == "q1"
        ));
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
    fn public_composer_error_payload_matches_compose_error_display() {
        let errors = [
            ComposeError::Authentication,
            ComposeError::RateLimited,
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
}
