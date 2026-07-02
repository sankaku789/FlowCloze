//! LLMが生成した問題JSONを中間JSONと照合して検証する．

use std::collections::{HashMap, HashSet};

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
    #[serde(rename = "type")]
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

/// READMEで定義した生成JSONの検証ルールに対応するエラー．
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
    BlankAnswerCountMismatch {
        id: String,
        blank_count: usize,
        answer_count: usize,
    },
    AnswerNotInTargets {
        id: String,
        answer: String,
    },
    /// answer文字列が空欄化されずquestion本文に残っている．
    AnswerLeakage {
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
            Self::InvalidIntermediateJson(message) => {
                write!(f, "中間JSONを読めません: {message}")
            }
            Self::InvalidGeneratedJson(message) => {
                write!(f, "生成結果JSONを読めません: {message}")
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
            Self::AnswerLeakage { id, answer } => {
                write!(f, "{id}: answer '{answer}' がquestion本文に残っています")
            }
            Self::MissingTargetAnswer { id, answer } => {
                write!(f, "{id}: target '{answer}' がanswersに含まれていません")
            }
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
            // LLMがanswerを空欄とは別に本文へ戻していないかを保守的に検出する．
            if !answer.is_empty() && question.question.contains(answer) {
                errors.push(ValidationError::AnswerLeakage {
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

/// question本文に含まれる標準空欄表記の個数を数える．
fn count_blanks(question: &str) -> usize {
    question.matches("＿＿＿").count()
}

/// JSONでnullが来ても空配列などの既定値として扱う互換用deserializer．
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// 旧出力で混ざりうる入れ子answersを，単一の文字列配列へ平坦化する．
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
    /// 再帰的に入れ子配列を展開し，最終的なanswers配列へ追加する．
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
