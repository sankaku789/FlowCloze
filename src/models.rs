//! Markdown上のFlowCloze記法を表すドメインモデル．

/// READMEで定義している有効な出題タイプ．
pub const ALLOWED_TARGET_TYPES: &[&str] = &["term-name", "meaning", "process", "relation"];
pub const DEFAULT_TARGET_TYPE: &str = "term-name";

/// `[答え]` または `[答え]{タイプ}` で書かれた，人間指定の出題対象．
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub answer: String,
    pub target_type: String,
}

/// targetを除去した本文上でのanswer出現位置．
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetOccurrence {
    pub target_index: usize,
    pub start: usize,
    pub end: usize,
}

/// 1つのqblockと，そこから抽出した出題対象・警告．
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QBlock {
    pub id: String,
    pub section: Option<String>,
    pub raw_source_text: String,
    pub source_text: String,
    pub targets: Vec<Target>,
    pub target_occurrences: Vec<TargetOccurrence>,
    pub warnings: Vec<String>,
}
