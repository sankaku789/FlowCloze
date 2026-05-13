use flowcloze::{parse_markdown, to_intermediate_yaml, validate_generated_yaml, ValidationError};
use std::fs;

fn fixture(path: &str) -> String {
    fs::read_to_string(format!("tests/fixtures/{path}")).unwrap()
}

fn intermediate_yaml() -> String {
    let markdown = fixture("mvp-context.md");
    let qblocks = parse_markdown(&markdown).unwrap();
    to_intermediate_yaml("tests/fixtures/mvp-context.md", &qblocks).unwrap()
}

#[test]
fn 正しい生成結果yamlを検証できる() {
    let intermediate_yaml = intermediate_yaml();
    let generated_yaml = fixture("generated-valid.yaml");

    let report = validate_generated_yaml(&intermediate_yaml, &generated_yaml);

    assert!(report.is_valid());
}

#[test]
fn 空欄数とanswers数の不一致を検出する() {
    let intermediate_yaml = intermediate_yaml();
    let generated_yaml = fixture("generated-blank-mismatch.yaml");

    let report = validate_generated_yaml(&intermediate_yaml, &generated_yaml);

    assert!(report
        .errors
        .contains(&ValidationError::BlankAnswerCountMismatch {
            id: "sem-001".to_string(),
            blank_count: 2,
            answer_count: 1,
        }));
}

#[test]
fn targetsにないanswerを検出する() {
    let intermediate_yaml = intermediate_yaml();
    let generated_yaml = fixture("generated-unknown-answer.yaml");

    let report = validate_generated_yaml(&intermediate_yaml, &generated_yaml);

    assert!(report
        .errors
        .contains(&ValidationError::AnswerNotInTargets {
            id: "sem-001".to_string(),
            answer: "ミューテックス".to_string(),
        }));
    assert!(report
        .errors
        .contains(&ValidationError::MissingTargetAnswer {
            id: "sem-001".to_string(),
            answer: "解放".to_string(),
        }));
}
