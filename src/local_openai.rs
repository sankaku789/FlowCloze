//! OpenAI Chat Completions互換provider adapter。
use crate::compose::{
    ComposeBatchOutput, ComposeBatchRequest, ComposeError, ComposeMetadata, QuestionComposer,
};
use crate::gemini::StructuredOutputMode;
use crate::http::{json_headers, HttpError, HttpTransport};
use crate::prompt::build_compose_request_prompt;
use reqwest::header::{HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;
const UNKNOWN: u8 = 0;
const SUPPORTED: u8 = 1;
const UNSUPPORTED: u8 = 2;
#[derive(Debug)]
pub struct OpenAiCompatibleAdapter {
    base_url: String,
    model: String,
    api_key: Option<String>,
    mode: StructuredOutputMode,
    state: AtomicU8,
    probe: Mutex<()>,
    transport: HttpTransport,
}
impl Clone for OpenAiCompatibleAdapter {
    fn clone(&self) -> Self {
        Self {
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            api_key: self.api_key.clone(),
            mode: self.mode,
            state: AtomicU8::new(self.state.load(Ordering::Acquire)),
            probe: Mutex::new(()),
            transport: self.transport.clone(),
        }
    }
}
impl OpenAiCompatibleAdapter {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').into(),
            model: model.into(),
            api_key: api_key.filter(|x| !x.trim().is_empty()),
            mode: StructuredOutputMode::Auto,
            state: AtomicU8::new(UNKNOWN),
            probe: Mutex::new(()),
            transport: HttpTransport::default(),
        }
    }
    pub fn with_structured_output(mut self, mode: StructuredOutputMode) -> Self {
        self.mode = mode;
        self
    }
    pub fn with_transport(mut self, transport: HttpTransport) -> Self {
        self.transport = transport;
        self
    }
    fn request(&self, prompt: &str, structured: bool) -> Result<String, HttpError> {
        let mut body = json!({"model":self.model,"messages":[{"role":"user","content":prompt}],"temperature":0.0});
        if structured {
            body["response_format"] = response_format();
        }
        let mut extra = Vec::new();
        if let Some(key) = &self.api_key {
            extra.push((
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|_| HttpError::Configuration)?,
            ));
        }
        self.transport.post_json(
            &format!("{}/chat/completions", self.base_url),
            json_headers(extra),
            &body.to_string(),
        )
    }
    fn request_legacy(&self, prompt: &str, structured: bool) -> Result<String, HttpError> {
        let mut body = json!({"model":self.model,"messages":[{"role":"user","content":prompt}],"temperature":0.0});
        if structured {
            body["response_format"] = legacy_response_format();
        }
        let mut extra = Vec::new();
        if let Some(key) = &self.api_key {
            extra.push((
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|_| HttpError::Configuration)?,
            ));
        }
        self.transport.post_json(
            &format!("{}/chat/completions", self.base_url),
            json_headers(extra),
            &body.to_string(),
        )
    }
    fn compose_once(
        &self,
        request: &ComposeBatchRequest,
        structured: bool,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        let prompt =
            build_compose_request_prompt(request).map_err(|_| ComposeError::Configuration)?;
        let raw = self.request(&prompt, structured).map_err(map_http)?;
        let envelope: ChatResponse =
            serde_json::from_str(&raw).map_err(|_| ComposeError::InvalidResponse)?;
        let content = envelope
            .choices
            .first()
            .map(|x| x.message.content.as_str())
            .ok_or(ComposeError::EmptyResponse)?;
        let mut output = crate::compose::parse_compose_output(content)?;
        output.metadata = ComposeMetadata {
            adapter: "openai-compatible".into(),
            provider: "openai-compatible".into(),
            model: self.model.clone(),
        };
        Ok(output)
    }
}
impl QuestionComposer for OpenAiCompatibleAdapter {
    fn compose(&self, request: &ComposeBatchRequest) -> Result<ComposeBatchOutput, ComposeError> {
        match self.mode {
            StructuredOutputMode::Off => self.compose_once(request, false),
            StructuredOutputMode::On => self.compose_once(request, true),
            StructuredOutputMode::Auto => {
                if self.state.load(Ordering::Acquire) == UNSUPPORTED {
                    return self.compose_once(request, false);
                }
                let _guard = self.probe.lock().map_err(|_| ComposeError::Transport)?;
                if self.state.load(Ordering::Acquire) == UNSUPPORTED {
                    return self.compose_once(request, false);
                }
                let prompt = build_compose_request_prompt(request)
                    .map_err(|_| ComposeError::Configuration)?;
                match self.request(&prompt, true) {
                    Ok(raw) => {
                        self.state.store(SUPPORTED, Ordering::Release);
                        parse_response(&raw, &self.model)
                    }
                    Err(HttpError::Api {
                        status: 400 | 422,
                        body,
                        ..
                    }) if body.contains("invalid_request_error")
                        && (body.contains("response_format") || body.contains("json_schema")) =>
                    {
                        self.state.store(UNSUPPORTED, Ordering::Release);
                        self.compose_once(request, false)
                    }
                    Err(e) => Err(map_http(e)),
                }
            }
        }
    }
}
fn map_http(e: HttpError) -> ComposeError {
    match e {
        HttpError::Configuration => ComposeError::Configuration,
        HttpError::Authentication { .. } => ComposeError::Authentication,
        HttpError::RateLimited { .. } => ComposeError::RateLimited,
        HttpError::Timeout => ComposeError::Timeout,
        HttpError::Transport => ComposeError::Transport,
        HttpError::Api {
            status, retryable, ..
        } => ComposeError::Api { status, retryable },
    }
}
/// 明示URLは1件、未指定時はOllamaからLM Studioの順で試す。
pub fn local_openai_url_candidates(explicit: Option<&str>) -> Vec<String> {
    match explicit.filter(|x| !x.trim().is_empty()) {
        Some(x) => vec![x.trim_end_matches('/').into()],
        None => vec![
            "http://127.0.0.1:11434/v1".into(),
            "http://127.0.0.1:1234/v1".into(),
        ],
    }
}

