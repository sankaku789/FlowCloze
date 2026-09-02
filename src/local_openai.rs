//! OpenAI Chat Completions互換provider adapter。

use crate::compose::{
    ComposeBatchOutput, ComposeBatchRequest, ComposeError, ComposeMetadata, QuestionComposer,
};
use crate::http::{json_headers, HttpError, HttpTransport};
use crate::prompt::build_compose_request_prompt;
use crate::providers::capability::{CapabilityProbe, CapabilityState, StructuredOutputMode};
use reqwest::header::{HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug)]
pub struct OpenAiCompatibleAdapter {
    base_url: String,
    model: String,
    api_key: Option<String>,
    mode: StructuredOutputMode,
    capability: CapabilityProbe,
    transport: HttpTransport,
}

impl Clone for OpenAiCompatibleAdapter {
    fn clone(&self) -> Self {
        Self {
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            api_key: self.api_key.clone(),
            mode: self.mode,
            capability: self.capability.clone(),
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
            capability: CapabilityProbe::default(),
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
        let mut body = json!({
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "temperature": 0.0
        });
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

    fn compose_once(
        &self,
        request: &ComposeBatchRequest,
        structured: bool,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        let prompt =
            build_compose_request_prompt(request).map_err(|_| ComposeError::Configuration)?;
        let raw = self.request(&prompt, structured).map_err(map_http)?;
        parse_response(&raw, &self.model)
    }

    fn unsupported_schema(error: &HttpError) -> bool {
        let HttpError::Api {
            status: 400 | 422,
            body,
            ..
        } = error
        else {
            return false;
        };
        let lower = body.to_ascii_lowercase();
        let mentions_schema = lower.contains("response_format") || lower.contains("json_schema");
        let rejects_schema = lower.contains("unsupported")
            || lower.contains("not support")
            || lower.contains("unknown")
            || lower.contains("unrecognized")
            || lower.contains("invalid");
        mentions_schema && rejects_schema
    }
}

impl QuestionComposer for OpenAiCompatibleAdapter {
    fn compose(&self, request: &ComposeBatchRequest) -> Result<ComposeBatchOutput, ComposeError> {
        match self.mode {
            StructuredOutputMode::Off => self.compose_once(request, false),
            StructuredOutputMode::On => self.compose_once(request, true),
            StructuredOutputMode::Auto => {
                if self.capability.state() == CapabilityState::Unsupported {
                    return self.compose_once(request, false);
                }

                let _guard = self
                    .capability
                    .lock()
                    .map_err(|_| ComposeError::Transport)?;

                if self.capability.state() == CapabilityState::Unsupported {
                    return self.compose_once(request, false);
                }

                let prompt = build_compose_request_prompt(request)
                    .map_err(|_| ComposeError::Configuration)?;
                match self.request(&prompt, true) {
                    Ok(raw) => {
                        self.capability.mark_supported();
                        parse_response(&raw, &self.model)
                    }
                    Err(error) if Self::unsupported_schema(&error) => {
                        self.capability.mark_unsupported();
                        self.compose_once(request, false)
                    }
                    Err(error) => Err(map_http(error)),
                }
            }
        }
    }
}

/// OpenAI互換endpoint候補を保持し、接続系失敗時だけ次候補へ切り替えるcomposer。
#[derive(Debug, Clone)]
pub struct OpenAiCompatiblePool {
    adapters: Vec<OpenAiCompatibleAdapter>,
}

impl OpenAiCompatiblePool {
    pub fn new(adapters: Vec<OpenAiCompatibleAdapter>) -> Self {
        Self { adapters }
    }

    pub fn from_candidates(
        explicit: Option<&str>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        let model = model.into();
        let adapters = local_openai_url_candidates(explicit)
            .into_iter()
            .map(|url| OpenAiCompatibleAdapter::new(url, model.clone(), api_key.clone()))
            .collect();
        Self { adapters }
    }

    pub fn with_structured_output(mut self, mode: StructuredOutputMode) -> Self {
        self.adapters = self
            .adapters
            .into_iter()
            .map(|adapter| adapter.with_structured_output(mode))
            .collect();
        self
    }

    pub fn with_transport(mut self, transport: HttpTransport) -> Self {
        self.adapters = self
            .adapters
            .into_iter()
            .map(|adapter| adapter.with_transport(transport.clone()))
            .collect();
        self
    }
}

impl QuestionComposer for OpenAiCompatiblePool {
    fn compose(&self, request: &ComposeBatchRequest) -> Result<ComposeBatchOutput, ComposeError> {
        if self.adapters.is_empty() {
            return Err(ComposeError::Configuration);
        }

        for (index, adapter) in self.adapters.iter().enumerate() {
            match adapter.compose(request) {
                Ok(output) => return Ok(output),
                Err(ComposeError::Transport | ComposeError::Timeout)
                    if index + 1 < self.adapters.len() => {}
                Err(error) => return Err(error),
            }
        }
        Err(ComposeError::Transport)
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
    let envelope: ChatResponse =
        serde_json::from_str(body).map_err(|_| ComposeError::InvalidResponse)?;
    let content = envelope
        .choices
        .first()
        .map(|choice| choice.message.content.as_str())
        .ok_or(ComposeError::EmptyResponse)?;
    let mut output = crate::compose::parse_compose_output(content)?;
    output.metadata = ComposeMetadata {
        adapter: "openai-compatible".into(),
        provider: "openai-compatible".into(),
        model: model.into(),
    };
    Ok(output)
}

fn response_format() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "flowcloze_compose",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "question": {"type": "string"}
                            },
                            "required": ["id", "question"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["items"],
                "additionalProperties": false
            }
        }
    })
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

    #[test]
    fn custom_url_is_the_only_candidate() {
        assert_eq!(
            local_openai_url_candidates(Some("http://localhost:9999/v1/")),
            vec!["http://localhost:9999/v1".to_string()]
        );
    }
}
