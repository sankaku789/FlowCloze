//! LLMが生成した問題JSONを中間JSONと照合して検証する．

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::json::IntermediateDocument;

/// 生成結果JSONのルート構造．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GeneratedDocument {
    pub questions: Vec<GeneratedQuestion>,
}

/// LLMが1つのqblockから生成した文章補完問題．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GeneratedQuestion {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(rename = "type", default = "default_question_type")]
    pub question_type: String,
    pub targets: Option<Vec<GeneratedTarget>>,
    pub question: String,
    #[serde(default, deserialize_with = "flatten_answers")]
    pub answers: Vec<String>,
    pub source_text: Option<String>,
    pub explanation: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub tags: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub warnings: Vec<String>,
}

fn default_question_type() -> String {
    "context-cloze".to_string()
}

/// 生成結果に含める入力targetsの写し．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GeneratedTarget {
    pub answer: String,
    #[serde(rename = "type")]
    pub target_type: String,
}

/// 生成結果の検証結果．
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    pub errors: Vec<ValidationError>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// 生成結果として最低限成立しているかを確認するエラー．
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    InvalidIntermediateJson(String),
    InvalidGeneratedJson(String),
    EmptyQuestion {
        id: String,
    },
    DuplicateQuestionId {
        id: String,
    },
    UnknownQuestionId {
        id: String,
    },
    MissingQuestion {
        id: String,
    },
    BlankAnswerCountMismatch {
        id: String,
        blank_count: usize,
        answer_count: usize,
    },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIntermediateJson(message) => {
                write!(f, "中間JSONを読めません: {message}")
            }
            Self::InvalidGeneratedJson(message) => {
                write!(f, "生成結果JSONを読めません: {message}")
            }
            Self::EmptyQuestion { id } => write!(f, "{id}: questionが空です"),
            Self::DuplicateQuestionId { id } => write!(f, "{id}: idが重複しています"),
            Self::UnknownQuestionId { id } => write!(f, "{id}: 中間データに存在しないidです"),
            Self::MissingQuestion { id } => write!(f, "{id}: 対応する出力questionがありません"),
            Self::BlankAnswerCountMismatch {
                id,
                blank_count,
                answer_count,
            } => write!(
                f,
                "{id}: 空欄数({blank_count})とanswers数({answer_count})が一致しません"
            ),
        }
    }
}

/// 中間JSONと生成結果JSONを照合して検証する．
pub fn validate_generated_json(intermediate_json: &str, generated_json: &str) -> ValidationReport {
    let intermediate = match serde_json::from_str::<IntermediateDocument>(intermediate_json) {
        Ok(document) => document,
        Err(error) => {
            return ValidationReport {
                errors: vec![ValidationError::InvalidIntermediateJson(error.to_string())],
            };
        }
    };
    let generated = match serde_json::from_str::<GeneratedDocument>(generated_json) {
        Ok(document) => document,
        Err(error) => {
            return ValidationReport {
                errors: vec![ValidationError::InvalidGeneratedJson(error.to_string())],
            };
        }
    };

    validate_documents(&intermediate, &generated)
}

/// 中間JSONとパース済み生成結果を照合して検証する．
pub fn validate_generated_document(
    intermediate_json: &str,
    generated: &GeneratedDocument,
) -> ValidationReport {
    let intermediate = match serde_json::from_str::<IntermediateDocument>(intermediate_json) {
        Ok(document) => document,
        Err(error) => {
            return ValidationReport {
                errors: vec![ValidationError::InvalidIntermediateJson(error.to_string())],
            };
        }
    };

    validate_documents(&intermediate, generated)
}

fn validate_documents(
    intermediate: &IntermediateDocument,
    generated: &GeneratedDocument,
) -> ValidationReport {
    let known_ids = intermediate
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    let mut seen_ids = HashSet::new();
    let mut duplicate_ids = HashSet::new();
    let mut errors = Vec::new();

    for question in &generated.questions {
        if !seen_ids.insert(question.id.as_str()) {
            duplicate_ids.insert(question.id.as_str());
        }
    }

    for duplicate_id in duplicate_ids {
        errors.push(ValidationError::DuplicateQuestionId {
            id: duplicate_id.to_string(),
        });
    }

    for task in &intermediate.tasks {
        if !seen_ids.contains(task.id.as_str()) {
            errors.push(ValidationError::MissingQuestion {
                id: task.id.clone(),
            });
        }
    }

    for question in &generated.questions {
        if question.question.trim().is_empty() {
            errors.push(ValidationError::EmptyQuestion {
                id: question.id.clone(),
            });
        }

        let blank_count = count_blanks(&question.question);
        if blank_count != question.answers.len() {
            errors.push(ValidationError::BlankAnswerCountMismatch {
                id: question.id.clone(),
                blank_count,
                answer_count: question.answers.len(),
            });
        }

        if !known_ids.contains(question.id.as_str()) {
            errors.push(ValidationError::UnknownQuestionId {
                id: question.id.clone(),
            });
            continue;
        }
    }

    ValidationReport { errors }
}

fn count_blanks(question: &str) -> usize {
    question.matches("＿＿＿").count()
}

fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn flatten_answers<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Option::<Vec<AnswerValue>>::deserialize(deserializer)?.unwrap_or_default();
    let mut answers = Vec::new();
    for value in values {
        value.flatten_into(&mut answers);
    }
    Ok(answers)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
enum AnswerValue {
    Text(String),
    Many(Vec<AnswerValue>),
}

impl AnswerValue {
    fn flatten_into(self, answers: &mut Vec<String>) {
        match self {
            Self::Text(answer) => answers.push(answer),
            Self::Many(values) => {
                for value in values {
                    value.flatten_into(answers);
                }
            }
        }
    }
}
