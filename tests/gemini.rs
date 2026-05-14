use flowcloze::gemini::strip_markdown_code_fence;

#[test]
fn jsonコードフェンスを取り除ける() {
    let text = "```json\n{\"questions\": []}\n```";

    assert_eq!(strip_markdown_code_fence(text), "{\"questions\": []}");
}

#[test]
fn コードフェンスがない場合はtrimだけ行う() {
    assert_eq!(
        strip_markdown_code_fence("  {\"questions\": []}\n"),
        "{\"questions\": []}"
    );
}
