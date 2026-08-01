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
    MissingQuestion {
        id: String,
    },
    QuestionOrderMismatch {
        expected: Vec<String>,
        actual: Vec<String>,
    },
    FixedFieldMismatch {
        id: String,
        field: FixedField,
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

/// 中間表現から再構築されるべき生成JSONの固定フィールド．
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedField {
    Section,
    QuestionType,
    Targets,
    Answers,
    SourceText,
}

impl FixedField {
    fn json_key(self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::QuestionType => "type",
            Self::Targets => "targets",
            Self::Answers => "answers",
            Self::SourceText => "source_text",
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIntermediateJson(_) => write!(f, "中間JSONを読めません"),
            Self::InvalidGeneratedJson(_) => write!(f, "生成結果JSONを読めません"),
            Self::EmptyQuestion { id } => write!(f, "{id}: questionが空です"),
            Self::DuplicateQuestionId { id } => write!(f, "{id}: idが重複しています"),
            Self::UnknownQuestionId { id } => write!(f, "{id}: 中間データに存在しないidです"),
            Self::MissingQuestion { id } => write!(f, "{id}: 生成結果にidがありません"),
            Self::QuestionOrderMismatch { expected, actual } => {
                write!(
                    f,
                    "questionの順序が一致しません: expected={expected:?}, actual={actual:?}"
                )
            }
            Self::FixedFieldMismatch { id, field } => {
                write!(f, "{id}: {}が中間データと一致しません", field.json_key())
            }
            Self::BlankAnswerCountMismatch {
                id,
                blank_count,
                answer_count,
            } => write!(
                f,
                "{id}: 空欄数({blank_count})とanswers数({answer_count})が一致しません"
            ),
            Self::AnswerNotInTargets { id, .. } => {
                write!(f, "{id}: answerがtargetsに含まれていません")
            }
            Self::AnswerLeakage { id, .. } => {
                write!(f, "{id}: answerがquestion本文に残っています")
            }
            Self::MissingTargetAnswer { id, .. } => {
                write!(f, "{id}: targetがanswersに含まれていません")
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

    validate_generated_documents(&intermediate, &generated)
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

    validate_generated_documents(&intermediate, generated)
}

/// JSON境界の外で使う標準検証。JSON APIはこのtyped検証への互換wrapperである。
pub(crate) fn validate_generated_documents(
    intermediate: &IntermediateDocument,
    generated: &GeneratedDocument,
) -> ValidationReport {
    let expected_ids = intermediate
        .qblocks
        .iter()
        .map(|qblock| qblock.id.as_str())
        .collect::<HashSet<_>>();
    let mut seen_ids = HashSet::new();
    let mut duplicate_ids = HashSet::new();
    let mut generated_ids = HashSet::new();
    let mut errors = Vec::new();

    for question in &generated.questions {
        if !seen_ids.insert(question.id.as_str()) && duplicate_ids.insert(question.id.as_str()) {
            errors.push(ValidationError::DuplicateQuestionId {
                id: question.id.clone(),
            });
        }
        generated_ids.insert(question.id.as_str());
    }
    for question in &generated.questions {
        if !expected_ids.contains(question.id.as_str()) {
            errors.push(ValidationError::UnknownQuestionId {
                id: question.id.clone(),
            });
        }
    }
    for qblock in &intermediate.qblocks {
        if !generated_ids.contains(qblock.id.as_str()) {
            errors.push(ValidationError::MissingQuestion {
                id: qblock.id.clone(),
            });
        }
    }
    if duplicate_ids.is_empty()
        && generated.questions.len() == intermediate.qblocks.len()
        && generated_ids.len() == expected_ids.len()
        && generated_ids == expected_ids
    {
        let expected = intermediate
            .qblocks
            .iter()
            .map(|q| q.id.clone())
            .collect::<Vec<_>>();
        let actual = generated
            .questions
            .iter()
            .map(|q| q.id.clone())
            .collect::<Vec<_>>();
        if actual != expected {
            errors.push(ValidationError::QuestionOrderMismatch { expected, actual });
        }
    }

    let qblocks_by_id = intermediate
        .qblocks
        .iter()
        .map(|qblock| (qblock.id.as_str(), qblock))
        .collect::<HashMap<_, _>>();
    let mut validated_known_ids = HashSet::new();
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

        let Some(qblock) = qblocks_by_id.get(question.id.as_str()) else {
            continue;
        };

        // 重複後続も本文とanswerの検証は続け、固定フィールド照合だけを省く．
        let check_fixed_fields = validated_known_ids.insert(question.id.as_str());
        let target_answers = qblock
            .targets
            .iter()
            .map(|target| target.answer.as_str())
            .collect::<HashSet<_>>();
        if check_fixed_fields && question.question_type != "context-cloze" {
            errors.push(ValidationError::FixedFieldMismatch {
                id: question.id.clone(),
                field: FixedField::QuestionType,
            });
        }
        // 旧生成器は未設定sectionを空文字列で出していたため、Noneと""は互換扱いにする。
        if check_fixed_fields
            && question.section.is_some()
            && !(qblock.section.is_none() && question.section.as_deref() == Some(""))
            && question.section != qblock.section
        {
            errors.push(ValidationError::FixedFieldMismatch {
                id: question.id.clone(),
                field: FixedField::Section,
            });
        }
        if check_fixed_fields
            && question.targets.is_some()
            && question.targets.as_ref()
                != Some(
                    &qblock
                        .targets
                        .iter()
                        .map(|target| GeneratedTarget {
                            answer: target.answer.clone(),
                            target_type: target.target_type.clone(),
                        })
                        .collect(),
                )
        {
            errors.push(ValidationError::FixedFieldMismatch {
                id: question.id.clone(),
                field: FixedField::Targets,
            });
        }
        let expected_answers = qblock
            .targets
            .iter()
            .map(|target| target.answer.clone())
            .collect::<Vec<_>>();
        if check_fixed_fields && question.answers != expected_answers {
            errors.push(ValidationError::FixedFieldMismatch {
                id: question.id.clone(),
                field: FixedField::Answers,
            });
        }
        if check_fixed_fields
            && question.source_text.is_some()
            && question.source_text.as_deref() != Some(&qblock.source_text)
        {
            errors.push(ValidationError::FixedFieldMismatch {
                id: question.id.clone(),
                field: FixedField::SourceText,
            });
        }

        for answer in &question.answers {
            if !target_answers.contains(answer.as_str()) {
                errors.push(ValidationError::AnswerNotInTargets {
                    id: question.id.clone(),
                    answer: answer.clone(),
                });
            }
            // target外に元からある同じ語句は漏洩ではない。target span分を差し引いた
            // sourceの出現数を基準に、providerが増やした完全一致だけを検出する。
            let target_count = qblock
                .targets
                .iter()
                .filter(|target| target.answer == *answer)
                .count();
            let baseline =
                count_occurrences(&qblock.source_text, answer).saturating_sub(target_count);
            if !answer.is_empty() && count_occurrences(&question.question, answer) > baseline {
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
        for target in &qblock.targets {
            if !answer_set.contains(target.answer.as_str()) {
                errors.push(ValidationError::MissingTargetAnswer {
                    id: question.id.clone(),
                    answer: target.answer.clone(),
                });
            }
        }
    }

    ValidationReport { errors }
}

/// located scaffoldが得られる標準生成経路用の検証。
/// JSON互換APIはspanを持たないため従来のbest-effort基準を維持する。
pub(crate) fn validate_generated_documents_with_leakage_baselines(
    intermediate: &IntermediateDocument,
    generated: &GeneratedDocument,
    leakage_baselines: &HashMap<String, Vec<usize>>,
) -> ValidationReport {
    let mut report = validate_generated_documents(intermediate, generated);
    report
        .errors
        .retain(|error| !matches!(error, ValidationError::AnswerLeakage { .. }));
    for question in &generated.questions {
        let Some(baselines) = leakage_baselines.get(&question.id) else {
            continue;
        };
        for (index, answer) in question.answers.iter().enumerate() {
            let baseline = baselines.get(index).copied().unwrap_or(0);
            if !answer.is_empty() && count_occurrences(&question.question, answer) > baseline {
                report.errors.push(ValidationError::AnswerLeakage {
                    id: question.id.clone(),
                    answer: answer.clone(),
                });
            }
        }
    }
    report
}

/// question本文に含まれる標準空欄表記の個数を数える．
fn count_blanks(question: &str) -> usize {
    question.matches("＿＿＿").count()
}

fn count_occurrences(text: &str, needle: &str) -> usize {
    text.match_indices(needle).count()
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
