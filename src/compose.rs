//! LLMが返すid/questionだけの結果を決定的な生成JSONへ合成する．

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::json::IntermediateDocument;
use crate::scaffold::BLANK;
use crate::validation::{GeneratedDocument, GeneratedQuestion, GeneratedTarget};

/// question本文を合成するプロバイダ非依存の境界．
pub trait QuestionComposer: Send + Sync {
    fn compose(&self, request: &ComposeBatchRequest) -> Result<ComposeBatchOutput, ComposeError>;
}

/// composerへ渡す，1回のまとめて処理する要求．
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComposeBatchRequest {
    pub schema_version: u32,
    pub batch_id: String,
    pub tasks: Vec<ComposeTask>,
    pub style: WritingStyle,
    pub prompt_version: String,
    /// providerへだけ渡す追加制約。観測・最終出力には含めない。
    pub extra_constraints: Vec<String>,
    /// 単独再試行時の、本文を含まない失敗分類。
    pub retry_feedback: Vec<String>,
}

/// Coreが確定した1問分の書き換え素材．
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComposeTask {
    pub id: String,
    pub source_text: String,
    pub scaffold_question: String,
    pub answers: Vec<String>,
    pub blank_token: String,
    pub blank_tokens: Vec<String>,
    pub blank_count: usize,
}

/// composerから返る最小の結果．固定フィールドは含めない．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ComposeBatchOutput {
    pub items: Vec<ComposedItem>,
    /// provider出力はitemsだけなので、追跡情報はadapter側で補う。
    #[serde(default)]
    pub metadata: ComposeMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ComposedItem {
    pub id: String,
    pub question: String,
}

/// 追跡用のprovider情報。最終GeneratedDocumentへは保存しない．
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct ComposeMetadata {
    pub adapter: String,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum WritingStyle {
    PlainJapanese,
}

/// composer境界での安全な失敗分類．本文やprovider応答は表示しない．
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeError {
    Configuration,
    Authentication,
    RateLimited,
    Timeout,
    Transport,
    Api { status: u16, retryable: bool },
    InvalidResponse,
    EmptyResponse,
}

impl std::fmt::Display for ComposeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let class = match self {
            Self::Configuration => "configuration",
            Self::Authentication => "authentication",
            Self::RateLimited => "rate-limited",
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::Api {
                retryable: true, ..
            } => "api-retryable",
            Self::Api {
                retryable: false, ..
            } => "api",
            Self::InvalidResponse => "invalid-response",
            Self::EmptyResponse => "empty-response",
        };
        write!(f, "question composer error: {class}")
    }
}

impl std::error::Error for ComposeError {}

/// APIを使わず、決定的な下書きをそのまま返すcomposer．
#[derive(Debug, Default)]
pub struct IdentityComposer;

impl QuestionComposer for IdentityComposer {
    fn compose(&self, request: &ComposeBatchRequest) -> Result<ComposeBatchOutput, ComposeError> {
        Ok(ComposeBatchOutput {
            items: request
                .tasks
                .iter()
                .map(|task| ComposedItem {
                    id: task.id.clone(),
                    question: task.scaffold_question.clone(),
                })
                .collect(),
            metadata: ComposeMetadata {
                adapter: "identity".to_string(),
                provider: "flowcloze".to_string(),
                model: "deterministic".to_string(),
            },
        })
    }
}

/// providerの生出力から共通のbatch出力を読む．
pub fn parse_compose_output(raw: &str) -> Result<ComposeBatchOutput, ComposeError> {
    let candidate = extract_json_candidate(raw);
    if candidate.trim().is_empty() {
        return Err(ComposeError::EmptyResponse);
    }
    serde_json::from_str(candidate).map_err(|_| ComposeError::InvalidResponse)
}

/// provider実装で共通利用するJSON候補抽出．
pub(crate) fn extract_json_candidate(raw: &str) -> &str {
    let trimmed = raw.trim();
    let without_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|text| text.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    match (without_fence.find('{'), without_fence.rfind('}')) {
        (Some(start), Some(end)) if start <= end => &without_fence[start..=end],
        _ => without_fence,
    }
}

