//! Adaptive Compose Planner.

use std::collections::{BTreeMap, HashMap, HashSet};
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
use crate::progress::{NoopProgressSink, ProgressEvent, ProgressSink, RetryCause, RetryResult};
use crate::prompt::{build_compose_request_prompt, build_question_composer_prompt};
use crate::quota::QuotaProfile;
use crate::rate_limit::RateLimitKind;
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
    /// 1回のLLM呼び出しに含めるqblock数の上限．
    pub max_tasks_per_batch: usize,
    /// 1回のLLM呼び出しに含める入力token概算のsoft上限．
    pub max_estimated_input_tokens: usize,
    /// 1回のLLM応答で生成させるtoken概算のsoft上限．
    pub max_estimated_output_tokens: usize,
    /// 1回のLLM呼び出しに含めるblank総数のsoft上限．
    pub max_blanks_per_batch: usize,
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
            max_tasks_per_batch: 12,
            max_estimated_input_tokens: 18_000,
            max_estimated_output_tokens: 6_000,
            max_blanks_per_batch: 24,
            max_retry_count: 2,
            max_concurrent_batches: 3,
        }
    }

    /// Local LLM向けの初期policy．安定性を優先して小さめのbatchにする．
    pub fn local_default() -> Self {
        Self {
            max_tasks_per_batch: 2,
            max_estimated_input_tokens: 4_000,
            max_estimated_output_tokens: 1_500,
            max_blanks_per_batch: 8,
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
    RateLimited { kind: RateLimitKind },
    Timeout,
    Transport,
    Api { status: u16 },
}

impl std::fmt::Display for ComposePlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration { id } => write!(f, "compose configuration error: task={id}"),
            Self::Prompt(_) => write!(f, "compose error: prompt"),
            Self::Llm(_) => write!(f, "compose error: llm"),
            Self::Json(_) => write!(f, "compose error: json"),
            Self::Validation { id, .. } => write!(f, "compose error: validation task={id}"),
            Self::Partial { .. } => write!(f, "compose error: generation incomplete"),
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

/// content failureがqblock固有か、batch全体の崩れかを区別する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureScope {
    QBlock,
    Batch { group: usize, previous_size: usize },
}

/// retry queue内で追跡するqblockの状態．
#[derive(Debug, Clone)]
struct TaskAttempt {
    /// scaffold.tasks / intermediate.qblocks のindex．
    index: usize,
    /// このqblockを再試行した回数．
    retry_count: u32,
    /// 現在のcompose mode．将来のログ出力にも使う．
    mode: ComposeMode,
    /// 前回失敗時の検証理由．retry promptへ渡す．
    feedback: Vec<String>,
    /// batch全体の失敗を別batch由来のretryと再結合しないためのgroup。
    retry_group: Option<usize>,
    /// batch全体の失敗時に次回batchを縮小するqblock数上限。
    max_batch_size: Option<usize>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QBlockCost {
    input_tokens: usize,
    output_tokens: usize,
    blanks: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct BatchLoad {
    input_tokens: usize,
    output_tokens: usize,
    blanks: usize,
    qblocks: usize,
}

impl BatchLoad {
    fn add(&mut self, cost: QBlockCost) {
        self.input_tokens = self.input_tokens.saturating_add(cost.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(cost.output_tokens);
        self.blanks = self.blanks.saturating_add(cost.blanks);
        self.qblocks = self.qblocks.saturating_add(1);
    }

    fn can_fit(&self, cost: QBlockCost, policy: BatchPolicy, max_qblocks: usize) -> bool {
        self.qblocks < max_qblocks
            && self.input_tokens.saturating_add(cost.input_tokens)
                <= policy.max_estimated_input_tokens
            && self.output_tokens.saturating_add(cost.output_tokens)
                <= policy.max_estimated_output_tokens
            && self.blanks.saturating_add(cost.blanks) <= policy.max_blanks_per_batch
    }
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

/// policy 検証済みの初回 batch 計画。開始表示と実行で同じ計画を共有する。
#[derive(Debug, Clone)]
pub struct PreparedComposePlan {
    batches: Vec<Vec<TaskAttempt>>,
    effective_policy: BatchPolicy,
}

/// API送信前に確認できる、1 qblock分の計画情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedQBlockSummary {
    /// 選択済みscaffold内での0始まりindex。
    pub index: usize,
    pub id: String,
    pub input_tokens: usize,
    pub expected_output_tokens: usize,
    pub blanks: usize,
    pub isolated_heavy: bool,
    pub oversized: bool,
}

/// 実際のPreparedComposePlanに含まれる1 batch分の集計。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedBatchSummary {
    pub qblocks: Vec<PreparedQBlockSummary>,
    pub input_tokens: usize,
    pub expected_output_tokens: usize,
    pub blanks: usize,
}

/// API送信前に表示するためのPreparedComposePlanの読み取り専用表現。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPlanSummary {
    pub batches: Vec<PreparedBatchSummary>,
    pub effective_policy: BatchPolicy,
}

impl PreparedComposePlan {
    pub fn batch_count(&self) -> usize {
        self.batches.len()
    }

