use flowcloze::{parse_markdown, to_intermediate_json, validate_generated_json, ValidationError};
use std::fs;

fn fixture(path: &str) -> String {
    fs::read_to_string(format!("tests/fixtures/{path}")).unwrap()
}

fn intermediate_json() -> String {
    let markdown = fixture("mvp-context.md");
    let qblocks = parse_markdown(&markdown).unwrap();
    to_intermediate_json("tests/fixtures/mvp-context.md", &qblocks).unwrap()
}

#[test]
fn 正しい生成結果jsonを検証できる() {
    let intermediate_json = intermediate_json();
    let generated_json = fixture("generated-valid.json");

    let report = validate_generated_json(&intermediate_json, &generated_json);

    assert!(report.is_valid());
}

#[test]
fn tagsとwarningsが空値でも空配列として扱う() {
    let intermediate_json = intermediate_json();
    let generated_json = r#"
{
  "questions": [
    {
      "id": "qblock-001",
      "type": "context-cloze",
      "question": "＿＿＿はOSの＿＿＿である。\n＿＿＿で＿＿＿し，だめなら＿＿＿になる。\n＿＿＿で＿＿＿する。",
      "answers": [
        "セマフォ",
        "プロセス間同期機能",
        "P命令",
        "獲得",
        "待ち状態",
        "V命令",
        "解放"
      ],
      "tags": null,
      "warnings": null
    }
  ]
}
"#;

    let report = validate_generated_json(&intermediate_json, generated_json);

    assert!(report.is_valid());
}

#[test]
fn answersが入れ子配列でも平坦化して検証する() {
    let intermediate_json = intermediate_json();
    let generated_json = r#"
{
  "questions": [
    {
      "id": "qblock-001",
      "type": "context-cloze",
      "question": "＿＿＿はOSの＿＿＿である。\n＿＿＿で＿＿＿し，だめなら＿＿＿になる。\n＿＿＿で＿＿＿する。",
      "answers": [
        ["セマフォ", "プロセス間同期機能"],
        "P命令",
        "獲得",
        "待ち状態",
        ["V命令", "解放"]
      ]
    }
  ]
}
"#;

    let report = validate_generated_json(&intermediate_json, generated_json);

    assert!(report.is_valid());
}

#[test]
fn 空欄数とanswers数の不一致を検出する() {
    let intermediate_json = intermediate_json();
    let generated_json = fixture("generated-blank-mismatch.json");

    let report = validate_generated_json(&intermediate_json, &generated_json);

    assert!(report
        .errors
        .contains(&ValidationError::BlankAnswerCountMismatch {
            id: "qblock-001".to_string(),
            blank_count: 2,
            answer_count: 1,
        }));
}

#[test]
fn targetsにないanswerを検出する() {
    let intermediate_json = intermediate_json();
    let generated_json = fixture("generated-unknown-answer.json");

    let report = validate_generated_json(&intermediate_json, &generated_json);

    assert!(report
        .errors
        .contains(&ValidationError::AnswerNotInTargets {
            id: "qblock-001".to_string(),
            answer: "ミューテックス".to_string(),
        }));
    assert!(report
        .errors
        .contains(&ValidationError::MissingTargetAnswer {
            id: "qblock-001".to_string(),
            answer: "解放".to_string(),
        }));
}
