//! LLMが返すid/questionだけの結果を決定的な生成JSONへ合成する．

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::json::IntermediateDocument;
use crate::rate_limit::RateLimitKind;
use crate::scaffold::BLANK;
use crate::validation::{GeneratedDocument, GeneratedQuestion, GeneratedTarget};

/// question本文を合成するプロバイダ非依存の境界．
pub trait QuestionComposer: Send + Sync {
    fn compose(&self, request: &ComposeBatchRequest) -> Result<ComposeBatchOutput, ComposeError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComposeBatchRequest {
    pub schema_version: u32,
    pub batch_id: String,
    pub tasks: Vec<ComposeTask>,
    pub style: WritingStyle,
    pub prompt_version: String,
    pub extra_constraints: Vec<String>,
    pub retry_feedback: Vec<String>,
}

/// Core内部では元情報を保持するが、providerへはprompt builderがid/questionだけを公開する。
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ComposeBatchOutput {
    pub items: Vec<ComposedItem>,
    #[serde(default)]
    pub metadata: ComposeMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ComposedItem {
    pub id: String,
    pub question: String,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeError {
    Configuration,
    Authentication,
    RateLimited { kind: RateLimitKind },
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
            Self::RateLimited { .. } => "rate-limited",
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::Api { retryable: true, .. } => "api-retryable",
            Self::Api { retryable: false, .. } => "api",
            Self::InvalidResponse => "invalid-response",
            Self::EmptyResponse => "empty-response",
        };
        write!(f, "question composer error: {class}")
    }
}

impl std::error::Error for ComposeError {}

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

pub fn parse_compose_output(raw: &str) -> Result<ComposeBatchOutput, ComposeError> {
    let candidate = extract_json_candidate(raw);
    if candidate.trim().is_empty() {
        return Err(ComposeError::EmptyResponse);
    }
    serde_json::from_str(candidate).map_err(|_| ComposeError::InvalidResponse)
}

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

fn provider_blank(index: usize) -> String {
    format!("<BLANK_{index}>")
}

/// provider-safe placeholder または旧sentinelを標準空欄へ戻す。
/// hard validationは空欄の個数・重複・相対順だけに限定する。
pub(crate) fn normalize_sentinel_question(
    question: &str,
    task: &ComposeTask,
) -> Result<String, &'static str> {
    let expected_count = task.blank_count;

