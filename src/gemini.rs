//! Gemini provider adapterと、互換性を保つ従来client API。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::compose::{
    ComposeBatchOutput, ComposeBatchRequest, ComposeError, ComposeMetadata, QuestionComposer,
};
use crate::http::{json_headers, HttpError, HttpTransport};
use crate::prompt::build_compose_request_prompt;

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const UNKNOWN: u8 = 0;
const SUPPORTED: u8 = 1;
const UNSUPPORTED: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredOutputMode {
    Off,
    On,
    Auto,
}

/// Geminiのprovider envelopeをCompose portへ変換するadapter。
#[derive(Debug)]
pub struct GeminiAdapter {
    api_key: String,
    model: String,
    base_url: String,
    mode: StructuredOutputMode,
    structured_state: AtomicU8,
    probe: Mutex<()>,
    transport: HttpTransport,
}

impl Clone for GeminiAdapter {
    fn clone(&self) -> Self {
        Self {
            api_key: self.api_key.clone(),
            model: self.model.clone(),
            base_url: self.base_url.clone(),
            mode: self.mode,
            structured_state: AtomicU8::new(self.structured_state.load(Ordering::Acquire)),
            probe: Mutex::new(()),
            transport: self.transport.clone(),
        }
    }
}
impl GeminiAdapter {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            base_url: DEFAULT_BASE_URL.into(),
            mode: StructuredOutputMode::Auto,
            structured_state: AtomicU8::new(UNKNOWN),
            probe: Mutex::new(()),
            transport: HttpTransport::default(),
        }
    }
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
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
        let mut config = json!({"temperature": 0.0});
        if structured {
            config["responseMimeType"] = json!("application/json");
            config["responseJsonSchema"] = compose_schema();
        }
        let body = json!({"contents":[{"role":"user","parts":[{"text":prompt}]}],"generationConfig":config}).to_string();
        let key = HeaderValue::from_str(&self.api_key).map_err(|_| HttpError::Configuration)?;
        self.transport.post_json(
            &format!(
                "{}/models/{}:generateContent",
                self.base_url.trim_end_matches('/'),
                self.model
            ),
            json_headers([(HeaderName::from_static("x-goog-api-key"), key)]),
            &body,
        )
    }
    fn request_legacy(&self, prompt: &str, structured: bool) -> Result<String, HttpError> {
        let mut config = json!({"temperature": 0.0});
        if structured {
            config["responseMimeType"] = json!("application/json");
            config["responseJsonSchema"] = legacy_schema();
        }
        let body = json!({"contents":[{"role":"user","parts":[{"text":prompt}]}],"generationConfig":config}).to_string();
        let key = HeaderValue::from_str(&self.api_key).map_err(|_| HttpError::Configuration)?;
        self.transport.post_json(
            &format!(
                "{}/models/{}:generateContent",
                self.base_url.trim_end_matches('/'),
                self.model
            ),
            json_headers([(HeaderName::from_static("x-goog-api-key"), key)]),
            &body,
        )
    }
    fn compose_once(
        &self,
        request: &ComposeBatchRequest,
        structured: bool,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        let prompt =
            build_compose_request_prompt(request).map_err(|_| ComposeError::Configuration)?;
        let raw = self.request(&prompt, structured).map_err(map_http_error)?;
        let response: GeminiResponse =
            serde_json::from_str(&raw).map_err(|_| ComposeError::InvalidResponse)?;
        let content = response
            .candidates
            .first()
            .and_then(|c| c.content.parts.first())
            .map(|part| part.text.as_str())
            .ok_or(ComposeError::EmptyResponse)?;
        let mut output = crate::compose::parse_compose_output(content)?;
        output.metadata = ComposeMetadata {
            adapter: "gemini".into(),
            provider: "gemini".into(),
            model: self.model.clone(),
        };
        Ok(output)
    }
    fn unsupported_schema(error: &HttpError) -> bool {
        matches!(error, HttpError::Api { status: 400 | 422, body, .. } if body.contains("INVALID_ARGUMENT") && (body.contains("responseMimeType") || body.contains("responseJsonSchema") || body.contains("response_mime_type") || body.contains("response_json_schema")))
    }
}
impl QuestionComposer for GeminiAdapter {
    fn compose(&self, request: &ComposeBatchRequest) -> Result<ComposeBatchOutput, ComposeError> {
        match self.mode {
            StructuredOutputMode::Off => self.compose_once(request, false),
            StructuredOutputMode::On => self.compose_once(request, true),
            StructuredOutputMode::Auto => {
                if self.structured_state.load(Ordering::Acquire) == UNSUPPORTED {
                    return self.compose_once(request, false);
                }
                // 最初のschema probeを直列化し、同じ失敗で複数回降格しない。
                let _guard = self.probe.lock().map_err(|_| ComposeError::Transport)?;
                if self.structured_state.load(Ordering::Acquire) == UNSUPPORTED {
                    return self.compose_once(request, false);
                }
                let prompt = build_compose_request_prompt(request)
                    .map_err(|_| ComposeError::Configuration)?;
                match self.request(&prompt, true) {
                    Ok(raw) => {
                        self.structured_state.store(SUPPORTED, Ordering::Release);
                        parse_response(&raw, &self.model)
                    }
                    Err(error) if Self::unsupported_schema(&error) => {
                        self.structured_state.store(UNSUPPORTED, Ordering::Release);
                        self.compose_once(request, false)
                    }
                    Err(error) => Err(map_http_error(error)),
                }
            }
        }
    }
}

