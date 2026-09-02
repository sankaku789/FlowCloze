use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use flowcloze::{
    ComposeBatchRequest, ComposeError, ComposeTask, GeminiAdapter, GeminiClient, LocalOpenAiClient,
    OpenAiCompatibleAdapter, OpenAiCompatiblePool, QuestionComposer, StructuredOutputMode,
    WritingStyle,
};

fn request() -> ComposeBatchRequest {
    ComposeBatchRequest {
        schema_version: 1,
        batch_id: "b".into(),
        tasks: vec![ComposeTask {
            id: "q1".into(),
            source_text: "source".into(),
            scaffold_question: "＿＿＿".into(),
            answers: vec!["answer".into()],
            blank_token: "＿＿＿".into(),
            blank_tokens: vec!["＿＿＿".into()],
            blank_count: 1,
        }],
        style: WritingStyle::PlainJapanese,
        prompt_version: "test".into(),
        extra_constraints: Vec::new(),
        retry_feedback: Vec::new(),
    }
}

fn mock_capture(status: u16, body: &'static str) -> (String, Arc<Mutex<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(String::new()));
    let destination = captured.clone();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 8192];
        let size = stream.read(&mut request).unwrap();
        *destination.lock().unwrap() = String::from_utf8_lossy(&request[..size]).into_owned();
        write!(
            stream,
            "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    (format!("http://{address}"), captured)
}

fn mock(responses: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(Mutex::new(0));
    let counter = calls.clone();
    thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            *counter.lock().unwrap() += 1;
            let reason = if status == 200 { "OK" } else { "Bad Request" };
            write!(
                stream,
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    (format!("http://{address}"), calls)
}

#[test]
fn both_adapters_use_the_common_fence_parser() {
    let gemini_body = r#"{"candidates":[{"content":{"parts":[{"text":"```json\n{\"items\":[{\"id\":\"q1\",\"question\":\"＿＿＿\"}]}\n```"}]}}]}"#;
    let (url, _) = mock(vec![(200, gemini_body)]);
    let gemini = GeminiAdapter::new("key", "model")
        .with_base_url(url)
        .with_structured_output(StructuredOutputMode::Off);
    assert_eq!(gemini.compose(&request()).unwrap().items[0].id, "q1");

    let openai_body = r#"{"choices":[{"message":{"content":"```json\n{\"items\":[{\"id\":\"q1\",\"question\":\"＿＿＿\"}]}\n```"}}]}"#;
    let (url, _) = mock(vec![(200, openai_body)]);
    let openai = OpenAiCompatibleAdapter::new(url, "model", None)
        .with_structured_output(StructuredOutputMode::Off);
    assert_eq!(openai.compose(&request()).unwrap().items[0].id, "q1");
}

#[test]
fn auto_downgrades_only_once_and_caches_gemini_schema_rejection() {
    let body = r#"{"candidates":[{"content":{"parts":[{"text":"{\"items\":[{\"id\":\"q1\",\"question\":\"＿＿＿\"}]}"}]}}]}"#;
    let (url, calls) = mock(vec![
        (
            400,
            r#"{"error":{"status":"INVALID_ARGUMENT","message":"responseJsonSchema unsupported"}}"#,
        ),
        (200, body),
        (200, body),
    ]);
    let adapter = GeminiAdapter::new("key", "model").with_base_url(url);
    adapter.compose(&request()).unwrap();
    adapter.compose(&request()).unwrap();
    assert_eq!(*calls.lock().unwrap(), 3);
}

#[test]
fn openai_auto_accepts_compatible_schema_rejection_wording() {
    let body =
        r#"{"choices":[{"message":{"content":"{\"items\":[{\"id\":\"q1\",\"question\":\"＿＿＿\"}]}"}}]}"#;
    let (url, calls) = mock(vec![
        (400, r#"{"error":"response_format json_schema is not supported"}"#),
        (200, body),
        (200, body),
    ]);
    let adapter = OpenAiCompatibleAdapter::new(url, "model", None);
    adapter.compose(&request()).unwrap();
    adapter.compose(&request()).unwrap();
    assert_eq!(*calls.lock().unwrap(), 3);
}

#[test]
fn empty_openai_pool_is_configuration_error() {
    let pool = OpenAiCompatiblePool::new(Vec::new());
    assert!(matches!(
        pool.compose(&request()),
        Err(ComposeError::Configuration)
    ));
}

#[test]
fn legacy_clients_keep_questions_schema_and_text_response() {
    let gemini_response =
        r#"{"candidates":[{"content":{"parts":[{"text":"```json\n{\"questions\":[]}\n```"}]}}]}"#;
    let (url, captured) = mock_capture(200, gemini_response);
    let text = GeminiClient::new("key", "model")
        .with_base_url(url)
        .generate_text("legacy")
        .unwrap();
    assert_eq!(text, "```json\n{\"questions\":[]}\n```");
    let request = captured.lock().unwrap().clone();
    assert!(request.contains("questions"));
    assert!(request.contains("\"required\":[\"id\",\"question\"]"));
    assert!(request.contains("\"additionalProperties\":false"));
    assert!(!request.contains("flowcloze_compose"));

    let openai_response = r#"{"choices":[{"message":{"content":"{\"questions\":[]}"}}]}"#;
    let (url, captured) = mock_capture(200, openai_response);
    let client = LocalOpenAiClient::new(url, "model", None);
    let _clone = client.clone();
    assert_eq!(
        client.generate_text("legacy").unwrap(),
        "{\"questions\":[]}"
    );
    let request = captured.lock().unwrap().clone();
    assert!(request.contains("questions"));
    assert!(request.contains("\"required\":[\"id\",\"question\"]"));
    assert!(request.contains("\"additionalProperties\":false"));
    assert!(!request.contains("flowcloze_compose"));
}