/// scaffold taskからport用taskを構築する．
pub fn compose_task_from_scaffold(task: &crate::scaffold::ScaffoldTask) -> ComposeTask {
    let blank_tokens = generated_sentinel_tokens(&task.scaffold_question, &task.source_text);
    let is_sentinel_scaffold = blank_tokens.len() == task.blank_count;
    ComposeTask {
        id: task.id.clone(),
        source_text: task.source_text.clone(),
        scaffold_question: task.scaffold_question.clone(),
        answers: task.answers.clone(),
        blank_token: blank_tokens
            .first()
            .cloned()
            .unwrap_or_else(|| BLANK.to_string()),
        blank_tokens: if is_sentinel_scaffold {
            blank_tokens
        } else {
            vec![BLANK.to_string(); task.blank_count]
        },
        blank_count: task.blank_count,
    }
}

/// sourceに元からあったtokenを差し引き、scaffoldが今回導入したtokenだけを返す。
fn generated_sentinel_tokens(scaffold: &str, source: &str) -> Vec<String> {
    let mut baseline = sentinel_token_counts(source);
    sentinel_tokens(scaffold)
        .into_iter()
        .filter(|token| match baseline.get_mut(token.as_str()) {
            Some(count) if *count > 0 => {
                *count -= 1;
                false
            }
            _ => true,
        })
        .collect()
}

/// sentinel scaffoldのtokenだけを標準空欄へ戻し、LLMの対応崩れを検出する。
pub(crate) fn normalize_sentinel_question(
    question: &str,
    task: &ComposeTask,
) -> Result<String, &'static str> {
    let Some(namespace) = task
        .blank_tokens
        .first()
        .and_then(|token| sentinel_namespace(token))
    else {
        return Ok(question.to_string());
    };
    if question.contains(BLANK) || question.contains("___") {
        return Err("anonymous-blank");
    }
    let expected = &task.blank_tokens;
    let mut positions = Vec::with_capacity(expected.len());
    for token in expected {
        let mut matches = question.match_indices(token);
        let Some((position, _)) = matches.next() else {
            return Err("missing-sentinel");
        };
        if matches.next().is_some() {
            return Err("duplicate-sentinel");
        }
        positions.push(position);
    }
    if positions.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("sentinel-order");
    }
    let active_prefix = format!("⟦FC_{namespace}_");
    let baseline = sentinel_token_counts(&task.source_text);
    let mut returned = HashMap::new();
    let mut rest = question;
    while let Some(start) = rest.find("⟦FC_") {
        rest = &rest[start..];
        let Some(end) = rest.find('⟧') else {
            if rest.starts_with(&active_prefix) {
                return Err("malformed-sentinel");
            }
            break;
        };
        let token_end = end + '⟧'.len_utf8();
        let candidate = &rest[..token_end];
        if sentinel_namespace(candidate).is_some() {
            *returned.entry(candidate).or_insert(0usize) += 1;
        }
        if candidate.starts_with(&active_prefix) && !expected.iter().any(|token| token == candidate)
        {
            return Err("unknown-sentinel");
        }
        rest = &rest[token_end..];
    }
    // 元の本文にあった別namespaceのtokenは通常文字列として残せるが、providerが
    // 新規に注入した完全tokenはsentinelの混入として拒否する。
    for (token, count) in returned {
        if !token.starts_with(&active_prefix) && count > baseline.get(token).copied().unwrap_or(0) {
            return Err("foreign-sentinel");
        }
    }
    let mut marker_baseline = marker_counts(&fc_markers(&task.scaffold_question));
    for marker in fc_markers(question) {
        let count = marker_baseline.entry(marker).or_default();
        if *count == 0 {
            return Err("foreign-sentinel");
        }
        *count -= 1;
    }
    let mut normalized = question.to_string();
    for token in expected {
        normalized = normalized.replace(token, BLANK);
    }
    Ok(normalized)
}

