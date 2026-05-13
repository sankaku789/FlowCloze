//! LLM生成後YAMLの検証処理。

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::yaml::IntermediateDocument;

/// 生成結果YAML全体。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GeneratedDocument {
    pub questions: Vec<GeneratedQuestion>,
}

/// LLMが生成した文章補完問題。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GeneratedQuestion {
    pub id: String,
    #[serde(rename = "type")]
    pub question_type: String,
    pub title: Option<String>,
    pub targets: Option<Vec<GeneratedTarget>>,
    pub question: String,
    pub answers: Vec<String>,
    pub source_text: Option<String>,
    pub explanation: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// 生成結果内に残されたtargets。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GeneratedTarget {
    pub answer: String,
    #[serde(rename = "type")]
    pub target_type: String,
}

/// Phase5の検証結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    pub errors: Vec<ValidationError>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// READMEの検証ルールに対応するエラー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    InvalidIntermediateYaml(String),
    InvalidGeneratedYaml(String),
    EmptyQuestion {
        id: String,
    },
    DuplicateQuestionId {
        id: String,
    },
    UnknownQuestionId {
        id: String,
    },
    BlankAnswerCountMismatch {
        id: String,
        blank_count: usize,
        answer_count: usize,
    },
    AnswerNotInTargets {
        id: String,
        answer: String,
    },
    MissingTargetAnswer {
        id: String,
        answer: String,
    },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIntermediateYaml(message) => {
                write!(f, "中間YAMLを読めません: {message}")
            }
            Self::InvalidGeneratedYaml(message) => {
                write!(f, "生成結果YAMLを読めません: {message}")
            }
            Self::EmptyQuestion { id } => write!(f, "{id}: questionが空です"),
            Self::DuplicateQuestionId { id } => write!(f, "{id}: idが重複しています"),
            Self::UnknownQuestionId { id } => write!(f, "{id}: 中間データに存在しないidです"),
            Self::BlankAnswerCountMismatch {
                id,
                blank_count,
                answer_count,
            } => write!(
                f,
                "{id}: 空欄数({blank_count})とanswers数({answer_count})が一致しません"
            ),
            Self::AnswerNotInTargets { id, answer } => {
                write!(f, "{id}: answer '{answer}' はtargetsに含まれていません")
            }
            Self::MissingTargetAnswer { id, answer } => {
                write!(f, "{id}: target '{answer}' がanswersに含まれていません")
            }
        }
    }
}

/// 中間YAMLと生成結果YAMLを照合して検証する。
pub fn validate_generated_yaml(intermediate_yaml: &str, generated_yaml: &str) -> ValidationReport {
    let intermediate = match serde_yaml::from_str::<IntermediateDocument>(intermediate_yaml) {
        Ok(document) => document,
        Err(error) => {
            return ValidationReport {
                errors: vec![ValidationError::InvalidIntermediateYaml(error.to_string())],
            };
        }
    };
    let generated = match serde_yaml::from_str::<GeneratedDocument>(generated_yaml) {
        Ok(document) => document,
        Err(error) => {
            return ValidationReport {
                errors: vec![ValidationError::InvalidGeneratedYaml(error.to_string())],
            };
        }
    };

    validate_documents(&intermediate, &generated)
}

fn validate_documents(
    intermediate: &IntermediateDocument,
    generated: &GeneratedDocument,
) -> ValidationReport {
    let target_answers_by_id = intermediate
        .qblocks
        .iter()
        .map(|qblock| {
            (
                qblock.id.as_str(),
                qblock
                    .targets
                    .iter()
                    .map(|target| target.answer.as_str())
                    .collect::<HashSet<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();
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

        let Some(target_answers) = target_answers_by_id.get(question.id.as_str()) else {
            errors.push(ValidationError::UnknownQuestionId {
                id: question.id.clone(),
            });
            continue;
        };

        for answer in &question.answers {
            if !target_answers.contains(answer.as_str()) {
                errors.push(ValidationError::AnswerNotInTargets {
                    id: question.id.clone(),
                    answer: answer.clone(),
                });
            }
        }

        let answer_set = question
            .answers
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        for target_answer in target_answers {
            if !answer_set.contains(target_answer) {
                errors.push(ValidationError::MissingTargetAnswer {
                    id: question.id.clone(),
                    answer: (*target_answer).to_string(),
                });
            }
        }
    }

    ValidationReport { errors }
}

fn count_blanks(question: &str) -> usize {
    question.matches("＿＿＿").count()
}
