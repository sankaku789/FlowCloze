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
    let task = &document.tasks[0];

    assert_eq!(document.schema_version, 3);
    assert_eq!(document.meta.source, "notes/os.md");
    assert_eq!(document.meta.format.block_separator, "\n\n");
    assert_eq!(task.id, "qblock-001");
    assert_eq!(task.task_type, "context-cloze");
    assert_eq!(task.section.as_deref(), Some("セマフォ"));
    assert!(task
        .source
        .plain
        .contains("セマフォはOSが提供するプロセス間同期機能の一つである．"));
    assert_eq!(task.blocks[0].id, "qblock-001-b001");
    assert!(!task.blocks[0].starts_new_paragraph);
    assert_eq!(task.blocks[0].target_refs, vec![0, 1, 2, 3, 4, 5, 6]);
    assert!(task.cloze_template.contains("＿＿＿はOSが提供する＿＿＿"));
    assert_eq!(task.answers[0], "セマフォ");
    assert!(task
        .targets
        .iter()
        .any(|target| target.answer == "セマフォ" && target.target_type == "term-name"));
    assert!(task
        .targets
        .iter()
        .any(|target| target.answer == "プロセス間同期機能" && target.target_type == "meaning"));
    assert!(task
        .targets
        .iter()
        .any(|target| target.answer == "待ち状態"));
    assert!(task.targets.iter().any(|target| target.answer == "解放"));
}

#[test]
fn qblock内の見出しは本文として扱う() {
    let markdown = r#"
# 単元

#qblock{
## 見出しA
- [情報システム]{term-name}は[目的を達成する仕組み]{meaning}である．
### 見出しB
- [ソフトウェア]{term-name}は[プログラム]{meaning}である．
}
"#;
    let qblocks = parse_markdown(markdown).unwrap();

    let json = to_intermediate_json("notes/se.md", &qblocks).unwrap();
    let document: IntermediateDocument = serde_json::from_str(&json).unwrap();
    let task = &document.tasks[0];

    assert_eq!(task.blocks.len(), 1);
    assert_eq!(
        task.blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        vec!["## 見出しA\n- 情報システムは目的を達成する仕組みである．\n### 見出しB\n- ソフトウェアはプログラムである．"]
    );
    assert!(!task.blocks[0].starts_new_paragraph);
    assert!(task.cloze_template.contains("元の文章:\n## 見出しA"));
    assert!(task.cloze_template.contains("### 見出しB"));
    assert!(task
        .cloze_template
        .contains("穴埋め下書き:\n　## 見出しA\n- ＿＿＿は＿＿＿である．\n### 見出しB\n- ＿＿＿は＿＿＿である．"));
}