fn fc_markers(text: &str) -> Vec<String> {
    let mut markers = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("⟦FC_") {
        rest = &rest[start..];
        let end = rest
            .find('⟧')
            .map(|end| end + '⟧'.len_utf8())
            .unwrap_or(rest.len());
        markers.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    markers
}

fn marker_counts(markers: &[String]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for marker in markers {
        *counts.entry(marker.clone()).or_insert(0) += 1;
    }
    counts
}

fn sentinel_token_counts(text: &str) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    let mut rest = text;
    while let Some(start) = rest.find("⟦FC_") {
        rest = &rest[start..];
        let Some(end) = rest.find('⟧') else { break };
        let token_end = end + '⟧'.len_utf8();
        let token = &rest[..token_end];
        if sentinel_namespace(token).is_some() {
            *counts.entry(token).or_insert(0) += 1;
        }
        rest = &rest[token_end..];
    }
    counts
}

fn sentinel_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("⟦FC_") {
        rest = &rest[start..];
        let Some(end) = rest.find('⟧') else {
            break;
        };
        let token_end = end + '⟧'.len_utf8();
        let token = &rest[..token_end];
        if sentinel_namespace(token).is_some() {
            tokens.push(token.to_string());
        }
        rest = &rest[token_end..];
    }
    tokens
}

fn sentinel_namespace(token: &str) -> Option<&str> {
    let inner = token.strip_prefix("⟦FC_")?.strip_suffix('⟧')?;
    let (namespace, index) = inner.split_once('_')?;
    (namespace.len() == 16
        && namespace.chars().all(|ch| ch.is_ascii_hexdigit())
        && index.len() == 6
        && index.chars().all(|ch| ch.is_ascii_digit()))
    .then_some(namespace)
}

/// strictな合成時に検出したID整合性の問題．
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeMergeIssue {
    DuplicateExpectedQuestionId { id: String },
    DuplicateQuestionId { id: String },
    UnknownQuestionId { id: String },
    MissingQuestionId { id: String },
}

/// strictな合成でdocumentを返せない理由．
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeMergeError {
    pub issues: Vec<ComposeMergeIssue>,
}

impl std::fmt::Display for ComposeMergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "問題文IDの合成に失敗しました: {:?}", self.issues)
    }
}

impl std::error::Error for ComposeMergeError {}

/// LLMが返すid/questionだけのルート構造．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ComposedDocument {
    /// taskごとに生成されたquestion本文．固定フィールドは含めない．
    pub questions: Vec<ComposedQuestion>,
}

/// LLMが1 taskに対して返す最小単位の生成結果．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ComposedQuestion {
    /// scaffold task id．中間表現との照合に使う．
    pub id: String,
    /// LLMが自然化したquestion本文．固定フィールドはここから推測しない．
    pub question: String,
}

/// LLM出力のquestionだけを採用し，固定フィールドを中間表現から再構築する．
pub fn merge_composed_questions(
    intermediate: &IntermediateDocument,
    composed: ComposedDocument,
) -> GeneratedDocument {
    let questions_by_id = composed
        .questions
        .into_iter()
        .map(|question| (question.id, question.question))
        .collect::<HashMap<_, _>>();

    // 中間表現のqblock順を最終JSONの出力順として維持する．
    GeneratedDocument {
        questions: intermediate
            .qblocks
            .iter()
            .filter_map(|qblock| {
                let question = questions_by_id.get(&qblock.id)?;
                Some(GeneratedQuestion {
                    id: qblock.id.clone(),
                    section: qblock.section.clone(),
                    question_type: "context-cloze".to_string(),
                    targets: Some(
                        qblock
                            .targets
                            .iter()
                            .map(|target| GeneratedTarget {
                                answer: target.answer.clone(),
                                target_type: target.target_type.clone(),
                            })
                            .collect(),
                    ),
                    question: normalize_question(question),
                    answers: qblock
                        .targets
                        .iter()
                        .map(|target| target.answer.clone())
                        .collect(),
                    source_text: Some(qblock.source_text.clone()),
                    explanation: None,
                    tags: Vec::new(),
                    warnings: qblock.warnings.clone(),
                })
            })
            .collect(),
    }
}

/// ID整合性を確認してから，中間表現の固定フィールドで再構築する．
pub fn try_merge_composed_questions(
    intermediate: &IntermediateDocument,
    composed: ComposedDocument,
) -> Result<GeneratedDocument, ComposeMergeError> {
    let issues = preflight_composed_questions(intermediate, &composed);
    if !issues.is_empty() {
        return Err(ComposeMergeError { issues });
    }
    Ok(merge_composed_questions(intermediate, composed))
}

