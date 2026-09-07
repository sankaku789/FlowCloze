mod local;
mod response;
mod structured;

use crate::compose::{
    ComposeBatchOutput, ComposeBatchRequest, ComposeError, ComposeMetadata, QuestionComposer,
};
use crate::http::{json_headers, HttpError, HttpTransport};
use crate::prompt::build_compose_request_prompt;
use crate::providers::capability::StructuredOutputMode;
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::json;

pub use local::{local_openai_url_candidates, try_local_openai_candidates};
use response::{extract_text, ChatResponse};
use structured::{
    response_format, unsupported_response_format, StructuredCapabilityProbe, StructuredStrategy,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAiAuth {
    None,
    Bearer(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiEndpointConfig {
    pub base_url: String,
    pub model: String,
    pub auth: OpenAiAuth,
    pub provider_label: String,
}

impl OpenAiEndpointConfig {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            auth: OpenAiAuth::None,
            provider_label: "openai-compatible".into(),
        }
    }

    pub fn with_bearer(mut self, api_key: impl Into<String>) -> Self {
        self.auth = OpenAiAuth::Bearer(api_key.into());
        self
    }

    pub fn with_provider_label(mut self, label: impl Into<String>) -> Self {
        self.provider_label = label.into();
        self
    }
}

#[derive(Debug)]
pub struct OpenAiCompatibleAdapter {
    endpoint: OpenAiEndpointConfig,
    mode: StructuredOutputMode,
    capability: StructuredCapabilityProbe,
    transport: HttpTransport,
}

impl Clone for OpenAiCompatibleAdapter {
    fn clone(&self) -> Self {
        Self {
            endpoint: self.endpoint.clone(),
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
        let mut endpoint = OpenAiEndpointConfig::new(base_url, model);
        if let Some(key) = api_key.filter(|value| !value.trim().is_empty()) {
            endpoint = endpoint.with_bearer(key);
        }
        Self::from_endpoint(endpoint)
    }

    pub fn from_endpoint(mut endpoint: OpenAiEndpointConfig) -> Self {
        endpoint.base_url = endpoint.base_url.trim_end_matches('/').to_string();
        Self {
            endpoint,
            mode: StructuredOutputMode::Auto,
            capability: StructuredCapabilityProbe::default(),
            transport: HttpTransport::default(),
        }
    }

    pub fn with_provider_label(mut self, label: impl Into<String>) -> Self {
        self.endpoint.provider_label = label.into();
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

    fn request(&self, prompt: &str, strategy: StructuredStrategy) -> Result<String, HttpError> {
        let mut body = json!({
            "model": self.endpoint.model,
            "messages": [{"role": "user", "content": prompt}],
            "temperature": 0.0
        });
        if let Some(format) = response_format(strategy) {
            body["response_format"] = format;
        }

        let mut extra = Vec::new();
        if let OpenAiAuth::Bearer(key) = &self.endpoint.auth {
            extra.push((
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|_| HttpError::Configuration)?,
            ));
        }

        self.transport.post_json(
            &format!("{}/chat/completions", self.endpoint.base_url),
            json_headers(extra),
            &body.to_string(),
        )
    }

    fn compose_once(
        &self,
        request: &ComposeBatchRequest,
        strategy: StructuredStrategy,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        let prompt =
            build_compose_request_prompt(request).map_err(|_| ComposeError::Configuration)?;
        let raw = self.request(&prompt, strategy).map_err(map_http)?;
        self.parse_response(&raw)
    }

    fn parse_response(&self, body: &str) -> Result<ComposeBatchOutput, ComposeError> {
        let envelope: ChatResponse =
            serde_json::from_str(body).map_err(|_| ComposeError::InvalidResponse)?;
        let content = extract_text(envelope)?;
        let mut output = crate::compose::parse_compose_output(&content)?;
        output.metadata = ComposeMetadata {
            adapter: "openai-compatible".into(),
            provider: self.endpoint.provider_label.clone(),
            model: self.endpoint.model.clone(),
        };
        Ok(output)
    }
}

impl QuestionComposer for OpenAiCompatibleAdapter {
    fn compose(&self, request: &ComposeBatchRequest) -> Result<ComposeBatchOutput, ComposeError> {
        match self.mode {
            StructuredOutputMode::Off => self.compose_once(request, StructuredStrategy::PromptOnly),
            StructuredOutputMode::On => {
                let prompt = build_compose_request_prompt(request)
                    .map_err(|_| ComposeError::Configuration)?;
                match self.request(&prompt, StructuredStrategy::JsonSchema) {
                    Ok(raw) => self.parse_response(&raw),
                    Err(error) if unsupported_response_format(&error) => {
                        match self.request(&prompt, StructuredStrategy::JsonObject) {
                            Ok(raw) => self.parse_response(&raw),
                            Err(error) => Err(map_http(error)),
                        }
                    }
                    Err(error) => Err(map_http(error)),
                }
            }
            StructuredOutputMode::Auto => {
                let prompt = build_compose_request_prompt(request)
                    .map_err(|_| ComposeError::Configuration)?;
                if let Some(strategy) = self.capability.strategy() {
                    return match self.request(&prompt, strategy) {
                        Ok(raw) => self.parse_response(&raw),
                        Err(error) => Err(map_http(error)),
                    };
                }

                let _guard = self
                    .capability
                    .lock()
                    .map_err(|_| ComposeError::Transport)?;
                if let Some(strategy) = self.capability.strategy() {
                    return match self.request(&prompt, strategy) {
                        Ok(raw) => self.parse_response(&raw),
                        Err(error) => Err(map_http(error)),
                    };
                }

                for strategy in [
                    StructuredStrategy::JsonSchema,
                    StructuredStrategy::JsonObject,
                    StructuredStrategy::PromptOnly,
                ] {
                    match self.request(&prompt, strategy) {
                        Ok(raw) => {
                            let output = self.parse_response(&raw)?;
                            self.capability.mark(strategy);
                            return Ok(output);
                        }
                        Err(error)
                            if strategy != StructuredStrategy::PromptOnly
                                && unsupported_response_format(&error) => {}
                        Err(error) => return Err(map_http(error)),
                    }
                }
                Err(ComposeError::InvalidResponse)
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

fn map_http(error: HttpError) -> ComposeError {
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
