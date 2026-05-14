//! Markdown補完問題記法のドメインモデル。

/// READMEで定義している出題タイプ。
pub const ALLOWED_TARGET_TYPES: &[&str] = &["term-name", "meaning", "process", "relation"];

/// `[答え]{タイプ}` で書かれた，人間が指定した出題対象。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub answer: String,
    pub target_type: String,
}

/// `qblock` と，そこから抽出した出題対象。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QBlock {
    pub id: String,
    pub section: Option<String>,
    pub source_text: String,
    pub targets: Vec<Target>,
    pub warnings: Vec<String>,
}