/// HashMap化で重複を失う前にID整合性を走査する．
pub(crate) fn preflight_composed_questions(
    intermediate: &IntermediateDocument,
    composed: &ComposedDocument,
) -> Vec<ComposeMergeIssue> {
    let mut expected_ids = std::collections::HashSet::new();
    let mut duplicate_expected_ids = std::collections::HashSet::new();
    let mut issues = Vec::new();
    for qblock in &intermediate.qblocks {
        if !expected_ids.insert(qblock.id.as_str())
            && duplicate_expected_ids.insert(qblock.id.as_str())
        {
            issues.push(ComposeMergeIssue::DuplicateExpectedQuestionId {
                id: qblock.id.clone(),
            });
        }
    }
    // 期待IDが曖昧なら応答を検査しても確定的な対応付けはできない．
    if !issues.is_empty() {
        return issues;
    }

    let mut response_ids = std::collections::HashSet::new();
    let mut duplicate_response_ids = std::collections::HashSet::new();
    for question in &composed.questions {
        if !response_ids.insert(question.id.as_str())
            && duplicate_response_ids.insert(question.id.as_str())
        {
            issues.push(ComposeMergeIssue::DuplicateQuestionId {
                id: question.id.clone(),
            });
        }
    }
    let mut unknown_ids = std::collections::HashSet::new();
    for question in &composed.questions {
        if !expected_ids.contains(question.id.as_str()) && unknown_ids.insert(question.id.as_str())
        {
            issues.push(ComposeMergeIssue::UnknownQuestionId {
                id: question.id.clone(),
            });
        }
    }
    for qblock in &intermediate.qblocks {
        if !response_ids.contains(qblock.id.as_str()) {
            issues.push(ComposeMergeIssue::MissingQuestionId {
                id: qblock.id.clone(),
            });
        }
    }
    issues
}

