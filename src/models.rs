//! Markdown補完問題記法のドメインモデル。

/// READMEで定義している初期段階の出題タイプ。
pub const ALLOWED_TARGET_TYPES: &[&str] = &[
    "term-name",
    "meaning",
    "process",
    "state",
    "reason",
    "merit",
    "demerit",
    "compare",
];

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
    pub title: Option<String>,
    pub source_text: String,
    pub targets: Vec<Target>,
    pub mode: Option<String>,
    pub attrs: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

impl QBlock {
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find_map(|(attr_key, value)| (attr_key == key).then_some(value.as_str()))
    }
}