fn parse_response(raw: &str, model: &str) -> Result<ComposeBatchOutput, ComposeError> {
    let response: GeminiResponse =
        serde_json::from_str(raw).map_err(|_| ComposeError::InvalidResponse)?;
    let content = response
        .candidates
        .first()
        .and_then(|c| c.content.parts.first())
        .map(|part| part.text.as_str())
        .ok_or(ComposeError::EmptyResponse)?;
    let mut output = crate::compose::parse_compose_output(content)?;
    output.metadata = ComposeMetadata {
        adapter: "gemini".into(),
        provider: "gemini".into(),
        model: model.into(),
    };
    Ok(output)
}

fn map_http_error(error: HttpError) -> ComposeError {
    match error {
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

/// Gemini generateContent APIを呼ぶ従来の同期client。
#[derive(Debug, Clone)]
pub struct GeminiClient {
    adapter: GeminiAdapter,
}
impl GeminiClient {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            adapter: GeminiAdapter::new(api_key, model)
                .with_structured_output(StructuredOutputMode::On),
        }
    }
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.adapter = self.adapter.with_base_url(base_url);
        self
    }
    pub fn generate_text(&self, prompt: &str) -> Result<String, GeminiError> {
        self.adapter
            .request_legacy(prompt, true)
            .map_err(GeminiError::from_http)
            .and_then(extract_text)
    }
}

/// Gemini REST APIへJSONを送る互換低レベルclient。
#[derive(Debug, Clone)]
pub struct GeminiApi {
    api_key: String,
    base_url: String,
    transport: HttpTransport,
}
impl GeminiApi {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.into(),
            transport: HttpTransport::default(),
        }
    }
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
    pub fn post_json<T: Serialize>(&self, path: &str, request: &T) -> Result<String, GeminiError> {
        let key = HeaderValue::from_str(&self.api_key)
            .map_err(|_| GeminiError::Http("configuration".into()))?;
        let body = serde_json::to_string(request)
            .map_err(|_| GeminiError::Response("serialization".into()))?;
        self.transport
            .post_json(
                &self.url(path),
                json_headers([(HeaderName::from_static("x-goog-api-key"), key)]),
                &body,
            )
            .map_err(GeminiError::from_http)
    }
    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiError {
    Http(String),
    Api {
        status: u16,
        body: String,
        attempts: u32,
    },
    Response(String),
    EmptyResponse,
}
impl GeminiError {
    fn from_http(error: HttpError) -> Self {
        match error {
            HttpError::Api { status, body, .. } => Self::Api {
                status,
                body,
                attempts: 3,
            },
            other => Self::Http(other.to_string()),
        }
    }
}
impl std::fmt::Display for GeminiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(_) => write!(f, "Gemini API HTTP error"),
            Self::Api {
                status, attempts, ..
            } => write!(f, "Gemini API error: status={status}, attempts={attempts}"),
            Self::Response(_) => write!(f, "Gemini API response error"),
            Self::EmptyResponse => write!(f, "Gemini API response was empty"),
        }
    }
}
impl std::error::Error for GeminiError {}

fn extract_text(body: String) -> Result<String, GeminiError> {
    let response: GeminiResponse = serde_json::from_str(&body)
        .map_err(|_| GeminiError::Response("invalid envelope".into()))?;
    response
        .candidates
        .first()
        .and_then(|c| c.content.parts.first())
        .map(|p| p.text.clone())
        .filter(|x| !x.trim().is_empty())
        .ok_or(GeminiError::EmptyResponse)
}
pub fn strip_markdown_code_fence(text: &str) -> String {
    crate::compose::extract_json_candidate(text).to_string()
}
fn compose_schema() -> Value {
    json!({"type":"object","properties":{"items":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"question":{"type":"string"}},"required":["id","question"]}}},"required":["items"]})
}
fn legacy_schema() -> Value {
    json!({"type":"object","properties":{"questions":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"question":{"type":"string"}},"required":["id","question"],"additionalProperties":false}}},"required":["questions"],"additionalProperties":false})
}
#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Vec<Candidate>,
}
#[derive(Deserialize)]
struct Candidate {
    content: ResponseContent,
}
#[derive(Deserialize)]
struct ResponseContent {
    parts: Vec<ResponsePart>,
}
#[derive(Deserialize)]
struct ResponsePart {
    text: String,
}
