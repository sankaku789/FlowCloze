//! Gemini provider adapter。

use reqwest::header::{HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::compose::{
    ComposeBatchOutput, ComposeBatchRequest, ComposeError, ComposeMetadata, QuestionComposer,
};
use crate::http::{json_headers, HttpError, HttpTransport};
use crate::prompt::build_compose_request_prompt;
use crate::providers::capability::{CapabilityProbe, CapabilityState};

pub use crate::providers::capability::StructuredOutputMode;

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Geminiのprovider envelopeをCompose portへ変換するadapter。
#[derive(Debug)]
pub struct GeminiAdapter {
    api_key: String,
    model: String,
    base_url: String,
    mode: StructuredOutputMode,
    capability: CapabilityProbe,
    transport: HttpTransport,
}

impl Clone for GeminiAdapter {
    fn clone(&self) -> Self {
        Self {
            api_key: self.api_key.clone(),
            model: self.model.clone(),
            base_url: self.base_url.clone(),
            mode: self.mode,
            capability: self.capability.clone(),
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
            capability: CapabilityProbe::default(),
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

    fn request(
        &self,
        prompt: &str,
        structured: bool,
        request: &ComposeBatchRequest,
    ) -> Result<String, HttpError> {
        let mut config = json!({"temperature": 0.0});
        if structured {
            config["responseMimeType"] = json!("application/json");
            config["responseJsonSchema"] = compose_schema(request);
        }
        let body = json!({
            "contents": [{"role": "user", "parts": [{"text": prompt}]}],
            "generationConfig": config
        })
        .to_string();
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
        let raw = self
            .request(&prompt, structured, request)
            .map_err(map_http_error)?;
        parse_response(&raw, &self.model)
    }

    fn unsupported_schema(error: &HttpError) -> bool {
        matches!(
            error,
            HttpError::Api {
                status: 400 | 422,
                body,
                ..
            } if body.contains("INVALID_ARGUMENT")
                && (
                    body.contains("responseMimeType")
                        || body.contains("responseJsonSchema")
                        || body.contains("response_mime_type")
                        || body.contains("response_json_schema")
                )
        )
    }
}

impl QuestionComposer for GeminiAdapter {
    fn compose(&self, request: &ComposeBatchRequest) -> Result<ComposeBatchOutput, ComposeError> {
        match self.mode {
            StructuredOutputMode::Off => self.compose_once(request, false),
            StructuredOutputMode::On => self.compose_once(request, true),
            StructuredOutputMode::Auto => {
                if self.capability.state() == CapabilityState::Unsupported {
                    return self.compose_once(request, false);
                }

                // 最初のschema probeを直列化し、同じ失敗で複数回降格しない。
                let _guard = self
                    .capability
                    .lock()
                    .map_err(|_| ComposeError::Transport)?;

                if self.capability.state() == CapabilityState::Unsupported {
                    return self.compose_once(request, false);
                }

                let prompt = build_compose_request_prompt(request)
                    .map_err(|_| ComposeError::Configuration)?;
                match self.request(&prompt, true, request) {
                    Ok(raw) => {
                        self.capability.mark_supported();
                        parse_response(&raw, &self.model)
                    }
                    Err(error) if Self::unsupported_schema(&error) => {
                        self.capability.mark_unsupported();
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
        HttpError::RateLimited { kind, .. } => ComposeError::RateLimited { kind },
        HttpError::Timeout => ComposeError::Timeout,
        HttpError::Transport => ComposeError::Transport,
        HttpError::Api {
            status, retryable, ..
        } => ComposeError::Api { status, retryable },
    }
}

pub fn strip_markdown_code_fence(text: &str) -> String {
    crate::compose::extract_json_candidate(text).to_string()
}

fn compose_schema(request: &ComposeBatchRequest) -> Value {
    let expected_ids = request
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let expected_items = request.tasks.len();
    json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "minItems": expected_items,
                "maxItems": expected_items,
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "enum": expected_ids},
                        "question": {"type": "string"}
                    },
                    "required": ["id", "question"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["items"],
        "additionalProperties": false
    })
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

#[cfg(test)]
mod schema_tests {
    use super::*;
    use crate::compose::{ComposeTask, WritingStyle};

    #[test]
    fn native_schema_constrains_batch_shape() {
        let request = ComposeBatchRequest {
            schema_version: 1,
            batch_id: "batch".into(),
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
        };
        let schema = compose_schema(&request);
        let items = &schema["properties"]["items"];
        assert_eq!(items["minItems"], 1);
        assert_eq!(items["maxItems"], 1);
        assert_eq!(items["items"]["properties"]["id"]["enum"], json!(["q1"]));
    }
}
