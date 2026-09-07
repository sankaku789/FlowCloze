use flowcloze::{
    parse_markdown, to_intermediate_json, validate_generated_json, FixedField, ValidationError,
};
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
fn detects_answer_leakage_in_question_body() {
    // 空欄数は正しいが，answerが別の本文位置に残っているケースを検証する．
    let intermediate_json = r#"{
        "meta": {"source": "inline.md"},
        "qblocks": [{
            "id": "q1",
            "source_text": "短期記憶はワーキングメモリである。",
            "targets": [{"answer": "ワーキングメモリ", "type": "term"}],
            "warnings": []
        }]
    }"#;
    let generated_json = r#"{
        "questions": [{
            "id": "q1",
            "type": "context-cloze",
            "question": "短期記憶は＿＿＿であり，ワーキングメモリは重要である。",
            "answers": ["ワーキングメモリ"]
        }]
    }"#;

    let report = validate_generated_json(intermediate_json, generated_json);

    assert!(report.errors.contains(&ValidationError::AnswerLeakage {
        id: "q1".to_string(),
        answer: "ワーキングメモリ".to_string(),
    }));
}

#[test]
fn allows_non_target_occurrences_but_detects_new_answer_occurrences() {
    let intermediate = r#"{"meta":{"source":"x"},"qblocks":[{
        "id":"q1","source_text":"alphaは[ではない]。alphaを説明するalpha。","targets":[{"answer":"alpha","type":"term"},{"answer":"alpha","type":"term"}],"warnings":[]
    }]}"#;
    let valid = r#"{"questions":[{"id":"q1","type":"context-cloze","question":"＿＿＿は[ではない]。＿＿＿を説明するalpha。","answers":["alpha","alpha"]}]}"#;
    assert!(validate_generated_json(intermediate, valid).is_valid());

    let leaked = valid.replace("説明するalpha。", "説明するalphaとalpha。");
    assert!(validate_generated_json(intermediate, &leaked)
        .errors
        .iter()
        .any(|error| matches!(error, ValidationError::AnswerLeakage { .. })));
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
      "question": "＿＿＿はOSの＿＿＿である．\n＿＿＿で＿＿＿し，だめなら＿＿＿になる．\n＿＿＿で＿＿＿する．",
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
      "question": "＿＿＿はOSの＿＿＿である．\n＿＿＿で＿＿＿し，だめなら＿＿＿になる．\n＿＿＿で＿＿＿する．",
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

#[test]
fn missing_and_order_mismatch_are_reported_only_when_ids_are_comparable() {
    let intermediate = r#"{"meta":{"source":"x"},"qblocks":[
        {"id":"q1","source_text":"a","targets":[],"warnings":[]},
        {"id":"q2","source_text":"b","targets":[],"warnings":[]}
    ]}"#;
    let missing =
        r#"{"questions":[{"id":"q1","type":"context-cloze","question":"","answers":[]}]}"#;
    let report = validate_generated_json(intermediate, missing);
    assert!(report.errors.contains(&ValidationError::MissingQuestion {
        id: "q2".to_string()
    }));
    assert!(!report
        .errors
        .iter()
        .any(|error| matches!(error, ValidationError::QuestionOrderMismatch { .. })));

    let swapped = r#"{"questions":[
        {"id":"q2","type":"context-cloze","question":"","answers":[]},
        {"id":"q1","type":"context-cloze","question":"","answers":[]}
    ]}"#;
    let report = validate_generated_json(intermediate, swapped);
    assert!(report
        .errors
        .iter()
        .any(|error| matches!(error, ValidationError::QuestionOrderMismatch { .. })));
}

#[test]
fn fixed_fields_reject_changes_but_allow_omitted_optional_fields() {
    let intermediate = r#"{"meta":{"source":"x"},"qblocks":[{
        "id":"q1","section":"s","source_text":"source","targets":[{"answer":"a","type":"term"}],"warnings":[]
    }]}"#;
    let compatible =
        r#"{"questions":[{"id":"q1","type":"context-cloze","question":"＿＿＿","answers":["a"]}]}"#;
    assert!(validate_generated_json(intermediate, compatible).is_valid());

    let changed = r#"{"questions":[{"id":"q1","section":"other","type":"other","targets":[],"question":"＿＿＿","answers":[],"source_text":"other"}]}"#;
    let report = validate_generated_json(intermediate, changed);
    for field in [
        FixedField::Section,
        FixedField::QuestionType,
        FixedField::Targets,
        FixedField::Answers,
        FixedField::SourceText,
    ] {
        assert!(report
            .errors
            .contains(&ValidationError::FixedFieldMismatch {
                id: "q1".to_string(),
                field,
            }));
    }
}

#[test]
fn empty_section_is_compatible_with_absent_intermediate_section() {
    let intermediate = r#"{"meta":{"source":"x"},"qblocks":[{"id":"q1","source_text":"source","targets":[{"answer":"a","type":"term"}],"warnings":[]}]}"#;
    for section in [r#""section":"""#, r#""section":null"#] {
        let generated = format!(
            r#"{{"questions":[{{"id":"q1",{section},"type":"context-cloze","question":"＿＿＿","answers":["a"]}}]}}"#
        );
        assert!(validate_generated_json(intermediate, &generated).is_valid());
    }
    let changed = r#"{"questions":[{"id":"q1","section":"changed","type":"context-cloze","question":"＿＿＿","answers":["a"]}]}"#;
    assert!(validate_generated_json(intermediate, changed)
        .errors
        .iter()
        .any(|error| matches!(
            error,
            ValidationError::FixedFieldMismatch {
                field: FixedField::Section,
                ..
            }
        )));
}

#[test]
fn duplicate_following_question_still_validates_its_content() {
    let intermediate = r#"{"meta":{"source":"x"},"qblocks":[{
        "id":"q1","source_text":"答えはalphaである。","targets":[{"answer":"alpha","type":"term"}],"warnings":[]
    }]}"#;
    let generated = r#"{"questions":[
        {"id":"q1","type":"context-cloze","question":"答えは＿＿＿である。","answers":["alpha"]},
        {"id":"q1","type":"context-cloze","question":"","answers":["other"]}
    ]}"#;

    let report = validate_generated_json(intermediate, generated);

    assert!(report
        .errors
        .iter()
        .any(|error| matches!(error, ValidationError::EmptyQuestion { id } if id == "q1")));
    assert!(report.errors.iter().any(|error| matches!(
        error,
        ValidationError::BlankAnswerCountMismatch { id, .. } if id == "q1"
    )));
    assert!(report.errors.iter().any(|error| matches!(
        error,
        ValidationError::AnswerNotInTargets { id, .. } if id == "q1"
    )));
    assert!(report.errors.iter().any(|error| matches!(
        error,
        ValidationError::MissingTargetAnswer { id, .. } if id == "q1"
    )));
}

#[test]
fn validation_display_redacts_question_and_answer_values() {
    let error = ValidationError::AnswerLeakage {
        id: "q1".to_string(),
        answer: "secret-answer".to_string(),
    };
    assert!(!error.to_string().contains("secret-answer"));
}
