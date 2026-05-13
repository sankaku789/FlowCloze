use flowcloze::gemini::strip_markdown_code_fence;

#[test]
fn yamlコードフェンスを取り除ける() {
    let text = "```yaml\nquestions:\n  - id: sem-001\n```";

    assert_eq!(
        strip_markdown_code_fence(text),
        "questions:\n  - id: sem-001"
    );
}

#[test]
fn コードフェンスがない場合はtrimだけ行う() {
    assert_eq!(
        strip_markdown_code_fence("  questions: []\n"),
        "questions: []"
    );
}
