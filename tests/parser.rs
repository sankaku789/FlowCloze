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
    assert_eq!(qblock.id, "qblock-001");
    assert_eq!(
        qblock.source_text,
        "セマフォはOSが提供するプロセス間同期機能の一つである．\nP命令はリソースの獲得を要求する．"
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
fn qblockを解析できる() {
    let markdown = r#"
#qblock{
[セマフォ]{term-name}はOSが提供する[プロセス間同期機能]{meaning}の一つである．
[P命令]{term-name}はリソースの[獲得]{process}を要求する．
}
"#;
    let qblocks = parse_markdown(markdown).unwrap();

    assert_eq!(qblocks.len(), 1);
    let qblock = &qblocks[0];
    assert_eq!(qblock.id, "qblock-001");
    assert_eq!(
        qblock.source_text,
        "セマフォはOSが提供するプロセス間同期機能の一つである．\nP命令はリソースの獲得を要求する．"
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
fn 直近の見出し1をqblockのsectionにする() {
    let markdown = r#"
# ソフトウェア工学の概論

#qblock{
[情報システム]{term-name}は目的を達成する仕組みである．
}

## ソフトウェア工学とは

#qblock{
[ソフトウェア工学]{term-name}は開発の問題を改善する．
}
"#;
    let qblocks = parse_markdown(markdown).unwrap();

    assert_eq!(
        qblocks
            .iter()
            .map(|qblock| qblock.section.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("ソフトウェア工学の概論"),
            Some("ソフトウェア工学の概論")
        ]
    );
}

#[test]
fn qblock内の先頭見出し1をsectionにする() {
    let markdown = r#"
#qblock{
# 組込みシステムとは
[組込みシステム]{term-name}は機器に組み込まれる．

## 構成要素
センサを持つ．
}
"#;

    let qblock = parse_markdown(markdown).unwrap().remove(0);

    assert_eq!(qblock.section.as_deref(), Some("組込みシステムとは"));
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
fn type未指定targetは既定タイプとして解析する() {
    let markdown = r#"
#qblock{
[セマフォ]はOSが提供する[プロセス間同期機能]の一つである．
}
"#;
    let qblocks = parse_markdown(markdown).unwrap();

    let qblock = &qblocks[0];
    assert_eq!(
        qblock.source_text,
        "セマフォはOSが提供するプロセス間同期機能の一つである．"
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
                target_type: "term-name".to_string(),
            },
        ]
    );
    assert!(qblock.warnings.is_empty());
}

#[test]
fn 空のtype指定は既定タイプとして解析する() {
    let markdown = r#"
#qblock{
[セマフォ]{}はOSが提供する機能である．
}
"#;
    let qblocks = parse_markdown(markdown).unwrap();

    let qblock = &qblocks[0];
    assert_eq!(qblock.source_text, "セマフォはOSが提供する機能である．");
    assert_eq!(
        qblock.targets,
        vec![Target {
            answer: "セマフォ".to_string(),
            target_type: "term-name".to_string(),
        }]
    );
    assert!(qblock.warnings.is_empty());
}

#[test]
fn 空白を含むtype指定はtargetとして扱わず表記を保持する() {
    let markdown = r#"
#qblock{
[壊れたtarget]{term name}はそのまま残し，[セマフォ]は抽出する．
}
"#;
    let qblocks = parse_markdown(markdown).unwrap();

    let qblock = &qblocks[0];
    assert_eq!(
        qblock.source_text,
        "[壊れたtarget]{term name}はそのまま残し，セマフォは抽出する．"
    );
    assert_eq!(
        qblock.targets,
        vec![Target {
            answer: "セマフォ".to_string(),
            target_type: "term-name".to_string(),
        }]
    );
}

#[test]
fn markdownリンクはtype未指定targetとして扱わない() {
    let markdown = r#"
#qblock{
[セマフォ]は[Wikipedia](https://example.com)に説明がある．
}
"#;
    let qblocks = parse_markdown(markdown).unwrap();

    let qblock = &qblocks[0];
    assert_eq!(
        qblock.source_text,
        "セマフォは[Wikipedia](https://example.com)に説明がある．"
    );
    assert_eq!(
        qblock.targets,
        vec![Target {
            answer: "セマフォ".to_string(),
            target_type: "term-name".to_string(),
        }]
    );
}

#[test]
fn 参照形式markdownリンクはtype未指定targetとして扱わない() {
    let markdown = r#"
#qblock{
[セマフォ]は[Wikipedia][wiki]や[解説][]に説明がある．
}
"#;
    let qblocks = parse_markdown(markdown).unwrap();

    let qblock = &qblocks[0];
    assert_eq!(
        qblock.source_text,
        "セマフォは[Wikipedia][wiki]や[解説][]に説明がある．"
    );
    assert_eq!(
        qblock.targets,
        vec![Target {
            answer: "セマフォ".to_string(),
            target_type: "term-name".to_string(),
        }]
    );
}

#[test]
fn 閉じていないtype指定は無効なtargetとして後続解析を続ける() {
    let markdown = r#"
#qblock{
[壊れたtarget]{term-name はそのまま残し，[セマフォ]は抽出する．
}
"#;
    let qblocks = parse_markdown(markdown).unwrap();

    let qblock = &qblocks[0];
    assert_eq!(
        qblock.source_text,
        "[壊れたtarget]{term-name はそのまま残し，セマフォは抽出する．"
    );
    assert_eq!(
        qblock.targets,
        vec![Target {
            answer: "セマフォ".to_string(),
            target_type: "term-name".to_string(),
        }]
    );
}

#[test]
fn qblockには連番idを割り当てる() {
    let markdown = fixture("duplicate-id.md");
    let qblocks = parse_markdown(&markdown).unwrap();

    assert_eq!(
        qblocks
            .iter()
            .map(|qblock| qblock.id.as_str())
            .collect::<Vec<_>>(),
        vec!["qblock-001", "qblock-002"]
    );
    assert!(qblocks.iter().all(|qblock| qblock.warnings.is_empty()));
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
        vec!["qblock-001"]
    );
}

#[test]
fn 閉じていないqblockはエラーにする() {
    let markdown = fixture("unclosed.md");
    let error = parse_markdown(&markdown).unwrap_err().to_string();

    assert!(error.contains("閉じられていません"));
}

#[test]
fn qblockには自動idを割り当てる() {
    let markdown = fixture("missing-id.md");
    let qblocks = parse_markdown(&markdown).unwrap();

    assert_eq!(qblocks[0].id, "qblock-001");
}

#[test]
fn qblockが複数ある場合は連番にする() {
    let markdown = r#"
#qblock{
[A]{term-name}
}

#qblock{
[B]{term-name}
}
"#;
    let qblocks = parse_markdown(markdown).unwrap();

    assert_eq!(
        qblocks
            .iter()
            .map(|qblock| qblock.id.as_str())
            .collect::<Vec<_>>(),
        vec!["qblock-001", "qblock-002"]
    );
}