/// URL候補を順に試す。接続系以外の失敗は別providerへ隠蔽しない。
pub fn try_local_openai_candidates<T>(
    explicit: Option<&str>,
    mut attempt: impl FnMut(&str) -> Result<T, HttpError>,
) -> Result<T, HttpError> {
    let candidates = local_openai_url_candidates(explicit);
    for (index, url) in candidates.iter().enumerate() {
        match attempt(url) {
            Ok(value) => return Ok(value),
            Err(HttpError::Transport | HttpError::Timeout) if index + 1 < candidates.len() => {}
            Err(error) => return Err(error),
        }
    }
    Err(HttpError::Transport)
}
#[derive(Debug, Clone)]
pub struct LocalOpenAiClient {
    adapter: OpenAiCompatibleAdapter,
}
impl LocalOpenAiClient {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            adapter: OpenAiCompatibleAdapter::new(base_url, model, api_key)
                .with_structured_output(StructuredOutputMode::On),
        }
    }
    pub fn generate_text(&self, prompt: &str) -> Result<String, LocalOpenAiError> {
        self.adapter
            .request_legacy(prompt, true)
            .map_err(LocalOpenAiError::from_http)
            .and_then(extract)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalOpenAiError {
    Http(String),
    Api { status: u16, body: String },
    Response(String),
    EmptyResponse,
}
impl LocalOpenAiError {
    fn from_http(e: HttpError) -> Self {
        match e {
            HttpError::Api { status, body, .. } => Self::Api { status, body },
            other => Self::Http(other.to_string()),
        }
    }
}
impl std::fmt::Display for LocalOpenAiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(_) => write!(f, "Local LLM HTTP error"),
            Self::Api { status, .. } => write!(f, "Local LLM API error: status={status}"),
            Self::Response(_) => write!(f, "Local LLM response parse error"),
            Self::EmptyResponse => write!(f, "Local LLM response was empty"),
        }
    }
}
impl std::error::Error for LocalOpenAiError {}
#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}
#[derive(Deserialize)]
struct Choice {
    message: Message,
}
#[derive(Deserialize)]
struct Message {
    content: String,
}
fn parse_response(body: &str, model: &str) -> Result<ComposeBatchOutput, ComposeError> {
    let e: ChatResponse = serde_json::from_str(body).map_err(|_| ComposeError::InvalidResponse)?;
    let content = e
        .choices
        .first()
        .map(|x| x.message.content.as_str())
        .ok_or(ComposeError::EmptyResponse)?;
    let mut output = crate::compose::parse_compose_output(content)?;
    output.metadata = ComposeMetadata {
        adapter: "openai-compatible".into(),
        provider: "openai-compatible".into(),
        model: model.into(),
    };
    Ok(output)
}
fn extract(body: String) -> Result<String, LocalOpenAiError> {
    serde_json::from_str::<ChatResponse>(&body)
        .map_err(|_| LocalOpenAiError::Response("invalid envelope".into()))?
        .choices
        .into_iter()
        .next()
        .map(|x| x.message.content)
        .filter(|x| !x.trim().is_empty())
        .ok_or(LocalOpenAiError::EmptyResponse)
}
fn response_format() -> Value {
    json!({"type":"json_schema","json_schema":{"name":"flowcloze_compose","strict":true,"schema":{"type":"object","properties":{"items":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"question":{"type":"string"}},"required":["id","question"],"additionalProperties":false}}},"required":["items"],"additionalProperties":false}}})
}
fn legacy_response_format() -> Value {
    json!({"type":"json_schema","json_schema":{"name":"flowcloze_questions","strict":true,"schema":{"type":"object","properties":{"questions":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"question":{"type":"string"}},"required":["id","question"],"additionalProperties":false}}},"required":["questions"],"additionalProperties":false}}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_url_selection_only_falls_through_connection_errors() {
        let mut tried = Vec::new();
        let selected = try_local_openai_candidates(None, |url| {
            tried.push(url.to_string());
            if tried.len() == 1 {
                Err(HttpError::Transport)
            } else {
                Ok(url.to_string())
            }
        })
        .unwrap();
        assert_eq!(tried.len(), 2);
        assert_eq!(selected, "http://127.0.0.1:1234/v1");

        let mut calls = 0;
        let error = try_local_openai_candidates(None, |_| {
            calls += 1;
            Err::<(), _>(HttpError::Authentication { status: 401 })
        })
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(matches!(error, HttpError::Authentication { status: 401 }));
    }
}
