use flowcloze::{build_generation_prompt, parse_markdown, IntermediateDocument};
use std::fs;

#[test]
fn プロンプトに中間データと制約を含める() {
    let markdown = fs::read_to_string("tests/fixtures/mvp-context.md").unwrap();
    let qblocks = parse_markdown(&markdown).unwrap();
    let document = IntermediateDocument::from_qblocks("tests/fixtures/mvp-context.md", &qblocks);

    let prompt = build_generation_prompt(&document).unwrap();

    assert!(prompt.contains("YAMLのみを出力する"));
    assert!(prompt.contains("targetsにないanswerを追加しない"));
    assert!(prompt.contains("id: sem-001"));
    assert!(prompt.contains("answer: セマフォ"));
    assert!(prompt.contains("type: term-name"));
}
