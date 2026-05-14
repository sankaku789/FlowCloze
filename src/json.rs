//! 抽出した中間データをJSONへ変換する処理。

use serde::{Deserialize, Serialize};

use crate::models::{QBlock, Target};

/// Phase3で保存する中間データ全体。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IntermediateDocument {
    pub meta: IntermediateMeta,
    pub qblocks: Vec<IntermediateQBlock>,
}

/// 生成元Markdownに関するメタ情報。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IntermediateMeta {
    pub source: String,
}

/// JSON保存用のqblock。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IntermediateQBlock {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub source_text: String,
    pub targets: Vec<IntermediateTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// READMEの仕様に合わせて `type` というキーで出力する。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IntermediateTarget {
    pub answer: String,
    #[serde(rename = "type")]
    pub target_type: String,
}

impl IntermediateDocument {
    pub fn from_qblocks(source: impl Into<String>, qblocks: &[QBlock]) -> Self {
        Self {
            meta: IntermediateMeta {
                source: source.into(),
            },
            qblocks: qblocks.iter().map(IntermediateQBlock::from).collect(),
        }
    }
}

impl From<&QBlock> for IntermediateQBlock {
    fn from(qblock: &QBlock) -> Self {
        Self {
            id: qblock.id.clone(),
            mode: qblock.mode.clone(),
            title: qblock.title.clone(),
            source_text: qblock.source_text.clone(),
            targets: qblock
                .targets
                .iter()
                .map(IntermediateTarget::from)
                .collect(),
            warnings: qblock.warnings.clone(),
        }
    }
}

impl From<&Target> for IntermediateTarget {
    fn from(target: &Target) -> Self {
        Self {
            answer: target.answer.clone(),
            target_type: target.target_type.clone(),
        }
    }
}

/// qblock抽出結果をPhase3用JSON文字列に変換する。
pub fn to_intermediate_json(source: &str, qblocks: &[QBlock]) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&IntermediateDocument::from_qblocks(source, qblocks))
}
