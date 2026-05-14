use flowcloze::{parse_markdown, to_intermediate_json, IntermediateDocument};
use std::fs;

fn fixture(path: &str) -> String {
    fs::read_to_string(format!("tests/fixtures/{path}")).unwrap()
}

#[test]
fn qblock抽出結果をjsonに変換できる() {
    let markdown = fixture("mvp-context.md");
    let qblocks = parse_markdown(&markdown).unwrap();

    let json = to_intermediate_json("notes/os.md", &qblocks).unwrap();

    let document: IntermediateDocument = serde_json::from_str(&json).unwrap();
    let qblock = &document.qblocks[0];

    assert_eq!(document.meta.source, "notes/os.md");
    assert_eq!(qblock.id, "sem-001");
    assert_eq!(qblock.mode.as_deref(), Some("context"));
    assert_eq!(qblock.title.as_deref(), Some("セマフォ"));
    assert!(qblock
        .source_text
        .contains("セマフォはOSが提供するプロセス間同期機能の一つである。"));
    assert!(qblock
        .targets
        .iter()
        .any(|target| target.answer == "セマフォ" && target.target_type == "term-name"));
    assert!(qblock
        .targets
        .iter()
        .any(|target| target.answer == "プロセス間同期機能" && target.target_type == "meaning"));
    assert!(qblock
        .targets
        .iter()
        .any(|target| target.answer == "待ち状態"));
    assert!(qblock.targets.iter().any(|target| target.answer == "解放"));
}
