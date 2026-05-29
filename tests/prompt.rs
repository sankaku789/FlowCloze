use flowcloze::{build_generation_prompt, parse_markdown, IntermediateDocument};
use std::fs;

#[test]
fn プロンプトに中間データと制約を含める() {
    let markdown = fs::read_to_string("tests/fixtures/mvp-context.md").unwrap();
    let qblocks = parse_markdown(&markdown).unwrap();
    let document = IntermediateDocument::from_qblocks("tests/fixtures/mvp-context.md", &qblocks);

    let prompt = build_generation_prompt(&document).unwrap();

    assert!(prompt.contains("JSONのみを出力する"));
    assert!(prompt.contains("targetsにないanswerを追加しない"));
    assert!(prompt.contains("question内の空欄順，answersの順序，入力targetsの順序を一致させる"));
    assert!(prompt.contains("1つのtargetにつき，question内に必ず1つの ＿＿＿ を置く"));
    assert!(prompt.contains("意味が近いtarget同士でも，1つの空欄にまとめない"));
    assert!(prompt.contains("source_text全体を問題文の素材として扱い"));
    assert!(prompt.contains("target以外の語句は空欄にせず"));
    assert!(prompt.contains("qblockが大きい場合でも1つのquestionにまとめる"));
    assert!(
        prompt.contains("source_textをそのまま抜き出してtargetだけを置換しただけの出力にしない")
    );
    assert!(prompt.contains("学習者に提示する文章補完問題として自然な本文に再構成する"));
    assert!(prompt.contains("生成前チェックリスト"));
    assert!(prompt.contains(r#"- qblock-001: blanks=7, answers=["セマフォ","プロセス間同期機能","P命令","獲得","待ち状態","V命令","解放"]"#));
    assert!(prompt.contains(r#""id": "qblock-001""#));
    assert!(prompt.contains(r#""answer": "セマフォ""#));
    assert!(prompt.contains(r#""type": "term-name""#));
}