/// LLM出力に混じりやすい前後の空白やMarkdown fenceを取り除く．
pub fn normalize_question(question: &str) -> String {
    question
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use crate::json::{
        IntermediateDocument, IntermediateMeta, IntermediateQBlock, IntermediateTarget,
    };

    use super::*;

    #[test]
    fn merges_only_question_from_llm_output() {
        let intermediate = IntermediateDocument {
            meta: IntermediateMeta {
                source: "input.md".to_string(),
            },
            qblocks: vec![IntermediateQBlock {
                id: "q1".to_string(),
                section: Some("Section".to_string()),
                source_text: "短期記憶はワーキングメモリである。".to_string(),
                targets: vec![IntermediateTarget {
                    answer: "ワーキングメモリ".to_string(),
                    target_type: "term".to_string(),
                }],
                warnings: vec!["warning".to_string()],
            }],
        };
        let composed = ComposedDocument {
            questions: vec![ComposedQuestion {
                id: "q1".to_string(),
                question: "短期記憶は＿＿＿である。".to_string(),
            }],
        };

        let generated = merge_composed_questions(&intermediate, composed);

        assert_eq!(generated.questions.len(), 1);
        assert_eq!(generated.questions[0].section.as_deref(), Some("Section"));
        assert_eq!(generated.questions[0].answers, vec!["ワーキングメモリ"]);
        assert_eq!(generated.questions[0].warnings, vec!["warning"]);
    }

    fn intermediate_with_ids(ids: &[&str]) -> IntermediateDocument {
        IntermediateDocument {
            meta: IntermediateMeta {
                source: "input.md".to_string(),
            },
            qblocks: ids
                .iter()
                .map(|id| IntermediateQBlock {
                    id: (*id).to_string(),
                    section: None,
                    source_text: "source".to_string(),
                    targets: Vec::new(),
                    warnings: Vec::new(),
                })
                .collect(),
        }
    }

    fn composed(ids: &[&str]) -> ComposedDocument {
        ComposedDocument {
            questions: ids
                .iter()
                .map(|id| ComposedQuestion {
                    id: (*id).to_string(),
                    question: format!(" {id} ```"),
                })
                .collect(),
        }
    }

    #[test]
    fn strict_merge_rejects_id_issues_in_deterministic_order() {
        let error = try_merge_composed_questions(
            &intermediate_with_ids(&["q1", "q2", "q3"]),
            composed(&["q2", "q2", "unknown", "unknown"]),
        )
        .unwrap_err();

        assert_eq!(
            error.issues,
            vec![
                ComposeMergeIssue::DuplicateQuestionId {
                    id: "q2".to_string()
                },
                ComposeMergeIssue::DuplicateQuestionId {
                    id: "unknown".to_string()
                },
                ComposeMergeIssue::UnknownQuestionId {
                    id: "unknown".to_string()
                },
                ComposeMergeIssue::MissingQuestionId {
                    id: "q1".to_string()
                },
                ComposeMergeIssue::MissingQuestionId {
                    id: "q3".to_string()
                },
            ]
        );
    }

    #[test]
    fn strict_merge_rebuilds_fixed_fields_on_success() {
        let generated = try_merge_composed_questions(
            &intermediate_with_ids(&["q1", "q2"]),
            composed(&["q1", "q2"]),
        )
        .expect("matching IDs should merge");

        assert_eq!(
            generated
                .questions
                .iter()
                .map(|question| question.id.as_str())
                .collect::<Vec<_>>(),
            vec!["q1", "q2"]
        );
    }

    #[test]
    fn strict_merge_returns_no_document_for_duplicate_expected_id() {
        let error = try_merge_composed_questions(
            &intermediate_with_ids(&["q1", "q1"]),
            composed(&["unknown", "unknown"]),
        )
        .unwrap_err();

        assert_eq!(
            error.issues,
            vec![ComposeMergeIssue::DuplicateExpectedQuestionId {
                id: "q1".to_string()
            }]
        );
    }

    #[test]
    fn legacy_merge_keeps_last_duplicate_and_ignores_unknown_and_missing() {
        let generated = merge_composed_questions(
            &intermediate_with_ids(&["q1", "q2"]),
            composed(&["q1", "unknown", "q1"]),
        );

        assert_eq!(generated.questions.len(), 1);
        assert_eq!(generated.questions[0].id, "q1");
        assert_eq!(generated.questions[0].question, "q1");
    }

    #[test]
    fn parses_fenced_output_with_surrounding_text() {
        let output = parse_compose_output(
            "result:\n```json\n{\"items\":[{\"id\":\"q1\",\"question\":\"＿＿＿\"}],\"metadata\":{\"adapter\":\"a\",\"provider\":\"p\",\"model\":\"m\"}}\n```\nend",
        )
        .expect("common parser should extract JSON");

        assert_eq!(output.items[0].id, "q1");
        assert_eq!(
            parse_compose_output(" \n "),
            Err(ComposeError::EmptyResponse)
        );
    }

    #[test]
    fn identity_composer_preserves_task_order_and_draft() {
        let request = ComposeBatchRequest {
            schema_version: 1,
            batch_id: "b1".to_string(),
            tasks: vec![
                ComposeTask {
                    id: "q2".to_string(),
                    source_text: "source".to_string(),
                    scaffold_question: "＿＿＿ second".to_string(),
                    answers: vec!["a".to_string()],
                    blank_token: BLANK.to_string(),
                    blank_tokens: vec![BLANK.to_string()],
                    blank_count: 1,
                },
                ComposeTask {
                    id: "q1".to_string(),
                    source_text: "source".to_string(),
                    scaffold_question: "＿＿＿ first".to_string(),
                    answers: vec!["b".to_string()],
                    blank_token: BLANK.to_string(),
                    blank_tokens: vec![BLANK.to_string()],
                    blank_count: 1,
                },
            ],
            style: WritingStyle::PlainJapanese,
            prompt_version: "compose-v1".to_string(),
            extra_constraints: Vec::new(),
            retry_feedback: Vec::new(),
        };

        let output = IdentityComposer.compose(&request).unwrap();

        assert_eq!(
            output
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["q2", "q1"]
        );
        assert_eq!(output.items[0].question, "＿＿＿ second");
    }
}
