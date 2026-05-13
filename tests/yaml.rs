use flowcloze::{parse_markdown, to_intermediate_yaml};
use std::fs;

fn fixture(path: &str) -> String {
    fs::read_to_string(format!("tests/fixtures/{path}")).unwrap()
}

#[test]
fn qblock抽出結果をyamlに変換できる() {
    let markdown = fixture("mvp-context.md");
    let qblocks = parse_markdown(&markdown).unwrap();

    let yaml = to_intermediate_yaml("notes/os.md", &qblocks).unwrap();

    assert!(yaml.contains("meta:\n  source: notes/os.md\n"));
    assert!(yaml.contains("qblocks:"));
    assert!(yaml.contains("id: sem-001"));
    assert!(yaml.contains("mode: context"));
    assert!(yaml.contains("title: セマフォ"));
    assert!(yaml.contains("source_text: |-"));
    assert!(yaml.contains("セマフォはOSが提供するプロセス間同期機能の一つである。"));
    assert!(yaml.contains("answer: セマフォ"));
    assert!(yaml.contains("type: term-name"));
    assert!(yaml.contains("answer: プロセス間同期機能"));
    assert!(yaml.contains("type: meaning"));
    assert!(yaml.contains("answer: 待ち状態"));
    assert!(yaml.contains("answer: 解放"));
}
