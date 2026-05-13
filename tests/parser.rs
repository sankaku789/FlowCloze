use flowcloze::models::Target;
use flowcloze::parse_markdown;
use std::fs;

fn fixture(path: &str) -> String {
    fs::read_to_string(format!("tests/fixtures/{path}")).unwrap()
}

#[test]
fn mvp_qblockを解析できる() {
    let markdown = fixture("mvp.md");
    let qblocks = parse_markdown(&markdown).unwrap();

    assert_eq!(qblocks.len(), 1);
    let qblock = &qblocks[0];
    assert_eq!(qblock.id, "sem-001");
    assert_eq!(qblock.title.as_deref(), Some("セマフォ"));
    assert_eq!(
        qblock.source_text,
        "セマフォはOSが提供するプロセス間同期機能の一つである。\nP命令はリソースの獲得を要求する。"
    );
    assert_eq!(
        qblock.targets,
        vec![
            Target {
                answer: "セマフォ".to_string(),
                target_type: "term-name".to_string(),
            },
            Target {
                answer: "プロセス間同期機能".to_string(),
                target_type: "meaning".to_string(),
            },
            Target {
                answer: "P命令".to_string(),
                target_type: "term-name".to_string(),
            },
            Target {
                answer: "獲得".to_string(),
                target_type: "process".to_string(),
            },
        ]
    );
}

#[test]
fn 未定義タイプは警告にする() {
    let markdown = fixture("unknown-type.md");
    let qblock = parse_markdown(&markdown).unwrap().remove(0);

    assert_eq!(
        qblock.targets,
        vec![Target {
            answer: "答え".to_string(),
            target_type: "custom-type".to_string(),
        }]
    );
    assert_eq!(
        qblock.warnings,
        vec!["answer '答え' のtarget type 'custom-type' は未定義です".to_string()]
    );
}

#[test]
fn 重複idは警告にする() {
    let markdown = fixture("duplicate-id.md");
    let qblocks = parse_markdown(&markdown).unwrap();

    assert_eq!(
        qblocks[0].warnings,
        vec!["qblock id 'dup' が重複しています".to_string()]
    );
    assert_eq!(
        qblocks[1].warnings,
        vec!["qblock id 'dup' が重複しています".to_string()]
    );
}

#[test]
fn コードフェンス内のqblock例は無視する() {
    let markdown = fixture("fenced-example.md");
    let qblocks = parse_markdown(&markdown).unwrap();

    assert_eq!(
        qblocks
            .iter()
            .map(|qblock| qblock.id.as_str())
            .collect::<Vec<_>>(),
        vec!["real"]
    );
}

#[test]
fn 閉じていないqblockはエラーにする() {
    let markdown = fixture("unclosed.md");
    let error = parse_markdown(&markdown).unwrap_err().to_string();

    assert!(error.contains("閉じられていません"));
}

#[test]
fn idがないqblockはエラーにする() {
    let markdown = fixture("missing-id.md");
    let error = parse_markdown(&markdown).unwrap_err().to_string();

    assert!(error.contains("必須属性 id がありません"));
}
