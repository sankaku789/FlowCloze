//! OpenAI互換APIを使うローカルLLMクライアント．

use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};

/// Ollama / llama.cpp server / vLLMなどのOpenAI互換APIを呼び出す同期クライアント．
#[derive(Debug, Clone)]
pub struct LocalOpenAiClient {
    base_url: String,
    model: String,
    api_key: Option<String>,
}

impl LocalOpenAiClient {
    /// base URL，model，任意のAPI keyからクライアントを作る．
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            api_key: api_key.filter(|key| !key.trim().is_empty()),
        }
    }

    /// promptをOpenAI互換chat completionsへ送り，message.contentを取り出す．
    pub fn generate_text(&self, prompt: &str) -> Result<String, LocalOpenAiError> {
        let request = json!({
            "model": self.model,
            "messages": [
                {
                    "role": "user",
                    "content": prompt
                }
            ],
            "temperature": 0.0,
            "response_format": composed_document_response_format()
        });
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|error| LocalOpenAiError::Http(error.to_string()))?;
        let mut builder = client
            .post(format!("{}/chat/completions", self.base_url))
            .header("content-type", "application/json")
            .json(&request);

        if let Some(api_key) = &self.api_key {
            builder = builder.bearer_auth(api_key);
        }

        let response = builder
            .send()
            .map_err(|error| LocalOpenAiError::Http(error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .map_err(|error| LocalOpenAiError::Http(error.to_string()))?;

        if !status.is_success() {
            return Err(LocalOpenAiError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: ChatCompletionResponse = serde_json::from_str(&body)
            .map_err(|error| LocalOpenAiError::Response(error.to_string()))?;
        parsed
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .filter(|content| !content.trim().is_empty())
            .ok_or(LocalOpenAiError::EmptyResponse)
    }
}

/// local backendで発生しうるエラー．
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalOpenAiError {
    Http(String),
    Api { status: u16, body: String },
    Response(String),
    EmptyResponse,
}

impl std::fmt::Display for LocalOpenAiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(message) => write!(f, "Local LLM HTTP error: {message}"),
            Self::Api { status, body } => {
                let label = StatusCode::from_u16(*status)
                    .ok()
                    .and_then(|status| status.canonical_reason().map(str::to_string))
                    .unwrap_or_else(|| "unknown status".to_string());
                write!(
                    f,
                    "Local LLM API error: status={status} ({label}), body={body}"
                )
            }
            Self::Response(message) => write!(f, "Local LLM response parse error: {message}"),
            Self::EmptyResponse => write!(f, "Local LLM response was empty"),
        }
    }
}

impl std::error::Error for LocalOpenAiError {}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionMessage,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionMessage {
    content: String,
}

/// OpenAI互換APIのresponse_formatでid/questionだけのJSON Schemaを指定する．
fn composed_document_response_format() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "flowcloze_composed_document",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "question": { "type": "string" }
                            },
                            "required": ["id", "question"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["questions"],
                "additionalProperties": false
            }
        }
    })
}
