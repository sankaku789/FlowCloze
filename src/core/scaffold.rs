//! LLMへ渡すための決定的なscaffoldを構築する．

use serde::Serialize;

use crate::json::IntermediateDocument;

/// すべての生成・検証で使う標準の空欄表記．
pub const BLANK: &str = "＿＿＿";

/// LLMへ渡すcompose対象taskの集合．
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScaffoldDocument {
    /// LLM呼び出し単位へ分割される前のtask一覧．
    pub tasks: Vec<ScaffoldTask>,
}

/// 1つのqblockに対応する，LLM用の下書き情報．
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScaffoldTask {
    /// 中間表現のqblock id．LLM出力との対応付けに使う．
    pub id: String,
    /// 元ノートから抽出したqblock本文．LLMが参照できる原文情報．
    pub source_text: String,
    /// answerを空欄へ置換した決定的なcloze template．
    pub cloze_template: String,
    /// LLMへ自然化対象として渡す下書き本文．初期実装ではcloze templateと同一．
    pub scaffold_question: String,
    /// task内に必要な空欄数．検証時にLLM出力の空欄数と照合する．
    pub blank_count: usize,
    /// target answerの順序付き一覧．LLMには本文へ戻さない語句として渡す．
    pub answers: Vec<String>,
}

/// 中間表現から，LLMに渡すためのscaffoldを決定的に構築する．
pub fn build_scaffold_document(intermediate: &IntermediateDocument) -> ScaffoldDocument {
    ScaffoldDocument {
        tasks: intermediate
            .qblocks
            .iter()
            .map(|qblock| {
                let answers = qblock
                    .targets
                    .iter()
                    .map(|target| target.answer.clone())
                    .collect::<Vec<_>>();
                let cloze_template = build_cloze_template(&qblock.source_text, &answers);

                ScaffoldTask {
                    id: qblock.id.clone(),
                    source_text: qblock.source_text.clone(),
                    scaffold_question: cloze_template.clone(),
                    cloze_template,
                    blank_count: answers.len(),
                    answers,
                }
            })
            .collect(),
    }
}

/// source_text内のanswerを出現順に1回ずつ空欄へ置換する．
fn build_cloze_template(source_text: &str, answers: &[String]) -> String {
    let mut template = source_text.to_string();
    for answer in answers {
        if answer.is_empty() {
            continue;
        }
        template = template.replacen(answer, BLANK, 1);
    }
    template
}

#[cfg(test)]
mod tests {
    use crate::json::{
        IntermediateDocument, IntermediateMeta, IntermediateQBlock, IntermediateTarget,
    };

    use super::*;

    #[test]
    fn builds_scaffold_by_replacing_targets_in_order() {
        let intermediate = IntermediateDocument {
            meta: IntermediateMeta {
                source: "input.md".to_string(),
            },
            qblocks: vec![IntermediateQBlock {
                id: "q1".to_string(),
                section: Some("Memory".to_string()),
                source_text: "短期記憶はワーキングメモリであり，容量は7±2である。".to_string(),
                targets: vec![
                    IntermediateTarget {
                        answer: "ワーキングメモリ".to_string(),
                        target_type: "term".to_string(),
                    },
                    IntermediateTarget {
                        answer: "7±2".to_string(),
                        target_type: "number".to_string(),
                    },
                ],
                warnings: Vec::new(),
            }],
        };

        let scaffold = build_scaffold_document(&intermediate);

        assert_eq!(scaffold.tasks.len(), 1);
        assert_eq!(
            scaffold.tasks[0].scaffold_question,
            "短期記憶は＿＿＿であり，容量は＿＿＿である。"
        );
        assert_eq!(scaffold.tasks[0].blank_count, 2);
    }
}
