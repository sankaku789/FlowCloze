//! 抽出したqblockを生成処理へ渡す中間JSONへ変換する．

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::models::{QBlock, Target};

pub const INTERMEDIATE_SCHEMA_VERSION: u32 = 3;
pub const DEFAULT_BLANK: &str = "＿＿＿";
pub const DEFAULT_BLOCK_SEPARATOR: &str = "\n\n";
pub const DEFAULT_PARAGRAPH_INDENT: &str = "　";

/// Markdown解析後に保存・生成入力として使う中間データ全体．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IntermediateDocument {
    pub schema_version: u32,
    pub meta: IntermediateMeta,
    pub tasks: Vec<IntermediateTask>,
}

/// 生成元Markdownに関するメタ情報．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IntermediateMeta {
    pub source: String,
    pub format: IntermediateFormat,
}

/// 中間JSON内で固定する出力フォーマット．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IntermediateFormat {
    pub blank: String,
    pub block_separator: String,
    pub paragraph_indent: String,
}

/// JSONへ保存する生成タスクのスナップショット．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IntermediateTask {
    pub id: String,
    #[serde(rename = "type")]
    pub task_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    pub source: IntermediateSource,
    pub blocks: Vec<IntermediateBlock>,
    pub cloze_template: String,
    pub targets: Vec<IntermediateTarget>,
    pub answers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// qblock本文の元表記と，target markup除去後の本文．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IntermediateSource {
    pub raw: String,
    pub plain: String,
}

/// cloze_templateを構成するテキストブロック．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IntermediateBlock {
    pub id: String,
    pub kind: String,
    pub starts_new_paragraph: bool,
    pub text: String,
    pub cloze_text: String,
    pub target_refs: Vec<usize>,
}

/// JSON上では仕様に合わせて `type` キーで表す出題対象．
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IntermediateTarget {
    pub index: usize,
    pub answer: String,
    #[serde(rename = "type")]
    pub target_type: String,
    pub block_id: String,
}

impl IntermediateDocument {
    pub fn from_qblocks(source: impl Into<String>, qblocks: &[QBlock]) -> Self {
        Self {
            schema_version: INTERMEDIATE_SCHEMA_VERSION,
            meta: IntermediateMeta {
                source: source.into(),
                format: IntermediateFormat {
                    blank: DEFAULT_BLANK.to_string(),
                    block_separator: DEFAULT_BLOCK_SEPARATOR.to_string(),
                    paragraph_indent: DEFAULT_PARAGRAPH_INDENT.to_string(),
                },
            },
            tasks: qblocks.iter().map(IntermediateTask::from).collect(),
        }
    }
}

impl From<&QBlock> for IntermediateTask {
    fn from(qblock: &QBlock) -> Self {
        let blocks = build_blocks(qblock);
        let block_id_by_target = blocks
            .iter()
            .flat_map(|block| {
                block
                    .target_refs
                    .iter()
                    .map(move |target_index| (*target_index, block.id.clone()))
            })
            .collect::<HashMap<_, _>>();
        let targets = qblock
            .targets
            .iter()
            .enumerate()
            .map(|(index, target)| {
                IntermediateTarget::from_target(index, target, &block_id_by_target)
            })
            .collect::<Vec<_>>();
        let answers = targets
            .iter()
            .map(|target| target.answer.clone())
            .collect::<Vec<_>>();
        let cloze_template = build_cloze_template(&qblock.source_text, &blocks);

        Self {
            id: qblock.id.clone(),
            task_type: "context-cloze".to_string(),
            section: qblock.section.clone(),
            source: IntermediateSource {
                raw: qblock.raw_source_text.clone(),
                plain: qblock.source_text.clone(),
            },
            blocks,
            cloze_template,
            targets,
            answers,
            warnings: qblock.warnings.clone(),
        }
    }
}

fn build_cloze_template(source_text: &str, blocks: &[IntermediateBlock]) -> String {
    let cloze_draft = blocks
        .iter()
        .map(|block| block.cloze_text.as_str())
        .collect::<Vec<_>>()
        .join(DEFAULT_BLOCK_SEPARATOR);

    format!("元の文章:\n{source_text}\n\n穴埋め下書き:\n{cloze_draft}")
}

impl IntermediateTarget {
    fn from_target(
        index: usize,
        target: &Target,
        block_id_by_target: &HashMap<usize, String>,
    ) -> Self {
        Self {
            index,
            answer: target.answer.clone(),
            target_type: target.target_type.clone(),
            block_id: block_id_by_target.get(&index).cloned().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockRange {
    start: usize,
    end: usize,
    starts_new_paragraph: bool,
    text: String,
}

fn build_blocks(qblock: &QBlock) -> Vec<IntermediateBlock> {
    split_plain_text_blocks(&qblock.source_text)
        .into_iter()
        .enumerate()
        .map(|(block_index, range)| {
            let target_refs = qblock
                .target_occurrences
                .iter()
                .filter(|occurrence| occurrence.start >= range.start && occurrence.end <= range.end)
                .map(|occurrence| occurrence.target_index)
                .collect::<Vec<_>>();
            let id = format!("{}-b{:03}", qblock.id, block_index + 1);
            let cloze_body = cloze_text_for_range(qblock, &range);

            IntermediateBlock {
                id,
                kind: "paragraph".to_string(),
                starts_new_paragraph: range.starts_new_paragraph,
                text: range.text,
                cloze_text: format!("{DEFAULT_PARAGRAPH_INDENT}{cloze_body}"),
                target_refs,
            }
        })
        .collect()
}

fn split_plain_text_blocks(text: &str) -> Vec<BlockRange> {
    let mut blocks = Vec::new();
    let mut current_start = None;
    let mut current_end = 0;
    let mut current_starts_new_paragraph = false;
    let mut offset = 0;

    for line in text.split('\n') {
        let line_start = offset;
        let line_end = line_start + line.len();
        offset = line_end + 1;

        if line.trim().is_empty() {
            push_current_block(
                text,
                &mut blocks,
                &mut current_start,
                current_end,
                current_starts_new_paragraph,
            );
            current_starts_new_paragraph = false;
            continue;
        }

        if current_start.is_none() {
            current_start = Some(line_start);
        }
        current_end = line_end;
    }

    push_current_block(
        text,
        &mut blocks,
        &mut current_start,
        current_end,
        current_starts_new_paragraph,
    );

    blocks
}

fn push_current_block(
    text: &str,
    blocks: &mut Vec<BlockRange>,
    current_start: &mut Option<usize>,
    current_end: usize,
    starts_new_paragraph: bool,
) {
    if let Some(start) = current_start.take() {
        blocks.push(BlockRange {
            start,
            end: current_end,
            starts_new_paragraph,
            text: text[start..current_end].to_string(),
        });
    }
}

fn cloze_text_for_range(qblock: &QBlock, range: &BlockRange) -> String {
    let mut cloze = String::new();
    let mut last = range.start;

    for occurrence in qblock
        .target_occurrences
        .iter()
        .filter(|occurrence| occurrence.start >= range.start && occurrence.end <= range.end)
    {
        cloze.push_str(&qblock.source_text[last..occurrence.start]);
        cloze.push_str(DEFAULT_BLANK);
        last = occurrence.end;
    }

    cloze.push_str(&qblock.source_text[last..range.end]);
    cloze
}

/// qblock抽出結果を整形済みの中間JSON文字列に変換する．
pub fn to_intermediate_json(source: &str, qblocks: &[QBlock]) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&IntermediateDocument::from_qblocks(source, qblocks))
}
