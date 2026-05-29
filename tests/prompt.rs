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
    assert!(prompt.contains("answersは空欄を戻したときに文が自然か確認するためだけに使い"));
    assert!(prompt.contains("生成対象は question 本文だけ"));
    assert!(prompt.contains("断片的な文を1つの自然な文章補完問題の文章へ整えたもの"));
    assert!(prompt.contains("question内の ＿＿＿ の数は tasks[].blank_count と一致させる"));
    assert!(prompt.contains("questionの ＿＿＿ は tasks[].answers と同じ順序で対応する"));
    assert!(prompt.contains("穴埋め下書きにある ＿＿＿ の数と順序は変えない"));
    assert!(prompt.contains("空欄の中身を分割して一部だけ本文側へ出さない"));
    assert!(prompt.contains("空欄へ戻したとき，question全体が常体の自然な日本語文"));
    assert!(prompt.contains("文同士をつなぎ直しながら本文に残す"));
    assert!(prompt.contains("箇条書きや見出しは，必要に応じて読める文章へ言い換える"));
    assert!(prompt.contains("question本文は常体（だ・である調）で書く"));
    assert!(prompt.contains("question本文の各段落は全角スペースで始める"));
    assert!(prompt.contains(
        "section / type / targets / answers / source_text は中間データから決定的に組み立てる"
    ));
    assert!(prompt.contains("questionの並び順も中間データのtasks順に整える"));
    assert!(prompt.contains("固定フィールドを推測・整形しなくてよい"));
    assert!(prompt.contains("各questionは id と question だけを持つ"));
    assert!(prompt.contains("Gemini用入力"));
    assert!(prompt.contains(r#""id": "qblock-001""#));
    assert!(prompt.contains(r#""blank_count": 7"#));
    assert!(prompt.contains(r#""cloze_template""#));
    assert!(prompt.contains(r#""answers""#));
    assert!(prompt.contains(r#""セマフォ""#));
    assert!(prompt.contains("元の文章:"));
    assert!(prompt.contains("穴埋め下書き:"));
    assert!(!prompt.contains(r#""type": "term-name""#));
    assert!(!prompt.contains(r#""meta""#));
    assert!(!prompt.contains(r#""source""#));
    assert!(!prompt.contains(r#""targets""#));
}
