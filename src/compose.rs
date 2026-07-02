//! LLMが返すid/questionだけの結果を決定的な生成JSONへ合成する．

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::json::IntermediateDocument;
use crate::validation::{GeneratedDocument, GeneratedQuestion, GeneratedTarget};

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
}
