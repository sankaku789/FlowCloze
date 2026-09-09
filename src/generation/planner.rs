//! Adaptive Compose Planner.

use std::collections::HashMap;

use crate::compose::QuestionComposer;
use crate::executor::{ComposeExecutionError, ComposeMode, TaskAttempt};
use crate::json::IntermediateDocument;
use crate::observability::{EventSink, NoopEventSink, RunContext};
use crate::progress::{NoopProgressSink, ProgressSink};
use crate::quota::QuotaProfile;
use crate::scaffold::{ScaffoldDocument, ScaffoldTask};
use crate::validation::GeneratedDocument;

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

/// backendごとのbatch作成条件を表す設定．
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
    /// 将来の並列実行用の上限値．初期実装では逐次実行する．
    pub max_concurrent_batches: usize,
}

/// QuestionComposer実行のcontent retry設定．
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
            max_concurrent_batches: 1,
        }
    }
}

/// compose plannerで発生しうる失敗を呼び出し側へ伝えるエラー．
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposePlanError {
    /// policyまたはtask設定が不正だった．
    Configuration { id: String },
    /// provider/composer呼び出しに失敗した．
    Llm(String),
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

impl std::fmt::Display for ComposePlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration { id } => write!(f, "compose configuration error: task={id}"),
            Self::Llm(_) => write!(f, "compose error: llm"),
            Self::Validation { id, .. } => write!(f, "compose error: validation task={id}"),
            Self::Partial { .. } => write!(f, "compose error: generation incomplete"),
        }
    }
}

impl std::error::Error for ComposePlanError {}

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

/// policy 検証済みの初回 batch 計画。開始表示と実行で同じ計画を共有する。
#[derive(Debug, Clone)]
pub struct PreparedComposePlan {
    pub(crate) batches: Vec<Vec<TaskAttempt>>,
    pub(crate) effective_policy: BatchPolicy,
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
    crate::executor::execute_prepared_with_terminal_cause(
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
    crate::executor::execute_prepared_with_terminal_cause(
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

/// orchestration用のpolicyを検証する。
pub(crate) fn validate_port_policy(
    _scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
) -> Result<(), ComposePlanError> {
    if policy.batch_policy.max_tasks_per_batch == 0
        || policy.batch_policy.max_estimated_input_tokens == 0
        || policy.batch_policy.max_estimated_output_tokens == 0
        || policy.batch_policy.max_blanks_per_batch == 0
        || policy.batch_policy.max_concurrent_batches == 0
    {
        return Err(ComposePlanError::Configuration {
            id: "policy".to_string(),
        });
    }
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

pub(crate) fn plan_batches<E>(
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

pub(crate) fn pack_attempts<E>(
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
pub(crate) fn estimate_task_tokens<E>(task: &ScaffoldTask, estimator: &E) -> usize
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

/// 簡易token推定で日本語として重めに数える文字種を判定する．
fn is_japanese_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}'
    )
}
