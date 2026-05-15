//! Markdown上のFlowCloze記法を表すドメインモデル。

/// READMEで定義している有効な出題タイプ。
pub const ALLOWED_TARGET_TYPES: &[&str] = &["term-name", "meaning", "process", "relation"];

/// `[答え]{タイプ}` で書かれた，人間指定の出題対象。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub answer: String,
    pub target_type: String,
}

/// 1つのqblockと，そこから抽出した出題対象・警告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QBlock {
    pub id: String,
    pub section: Option<String>,
    pub source_text: String,
    pub targets: Vec<Target>,
    pub warnings: Vec<String>,
}
