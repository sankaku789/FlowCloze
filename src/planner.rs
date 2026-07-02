//! Adaptive Compose Planner.

use std::collections::HashMap;

use crate::compose::{merge_composed_questions, ComposedDocument, ComposedQuestion};
use crate::json::IntermediateDocument;
use crate::prompt::build_question_composer_prompt;
use crate::scaffold::{ScaffoldDocument, ScaffoldTask};
use crate::validation::{validate_generated_document, GeneratedDocument, ValidationError};

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
                if is_japanese_char(ch) {
                    1
                } else if ch.is_ascii_alphanumeric() {
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
    /// prompt構築に失敗した．
    Prompt(String),
    /// LLMクライアント呼び出しに失敗した．
    Llm(String),
    /// LLM応答をJSONとして解釈できなかった．
    Json(String),
    /// retry上限後もtaskの検証に失敗した．
    Validation { id: String, errors: Vec<String> },
}

impl std::fmt::Display for ComposePlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Prompt(message) => write!(f, "プロンプト生成に失敗しました: {message}"),
            Self::Llm(message) => write!(f, "{message}"),
            Self::Json(message) => write!(f, "生成結果JSONを読めません: {message}"),
            Self::Validation { id, errors } => {
                write!(
                    f,
                    "{id}: 生成結果の検証に失敗しました: {}",
                    errors.join("; ")
                )
            }
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
    /// 失敗時点のretry回数．
    retry_count: u32,
    /// 次回promptへ渡すための検証フィードバック．
    errors: Vec<String>,
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
                    retry_count: attempt.retry_count,
                    errors: vec![format!("生成結果JSONを読めません: {error}")],
                })
                .collect());
        }
    };

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
        let Some(question) = questions_by_id.remove(&qblock.id) else {
            failures.push(TaskFailure {
                index: attempt.index,
                retry_count: attempt.retry_count,
                errors: vec![format!("{}: LLM出力にidが含まれていません", qblock.id)],
            });
            continue;
        };

        let generated = merge_composed_questions(
            &IntermediateDocument {
                meta: intermediate.meta.clone(),
                qblocks: vec![qblock.clone()],
            },
            ComposedDocument {
                questions: vec![question.clone()],
            },
        );
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
                retry_count: attempt.retry_count,
                errors: build_retry_feedback(qblock.id.as_str(), &generated, &report.errors),
            });
        }
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
                id: failure.index.to_string(),
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

/// code fenceや前置き文が混ざった応答からJSON objectらしい範囲を抽出する．
fn extract_json_candidate(raw: &str) -> &str {
    let trimmed = raw.trim();
    let without_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|text| text.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);

    let Some(start) = without_fence.find('{') else {
        return without_fence;
    };
    let Some(end) = without_fence.rfind('}') else {
        return without_fence;
    };

    &without_fence[start..=end]
}

/// 検証エラーを次回promptへ渡す日本語フィードバックへ変換する．
fn build_retry_feedback(
    id: &str,
    generated: &GeneratedDocument,
    errors: &[ValidationError],
) -> Vec<String> {
    let question = generated
        .questions
        .first()
        .map(|question| question.question.as_str())
        .unwrap_or("");
    let mut feedback = errors.iter().map(ToString::to_string).collect::<Vec<_>>();
    feedback.push(format!("{id}: 前回question: {question}"));
    feedback
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
    use crate::json::{
        IntermediateDocument, IntermediateMeta, IntermediateQBlock, IntermediateTarget,
    };
    use crate::scaffold::build_scaffold_document;

    use super::*;

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
}