    // 新しいprovider境界。<BLANK_n> が1つでもあれば、この形式だけを検証する。
    if question.contains("<BLANK_") {
        let mut positions = Vec::with_capacity(expected_count);
        for index in 0..expected_count {
            let marker = provider_blank(index);
            let mut matches = question.match_indices(&marker);
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

        // 期待していない番号や壊れたplaceholderを拒否する。
        let mut rest = question;
        let mut seen = 0usize;
        while let Some(start) = rest.find("<BLANK_") {
            rest = &rest[start..];
            let Some(end) = rest.find('>') else {
                return Err("malformed-sentinel");
            };
            let candidate = &rest[..=end];
            let Some(index_text) = candidate
                .strip_prefix("<BLANK_")
                .and_then(|value| value.strip_suffix('>'))
            else {
                return Err("malformed-sentinel");
            };
            let Ok(index) = index_text.parse::<usize>() else {
                return Err("malformed-sentinel");
            };
            if index >= expected_count {
                return Err("unknown-sentinel");
            }
            seen += 1;
            rest = &rest[end + 1..];
        }
        if seen != expected_count {
            return Err("duplicate-sentinel");
        }

        let mut normalized = question.to_string();
        for index in 0..expected_count {
            normalized = normalized.replace(&provider_blank(index), BLANK);
        }
        return Ok(normalized);
    }

    // 非sentinel scaffoldは従来どおりそのまま扱う。
    let expected = &task.blank_tokens;
    let Some(_namespace) = expected.first().and_then(|token| sentinel_namespace(token)) else {
        return Ok(question.to_string());
    };

    if question.contains(BLANK) || question.contains("___") {
        return Err("anonymous-blank");
    }

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

    let mut normalized = question.to_string();
    for token in expected {
        normalized = normalized.replace(token, BLANK);
    }
    Ok(normalized)
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
        let Some(end) = rest.find('⟧') else { break };
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeMergeIssue {
    DuplicateExpectedQuestionId { id: String },
    DuplicateQuestionId { id: String },
    UnknownQuestionId { id: String },
    MissingQuestionId { id: String },
}

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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ComposedDocument {
    pub questions: Vec<ComposedQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ComposedQuestion {
    pub id: String,
    pub question: String,
}

pub fn merge_composed_questions(
    intermediate: &IntermediateDocument,
    composed: ComposedDocument,
) -> GeneratedDocument {
    let questions_by_id = composed
        .questions
        .into_iter()
        .map(|question| (question.id, question.question))
        .collect::<HashMap<_, _>>();

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

pub(crate) fn preflight_composed_questions(
    intermediate: &IntermediateDocument,
    composed: &ComposedDocument,
) -> Vec<ComposeMergeIssue> {
    let mut expected_ids = HashSet::new();
    let mut duplicate_expected_ids = HashSet::new();
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
    if !issues.is_empty() {
        return issues;
    }

    let mut response_ids = HashSet::new();
    let mut duplicate_response_ids = HashSet::new();
    for question in &composed.questions {
        if !response_ids.insert(question.id.as_str())
            && duplicate_response_ids.insert(question.id.as_str())
        {
            issues.push(ComposeMergeIssue::DuplicateQuestionId {
                id: question.id.clone(),
            });
        }
    }

    let mut unknown_ids = HashSet::new();
    for question in &composed.questions {
        if !expected_ids.contains(question.id.as_str()) && unknown_ids.insert(question.id.as_str()) {
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

    fn sentinel_task(blank_count: usize) -> ComposeTask {
        let blank_tokens = (0..blank_count)
            .map(|index| format!("⟦FC_0123456789abcdef_{index:06}⟧"))
            .collect::<Vec<_>>();
        ComposeTask {
            id: "q1".into(),
            source_text: "source".into(),
            scaffold_question: blank_tokens.join(" / "),
            answers: Vec::new(),
            blank_token: blank_tokens.first().cloned().unwrap_or_default(),
            blank_tokens,
            blank_count,
        }
    }

    #[test]
    fn normalizes_provider_safe_placeholders() {
        let task = sentinel_task(2);
        let normalized = normalize_sentinel_question("A<BLANK_0>B<BLANK_1>C", &task).unwrap();
        assert_eq!(normalized, format!("A{BLANK}B{BLANK}C"));
    }

    #[test]
    fn provider_safe_placeholders_reject_missing_duplicate_and_order() {
        let task = sentinel_task(2);
        assert_eq!(
            normalize_sentinel_question("A<BLANK_0>B", &task),
            Err("missing-sentinel")
        );
        assert_eq!(
            normalize_sentinel_question("<BLANK_0><BLANK_0><BLANK_1>", &task),
            Err("duplicate-sentinel")
        );
        assert_eq!(
            normalize_sentinel_question("<BLANK_1><BLANK_0>", &task),
            Err("sentinel-order")
        );
    }

    #[test]
    fn provider_safe_placeholders_reject_unknown_index() {
        let task = sentinel_task(1);
        assert_eq!(
            normalize_sentinel_question("<BLANK_0><BLANK_9>", &task),
            Err("unknown-sentinel")
        );
    }

    #[test]
    fn legacy_sentinel_path_still_normalizes() {
        let task = sentinel_task(1);
        let question = format!("A{}B", task.blank_tokens[0]);
        assert_eq!(normalize_sentinel_question(&question, &task).unwrap(), format!("A{BLANK}B"));
    }

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
                ComposeMergeIssue::DuplicateQuestionId { id: "q2".into() },
                ComposeMergeIssue::DuplicateQuestionId { id: "unknown".into() },
                ComposeMergeIssue::UnknownQuestionId { id: "unknown".into() },
                ComposeMergeIssue::MissingQuestionId { id: "q1".into() },
                ComposeMergeIssue::MissingQuestionId { id: "q3".into() },
            ]
        );
    }

    #[test]
    fn parses_fenced_output_with_surrounding_text() {
        let output = parse_compose_output(
            "result:\n```json\n{\"items\":[{\"id\":\"q1\",\"question\":\"＿＿＿\"}],\"metadata\":{\"adapter\":\"a\",\"provider\":\"p\",\"model\":\"m\"}}\n```\nend",
        )
        .expect("common parser should extract JSON");
        assert_eq!(output.items[0].id, "q1");
        assert_eq!(parse_compose_output(" \n "), Err(ComposeError::EmptyResponse));
    }

    #[test]
    fn identity_composer_preserves_task_order_and_draft() {
        let request = ComposeBatchRequest {
            schema_version: 1,
            batch_id: "b1".to_string(),
            tasks: vec![ComposeTask {
                id: "q1".to_string(),
                source_text: "source".to_string(),
                scaffold_question: "＿＿＿ first".to_string(),
                answers: vec!["a".to_string()],
                blank_token: BLANK.to_string(),
                blank_tokens: vec![BLANK.to_string()],
                blank_count: 1,
            }],
            style: WritingStyle::PlainJapanese,
            prompt_version: "compose-v1".to_string(),
            extra_constraints: Vec::new(),
            retry_feedback: Vec::new(),
        };
        let output = IdentityComposer.compose(&request).unwrap();
        assert_eq!(output.items[0].question, "＿＿＿ first");
    }
}
