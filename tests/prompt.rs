use flowcloze::{build_generation_prompt, parse_markdown, IntermediateDocument};
use std::fs;

#[test]
fn プロンプトに中間データと制約を含める() {
    let markdown = fs::read_to_string("tests/fixtures/mvp-context.md").unwrap();
    let qblocks = parse_markdown(&markdown).unwrap();
    let document = IntermediateDocument::from_qblocks("tests/fixtures/mvp-context.md", &qblocks);

    let prompt = build_generation_prompt(&document).unwrap();

    assert!(prompt.contains("JSONのみ"));
    assert!(prompt.contains("バラバラのノート文を1つの文章補完問題の本文へ編集すること"));
    assert!(prompt.contains("あなたはquestion本文の編集に集中してください"));
    assert!(prompt.contains("生成対象は question 本文だけ"));
    assert!(prompt.contains("断片的な文を1つの自然な文章補完問題の文章へ整えたもの"));
    assert!(prompt.contains("穴埋め下書きにある ＿＿＿ の数と順序は変えない"));
    assert!(prompt.contains("文同士をつなぎ直しながら本文に残す"));
    assert!(prompt.contains("箇条書きや見出しは，必要に応じて読める文章へ言い換える"));
    assert!(prompt.contains("question本文は常体（だ・である調）で書く"));
    assert!(prompt.contains("section / type / targets / answers / source_text は中間データから決定的に組み立てる"));
    assert!(prompt.contains("questionの並び順も中間データのtasks順に整える"));
    assert!(prompt.contains("固定フィールドを推測・整形しなくてよい"));
    assert!(prompt.contains("各questionは id と question だけを持つ"));
    assert!(prompt.contains("空欄数チェック"));
    assert!(prompt.contains("- qblock-001: question内の ＿＿＿ は7個"));
    assert!(prompt.contains(r#""id": "qblock-001""#));
    assert!(prompt.contains("元の文章:"));
    assert!(prompt.contains("穴埋め下書き:"));
    assert!(prompt.contains(r#""answer": "セマフォ""#));
    assert!(prompt.contains(r#""type": "term-name""#));
}