    /// 実行時と同じbatchを再計算せず、そのまま表示用に展開する。
    pub fn summary(&self, scaffold: &ScaffoldDocument) -> PreparedPlanSummary {
        let estimator = CharHeuristicTokenEstimator;
        let batches = self
            .batches
            .iter()
            .map(|batch| {
                let mut load = BatchLoad::default();
                let qblocks = batch
                    .iter()
                    .map(|attempt| {
                        let task = &scaffold.tasks[attempt.index];
                        let cost = estimate_qblock_cost(task, &estimator);
                        load.add(cost);
                        PreparedQBlockSummary {
                            index: attempt.index,
                            id: task.id.clone(),
                            input_tokens: cost.input_tokens,
                            expected_output_tokens: cost.output_tokens,
                            blanks: cost.blanks,
                            isolated_heavy: batch.len() == 1
                                && is_heavy_qblock(cost, self.effective_policy),
                            oversized: exceeds_soft_budget(cost, self.effective_policy),
                        }
                    })
                    .collect();
                PreparedBatchSummary {
                    qblocks,
                    input_tokens: load.input_tokens,
                    expected_output_tokens: load.output_tokens,
                    blanks: load.blanks,
                }
            })
            .collect();
        PreparedPlanSummary {
            batches,
            effective_policy: self.effective_policy,
        }
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
    crate::executor::execute_legacy(
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
    crate::executor::execute_legacy(
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

fn validate_port_policy(
    _scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
) -> Result<(), ComposePlanError> {
    if policy.max_content_retries > 2
        || policy.batch_policy.max_tasks_per_batch == 0
        || policy.batch_policy.max_estimated_input_tokens == 0
        || policy.batch_policy.max_estimated_output_tokens == 0
        || policy.batch_policy.max_blanks_per_batch == 0
        || policy.batch_policy.max_concurrent_batches == 0
    {
        return Err(ComposePlanError::Configuration {
            id: "policy".to_string(),
        });
    }
    // BatchPolicyはpacking用のsoft budget。単独qblockが超える場合は
    // oversized singletonとして許可し、provider hard limitはquota側で検査する。
    Ok(())
}

/// policy 検証と初回batch計画を、実行前にまとめて行う。
/// qblock単独がsoft budgetを超える場合はsingletonとして許可する。
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
    prepare_compose_plan_with_quota(scaffold, policy, None)
}

pub fn prepare_compose_plan_with_quota(
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
    quota: Option<&QuotaProfile>,
) -> Result<PreparedComposePlan, ComposePlanError> {
    let estimator = CharHeuristicTokenEstimator;
    let effective_batch_policy =
        quota_adjusted_batch_policy(scaffold, policy.batch_policy, quota, &estimator)?;
    validate_port_policy(
        scaffold,
        ComposeExecutionPolicy {
            batch_policy: effective_batch_policy,
            ..policy
        },
    )?;
    Ok(PreparedComposePlan {
        batches: plan_batches(scaffold, effective_batch_policy, &estimator),
        effective_policy: effective_batch_policy,
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

fn quota_adjusted_batch_policy<E>(
    scaffold: &ScaffoldDocument,
    base: BatchPolicy,
    quota: Option<&QuotaProfile>,
    estimator: &E,
) -> Result<BatchPolicy, ComposePlanError>
where
    E: TokenEstimator,
{
    let Some(quota) = quota else {
        return Ok(base);
    };
    let mut policy = base;
    if let Some(tpm) = quota.tpm {
        let tpm = usize::try_from(tpm).unwrap_or(usize::MAX);
        if let Some(task) = scaffold
            .tasks
            .iter()
            .find(|task| estimate_task_tokens(task, estimator) > tpm)
        {
            return Err(ComposePlanError::Configuration {
                id: task.id.clone(),
            });
        }
        policy.max_estimated_input_tokens = policy.max_estimated_input_tokens.min(tpm);
    }
    let Some(request_budget) = quota.request_budget() else {
        return Ok(policy);
    };
    if request_budget == 0 {
        return Err(ComposePlanError::Configuration {
            id: "quota-budget".to_string(),
        });
    }

    let hard_tasks = quota
        .adaptive_max_tasks_per_batch
        .unwrap_or(policy.max_tasks_per_batch)
        .max(policy.max_tasks_per_batch);
    let mut hard_input = quota
        .adaptive_max_input_tokens
        .unwrap_or(policy.max_estimated_input_tokens)
        .max(policy.max_estimated_input_tokens);
    if let Some(tpm) = quota.tpm {
        hard_input = hard_input.min(usize::try_from(tpm).unwrap_or(usize::MAX));
    }
    let hard_output = quota
        .adaptive_max_output_tokens
        .unwrap_or(policy.max_estimated_output_tokens)
        .max(policy.max_estimated_output_tokens);
    let hard_blanks = quota
        .adaptive_max_blanks_per_batch
        .unwrap_or(policy.max_blanks_per_batch)
        .max(policy.max_blanks_per_batch);

    let mut batch_count = plan_batches(scaffold, policy, estimator).len();
    while batch_count > request_budget {
        let mut changed = false;
        if policy.max_tasks_per_batch < hard_tasks {
            policy.max_tasks_per_batch += 1;
            changed = true;
        }
        if policy.max_estimated_input_tokens < hard_input {
            let remaining = hard_input - policy.max_estimated_input_tokens;
            policy.max_estimated_input_tokens += remaining.clamp(1, 1_000);
            changed = true;
        }
        if policy.max_estimated_output_tokens < hard_output {
            let remaining = hard_output - policy.max_estimated_output_tokens;
            policy.max_estimated_output_tokens += remaining.clamp(1, 500);
            changed = true;
        }
        if policy.max_blanks_per_batch < hard_blanks {
            policy.max_blanks_per_batch += 1;
            changed = true;
        }
        if !changed {
            break;
        }
        batch_count = plan_batches(scaffold, policy, estimator).len();
    }
    if batch_count > request_budget {
        return Err(ComposePlanError::Configuration {
            id: "quota-budget".to_string(),
        });
    }
    Ok(policy)
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

fn plan_batches<E>(
    scaffold: &ScaffoldDocument,
    policy: BatchPolicy,
    estimator: &E,
) -> Vec<Vec<TaskAttempt>>
where
    E: TokenEstimator,
{
    let attempts = scaffold
        .tasks
        .iter()
        .enumerate()
        .map(|(index, _)| TaskAttempt {
            index,
            retry_count: 0,
            mode: ComposeMode::Batched,
            feedback: Vec::new(),
            retry_group: None,
            max_batch_size: None,
        })
        .collect();
    pack_attempts(
        scaffold,
        attempts,
        policy,
        policy.max_tasks_per_batch,
        estimator,
    )
}

fn pack_attempts<E>(
    scaffold: &ScaffoldDocument,
    attempts: Vec<TaskAttempt>,
    policy: BatchPolicy,
    max_qblocks: usize,
    estimator: &E,
) -> Vec<Vec<TaskAttempt>>
where
    E: TokenEstimator,
{
    if attempts.is_empty() {
        return Vec::new();
    }
    let max_qblocks = max_qblocks.max(1);
    let mut heavy = Vec::new();
    let mut light = Vec::new();

    for attempt in attempts {
        let cost = estimate_qblock_cost(&scaffold.tasks[attempt.index], estimator);
        if is_heavy_qblock(cost, policy) || exceeds_soft_budget(cost, policy) {
            heavy.push(vec![attempt]);
        } else {
            light.push((attempt, cost));
        }
    }

    light.sort_by(|(left_attempt, left_cost), (right_attempt, right_cost)| {
        cost_score(*right_cost, policy)
            .cmp(&cost_score(*left_cost, policy))
            .then_with(|| left_attempt.index.cmp(&right_attempt.index))
    });

    let mut bins = Vec::<(Vec<TaskAttempt>, BatchLoad)>::new();
    for (attempt, cost) in light {
        let mut pending = Some(attempt);
        for (batch, load) in &mut bins {
            if load.can_fit(cost, policy, max_qblocks) {
                batch.push(pending.take().expect("attempt is inserted once"));
                load.add(cost);
                break;
            }
        }
        if let Some(attempt) = pending {
            let mut load = BatchLoad::default();
            load.add(cost);
            bins.push((vec![attempt], load));
        }
    }

    let mut batches = bins
        .into_iter()
        .map(|(mut batch, _)| {
            batch.sort_by_key(|attempt| attempt.index);
            batch
        })
        .collect::<Vec<_>>();
    batches.extend(heavy);
    batches.sort_by_key(|batch| {
        batch
            .iter()
            .map(|attempt| attempt.index)
            .min()
            .unwrap_or(usize::MAX)
    });
    batches
}

fn estimate_qblock_cost<E>(task: &ScaffoldTask, estimator: &E) -> QBlockCost
where
    E: TokenEstimator,
{
    let output_base = estimator.estimate(&task.scaffold_question);
    QBlockCost {
        input_tokens: estimate_task_tokens(task, estimator),
        output_tokens: output_base.saturating_mul(6).saturating_add(4) / 5 + 24,
        blanks: task.blank_count,
    }
}

fn exceeds_soft_budget(cost: QBlockCost, policy: BatchPolicy) -> bool {
    cost.input_tokens > policy.max_estimated_input_tokens
        || cost.output_tokens > policy.max_estimated_output_tokens
        || cost.blanks > policy.max_blanks_per_batch
}

fn is_heavy_qblock(cost: QBlockCost, policy: BatchPolicy) -> bool {
    consumes_at_least(cost.input_tokens, policy.max_estimated_input_tokens, 3, 5)
        || consumes_at_least(cost.output_tokens, policy.max_estimated_output_tokens, 3, 5)
        || consumes_at_least(cost.blanks, policy.max_blanks_per_batch, 3, 5)
}

fn consumes_at_least(value: usize, limit: usize, numerator: usize, denominator: usize) -> bool {
    value.saturating_mul(denominator) >= limit.saturating_mul(numerator)
}

fn cost_score(cost: QBlockCost, policy: BatchPolicy) -> usize {
    normalized_cost(cost.input_tokens, policy.max_estimated_input_tokens)
        .max(normalized_cost(
            cost.output_tokens,
            policy.max_estimated_output_tokens,
        ))
        .max(normalized_cost(cost.blanks, policy.max_blanks_per_batch))
}

fn normalized_cost(value: usize, limit: usize) -> usize {
    value.saturating_mul(10_000) / limit.max(1)
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
}

#[cfg(test)]
mod quota_planner_tests {
    use super::*;

    fn scaffold(count: usize) -> ScaffoldDocument {
        ScaffoldDocument {
            tasks: (0..count)
                .map(|index| ScaffoldTask {
                    id: format!("q{index}"),
                    source_text: "短い本文".to_string(),
                    cloze_template: "短い＿＿＿".to_string(),
                    scaffold_question: "短い＿＿＿".to_string(),
                    blank_count: 1,
                    answers: vec!["本文".to_string()],
                })
                .collect(),
        }
    }

    #[test]
    fn quota_plan_expands_only_until_request_budget_fits() {
        let scaffold = scaffold(5);
        let policy = ComposeExecutionPolicy {
            batch_policy: BatchPolicy {
                max_tasks_per_batch: 1,
                max_estimated_input_tokens: 1_000,
                max_estimated_output_tokens: 1_000,
                max_blanks_per_batch: 8,
                max_retry_count: 2,
                max_concurrent_batches: 1,
            },
            max_content_retries: 2,
        };
        let quota = QuotaProfile {
            name: "test".into(),
            rpm: Some(5),
            tpm: Some(10_000),
            rpd: Some(3),
            reserve_requests: 1,
            adaptive_max_tasks_per_batch: Some(3),
            adaptive_max_input_tokens: Some(2_000),
            adaptive_max_output_tokens: Some(2_000),
            adaptive_max_blanks_per_batch: Some(16),
        };
        let plan = prepare_compose_plan_with_quota(&scaffold, policy, Some(&quota)).unwrap();
        assert_eq!(plan.batch_count(), 2);
        assert_eq!(plan.effective_policy.max_tasks_per_batch, 3);
    }

    #[test]
    fn quota_plan_fails_before_provider_when_hard_limit_cannot_fit() {
        let scaffold = scaffold(5);
        let policy = ComposeExecutionPolicy {
            batch_policy: BatchPolicy {
                max_tasks_per_batch: 1,
                max_estimated_input_tokens: 1_000,
                max_estimated_output_tokens: 1_000,
                max_blanks_per_batch: 8,
                max_retry_count: 2,
                max_concurrent_batches: 1,
            },
            max_content_retries: 2,
        };
        let quota = QuotaProfile {
            name: "test".into(),
            rpm: None,
            tpm: None,
            rpd: Some(3),
            reserve_requests: 1,
            adaptive_max_tasks_per_batch: Some(2),
            adaptive_max_input_tokens: Some(1_000),
            adaptive_max_output_tokens: Some(1_000),
            adaptive_max_blanks_per_batch: Some(8),
        };
        assert!(matches!(
            prepare_compose_plan_with_quota(&scaffold, policy, Some(&quota)),
            Err(ComposePlanError::Configuration { id }) if id == "quota-budget"
        ));
    }

    #[test]
    fn planner_repacks_noncontiguous_light_qblocks_and_isolates_heavy_qblock() {
        let task = |id: &str, source_len: usize| ScaffoldTask {
            id: id.to_string(),
            source_text: "a".repeat(source_len),
            cloze_template: "b".repeat(10),
            scaffold_question: "b".repeat(10),
            blank_count: 1,
            answers: vec!["z".to_string()],
        };
        let scaffold = ScaffoldDocument {
            tasks: vec![
                task("q0", 10),
                task("q1", 70),
                task("q2", 10),
                task("q3", 10),
            ],
        };
        let policy = BatchPolicy {
            max_tasks_per_batch: 4,
            max_estimated_input_tokens: 100,
            max_estimated_output_tokens: 200,
            max_blanks_per_batch: 10,
            max_retry_count: 2,
            max_concurrent_batches: 1,
        };

        let planned = plan_batches(&scaffold, policy, &CharHeuristicTokenEstimator)
            .into_iter()
            .map(|batch| {
                batch
                    .into_iter()
                    .map(|attempt| attempt.index)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        assert_eq!(planned, vec![vec![0, 2, 3], vec![1]]);
    }

    #[test]
    fn output_and_blank_budgets_can_make_qblocks_singletons() {
        let scaffold = ScaffoldDocument {
            tasks: vec![
                ScaffoldTask {
                    id: "light-a".into(),
                    source_text: "a".repeat(5),
                    cloze_template: "b".repeat(10),
                    scaffold_question: "b".repeat(10),
                    blank_count: 1,
                    answers: vec!["z".into()],
                },
                ScaffoldTask {
                    id: "output-heavy".into(),
                    source_text: "a".repeat(5),
                    cloze_template: "b".repeat(50),
                    scaffold_question: "b".repeat(50),
                    blank_count: 1,
                    answers: vec!["z".into()],
                },
                ScaffoldTask {
                    id: "blank-heavy".into(),
                    source_text: "a".repeat(5),
                    cloze_template: "b".repeat(10),
                    scaffold_question: "b".repeat(10),
                    blank_count: 6,
                    answers: vec!["z".into(); 6],
                },
                ScaffoldTask {
                    id: "light-b".into(),
                    source_text: "a".repeat(5),
                    cloze_template: "b".repeat(10),
                    scaffold_question: "b".repeat(10),
                    blank_count: 1,
                    answers: vec!["z".into()],
                },
            ],
        };
        let policy = BatchPolicy {
            max_tasks_per_batch: 4,
            max_estimated_input_tokens: 1_000,
            max_estimated_output_tokens: 100,
            max_blanks_per_batch: 10,
            max_retry_count: 2,
            max_concurrent_batches: 1,
        };

        let planned = plan_batches(&scaffold, policy, &CharHeuristicTokenEstimator)
            .into_iter()
            .map(|batch| {
                batch
                    .into_iter()
                    .map(|attempt| attempt.index)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        assert_eq!(planned, vec![vec![0, 3], vec![1], vec![2]]);
    }

    #[test]
    fn first_content_retry_is_rebatched_and_last_retry_is_single_task() {
        let scaffold = scaffold(3);
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
