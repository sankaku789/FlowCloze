use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use flowcloze::config::auth_store::AuthStore;
use flowcloze::providers::model_registry::{ModelProfile, ModelRegistry};
use flowcloze::{
    build_adapter, AuthRequirement, ComposeBatchRequest, ComposeError, ComposeTask,
    OpenAiCompatibleAdapter, OpenAiCompatiblePool, ProviderCatalog, ProviderDefinition,
    QuestionComposer, StructuredOutputMode,
};

fn request() -> ComposeBatchRequest {
    ComposeBatchRequest {
        batch_id: "b".into(),
        tasks: vec![ComposeTask {
            id: "q1".into(),
            scaffold_question: "<BLANK_0>".into(),
            targets: vec!["answer".into()],
            blank_count: 1,
        }],
        prompt_version: "test".into(),
        extra_constraints: Vec::new(),
        retry_feedback: Vec::new(),
    }
}

fn read_complete_request(stream: &mut TcpStream) {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut expected_len = None;

    loop {
        let read = stream.read(&mut chunk).unwrap();
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);

        if expected_len.is_none() {
            if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let header_end = header_end + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_len = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                expected_len = Some(header_end + content_len);
            }
        }

        if expected_len.is_some_and(|len| request.len() >= len) {
            break;
        }
    }
}

fn mock(responses: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(Mutex::new(0));
    let counter = calls.clone();
    thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            read_complete_request(&mut stream);
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
fn openai_adapter_uses_the_common_fence_parser() {
    let openai_body = r#"{"choices":[{"message":{"content":"```json\n{\"items\":[{\"id\":\"q1\",\"question\":\"<BLANK_0>\"}]}\n```"}}]}"#;
    let (url, _) = mock(vec![(200, openai_body)]);
    let openai = OpenAiCompatibleAdapter::new(url, "model", None)
        .with_structured_output(StructuredOutputMode::Off)
        .with_legacy_compose(true);
    assert_eq!(openai.compose(&request()).unwrap().items[0].id, "q1");
}

#[test]
fn google_catalog_and_factory_use_the_openai_compatible_contract() {
    let body = r#"{"choices":[{"message":{"content":"{\"items\":[{\"id\":\"q1\",\"question\":\"<BLANK_0>\"}]}"}}]}"#;
    let (url, _) = mock(vec![(200, body)]);
    let mut providers = ProviderCatalog::default();
    providers
        .register(ProviderDefinition {
            id: "google".into(),
            base_url: url,
            auth: AuthRequirement::ApiKey,
        })
        .unwrap();
    let mut models = ModelRegistry::default();
    models
        .register(ModelProfile {
            name: "google-test".into(),
            provider: "google".into(),
            model: "gemini-test".into(),
        })
        .unwrap();
    let model = models.resolve("google-test", &providers).unwrap();
    let mut auth = AuthStore::default();
    auth.set_api_key("google", "key").unwrap();

    let output = build_adapter(&model, &auth)
        .unwrap()
        .with_structured_output(StructuredOutputMode::Off)
        .with_legacy_compose(true)
        .compose(&request())
        .unwrap();

    assert_eq!(output.items[0].id, "q1");
    assert_eq!(output.metadata.provider, "google");
    assert_eq!(output.metadata.model, "gemini-test");
}

#[test]
fn openai_auto_falls_back_to_json_object_and_caches_it() {
    let body = r#"{"choices":[{"message":{"content":"{\"items\":[{\"id\":\"q1\",\"question\":\"<BLANK_0>\"}]}"}}]}"#;
    let (url, calls) = mock(vec![
        (
            400,
            r#"{"error":"response_format json_schema is not supported"}"#,
        ),
        (200, body),
        (200, body),
    ]);
    let adapter = OpenAiCompatibleAdapter::new(url, "model", None).with_legacy_compose(true);
    adapter.compose(&request()).unwrap();
    adapter.compose(&request()).unwrap();
    assert_eq!(*calls.lock().unwrap(), 3);
}

#[test]
fn openai_adapter_defaults_to_target_aware_segment_protocol() {
    let body = r#"{"choices":[{"message":{"content":"{\"items\":{\"q1\":{\"segments\":[\"before \",\" after\"]}}}"}}]}"#;
    let (url, _) = mock(vec![(200, body)]);
    let adapter = OpenAiCompatibleAdapter::new(url, "model", None)
        .with_structured_output(StructuredOutputMode::Off);
    let output = adapter.compose(&request()).unwrap();
    assert_eq!(output.items[0].question, "before <BLANK_0> after");
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
fn openai_auto_falls_back_to_prompt_only_and_caches_it() {
    let body = r#"{"choices":[{"message":{"content":"{\"items\":[{\"id\":\"q1\",\"question\":\"<BLANK_0>\"}]}"}}]}"#;
    let (url, calls) = mock(vec![
        (
            400,
            r#"{"error":"response_format json_schema is not supported"}"#,
        ),
        (
            400,
            r#"{"error":"response_format json_object is not supported"}"#,
        ),
        (200, body),
        (200, body),
    ]);
    let adapter = OpenAiCompatibleAdapter::new(url, "model", None).with_legacy_compose(true);
    adapter.compose(&request()).unwrap();
    adapter.compose(&request()).unwrap();
    assert_eq!(*calls.lock().unwrap(), 4);
}

#[test]
fn openai_normalizes_content_parts() {
    let body = r#"{"choices":[{"message":{"content":[{"type":"text","text":"{\"items\":["},{"type":"text","text":"{\"id\":\"q1\",\"question\":\"<BLANK_0>\"}]}"}]}}]}"#;
    let (url, _) = mock(vec![(200, body)]);
    let adapter = OpenAiCompatibleAdapter::new(url, "model", None)
        .with_structured_output(StructuredOutputMode::Off)
        .with_legacy_compose(true);
    assert_eq!(adapter.compose(&request()).unwrap().items[0].id, "q1");
}
