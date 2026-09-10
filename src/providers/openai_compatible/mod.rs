mod calibration;
mod local;
mod response;
mod segments;
mod structured;

use std::sync::{Arc, OnceLock};

use crate::compose::{
    ComposeBatchOutput, ComposeBatchRequest, ComposeError, ComposeMetadata, QuestionComposer,
};
use crate::http::{json_headers, HttpError, HttpTransport};
use crate::prompt::build_compose_request_prompt;
use crate::providers::capability::StructuredOutputMode;
use calibration::{
    analyze_segment_calibration_output, build_segment_calibration_request,
    segment_calibration_error_feedback,
};
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::json;

pub use local::{local_openai_url_candidates, try_local_openai_candidates};
use response::{extract_text, ChatResponse};
use segments::{build_segment_compose_request_prompt, parse_segment_compose_output};
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
    legacy_compose: bool,
    segment_calibration: Arc<OnceLock<Vec<String>>>,
}

impl Clone for OpenAiCompatibleAdapter {
    fn clone(&self) -> Self {
        Self {
            endpoint: self.endpoint.clone(),
            mode: self.mode,
            capability: self.capability.clone(),
            transport: self.transport.clone(),
            legacy_compose: self.legacy_compose,
            segment_calibration: self.segment_calibration.clone(),
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
            legacy_compose: false,
            segment_calibration: Arc::new(OnceLock::new()),
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

    /// 旧id/question + <BLANK_n> wire protocolへ戻す。
    /// 既定はsegments protocolで、LLMにはplaceholder文字列を見せない。
    pub fn with_legacy_compose(mut self, legacy: bool) -> Self {
        self.legacy_compose = legacy;
        self
    }

    fn build_prompt(&self, request: &ComposeBatchRequest) -> Result<String, ComposeError> {
        if self.legacy_compose {
            build_compose_request_prompt(request).map_err(|_| ComposeError::Configuration)
        } else {
            build_segment_compose_request_prompt(request).map_err(|_| ComposeError::Configuration)
        }
    }

    fn request(
        &self,
        prompt: &str,
        strategy: StructuredStrategy,
        request: &ComposeBatchRequest,
    ) -> Result<String, HttpError> {
        let mut body = json!({
            "model": self.endpoint.model,
            "messages": [{"role": "user", "content": prompt}]
        });
        if should_send_temperature(&self.endpoint) {
            body["temperature"] = json!(0.0);
        }
        if let Some(format) = response_format(strategy, request, self.legacy_compose) {
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

        /* Temporary request dump for local provider debugging.
        eprintln!("\n===== FLOWCLOZE PROMPT =====");
        eprintln!("{prompt}");
        eprintln!("===== END PROMPT =====\n");
        */

        let raw = self.transport.post_json(
            &format!("{}/chat/completions", self.endpoint.base_url),
            json_headers(extra),
            &body.to_string(),
        )?;

        /* Temporary response dump for local provider debugging.
        eprintln!("\n===== FLOWCLOZE RAW RESPONSE =====");
        eprintln!("{raw}");
        eprintln!("===== END RAW RESPONSE =====\n");
        */

        Ok(raw)
    }

    fn compose_once(
        &self,
        request: &ComposeBatchRequest,
        strategy: StructuredStrategy,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        let prompt = self.build_prompt(request)?;
        let raw = self.request(&prompt, strategy, request).map_err(map_http)?;
        self.parse_response(&raw, request)
    }

    fn parse_response(
        &self,
        body: &str,
        request: &ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        let envelope: ChatResponse =
            serde_json::from_str(body).map_err(|_| ComposeError::InvalidResponse)?;
        let content = extract_text(envelope)?;
        let mut output = if self.legacy_compose {
            crate::compose::parse_compose_output(&content)?
        } else {
            parse_segment_compose_output(&content, request)?
        };
        output.metadata = ComposeMetadata {
            adapter: "openai-compatible".into(),
            provider: self.endpoint.provider_label.clone(),
            model: self.endpoint.model.clone(),
        };
        Ok(output)
    }

    fn compose_request(
        &self,
        request: &ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        match self.mode {
            StructuredOutputMode::Off => self.compose_once(request, StructuredStrategy::PromptOnly),
            StructuredOutputMode::On => {
                let prompt = self.build_prompt(request)?;
                match self.request(&prompt, StructuredStrategy::JsonSchema, request) {
                    Ok(raw) => self.parse_response(&raw, request),
                    Err(error) if unsupported_response_format(&error) => {
                        match self.request(&prompt, StructuredStrategy::JsonObject, request) {
                            Ok(raw) => self.parse_response(&raw, request),
                            Err(error) => Err(map_http(error)),
                        }
                    }
                    Err(error) => Err(map_http(error)),
                }
            }
            StructuredOutputMode::Auto => {
                let prompt = self.build_prompt(request)?;
                if let Some(strategy) = self.capability.strategy() {
                    return match self.request(&prompt, strategy, request) {
                        Ok(raw) => self.parse_response(&raw, request),
                        Err(error) => Err(map_http(error)),
                    };
                }

                let _guard = self
                    .capability
                    .lock()
                    .map_err(|_| ComposeError::Transport)?;
                if let Some(strategy) = self.capability.strategy() {
                    return match self.request(&prompt, strategy, request) {
                        Ok(raw) => self.parse_response(&raw, request),
                        Err(error) => Err(map_http(error)),
                    };
                }

                for strategy in [
                    StructuredStrategy::JsonSchema,
                    StructuredStrategy::JsonObject,
                    StructuredStrategy::PromptOnly,
                ] {
                    match self.request(&prompt, strategy, request) {
                        Ok(raw) => {
                            let output = self.parse_response(&raw, request)?;
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

    fn segment_calibration_feedback(&self) -> Vec<String> {
        if self.legacy_compose {
            return Vec::new();
        }
        self.segment_calibration
            .get_or_init(|| {
                let request = build_segment_calibration_request();
                match self.compose_request(&request) {
                    Ok(output) => analyze_segment_calibration_output(&output),
                    Err(error) => segment_calibration_error_feedback(&error),
                }
            })
            .clone()
    }
}

impl QuestionComposer for OpenAiCompatibleAdapter {
    fn compose(&self, request: &ComposeBatchRequest) -> Result<ComposeBatchOutput, ComposeError> {
        if self.legacy_compose {
            return self.compose_request(request);
        }

        let calibration_feedback = self.segment_calibration_feedback();
        if calibration_feedback.is_empty() {
            return self.compose_request(request);
        }

        let mut calibrated_request = request.clone();
        calibrated_request.retry_feedback.extend(calibration_feedback);
        self.compose_request(&calibrated_request)
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

    pub fn with_legacy_compose(mut self, legacy: bool) -> Self {
        self.adapters = self
            .adapters
            .into_iter()
            .map(|adapter| adapter.with_legacy_compose(legacy))
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

fn should_send_temperature(endpoint: &OpenAiEndpointConfig) -> bool {
    !(endpoint.provider_label == "gemini" && endpoint.model.starts_with("gemini-3"))
}

fn map_http(error: HttpError) -> ComposeError {
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

#[cfg(test)]
mod request_tests {
    use super::*;

    #[test]
    fn segment_compose_is_default_and_legacy_is_opt_in() {
        let adapter = OpenAiCompatibleAdapter::new("https://example.invalid/v1", "model", None);
        assert!(!adapter.legacy_compose);
        assert!(adapter.with_legacy_compose(true).legacy_compose);
    }

    #[test]
    fn gemini_3_omits_deprecated_sampling_parameters() {
        let endpoint = OpenAiEndpointConfig::new("https://example.invalid/v1", "gemini-3.8-flash")
            .with_provider_label("gemini");
        assert!(!should_send_temperature(&endpoint));
    }

    #[test]
    fn gemini_25_and_other_openai_compatible_models_keep_temperature() {
        let gemini = OpenAiEndpointConfig::new("https://example.invalid/v1", "gemini-2.5-flash")
            .with_provider_label("gemini");
        let other = OpenAiEndpointConfig::new("https://example.invalid/v1", "mistral-small-latest")
            .with_provider_label("openai-compatible");
        assert!(should_send_temperature(&gemini));
        assert!(should_send_temperature(&other));
    }
}
