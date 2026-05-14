use flowcloze::{build_generation_prompt, parse_markdown, IntermediateDocument};
use std::fs;

#[test]
fn プロンプトに中間データと制約を含める() {
    let markdown = fs::read_to_string("tests/fixtures/mvp-context.md").unwrap();
    let qblocks = parse_markdown(&markdown).unwrap();
    let document = IntermediateDocument::from_qblocks("tests/fixtures/mvp-context.md", &qblocks);

    let prompt = build_generation_prompt(&document).unwrap();

    assert!(prompt.contains("JSONのみを出力する"));
    assert!(prompt.contains("targetsにないanswerを追加しない"));
    assert!(prompt.contains(r#""id": "qblock-001""#));
    assert!(prompt.contains(r#""answer": "セマフォ""#));
    assert!(prompt.contains(r#""type": "term-name""#));
}
