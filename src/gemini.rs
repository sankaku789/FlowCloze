//! Gemini APIを使って文章補完問題を生成する処理。

use std::time::Duration;

use serde::{Deserialize, Serialize};

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Gemini APIクライアント。
#[derive(Debug, Clone)]
pub struct GeminiClient {
    api_key: String,
    model: String,
    base_url: String,
}

impl GeminiClient {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// プロンプトをGeminiへ送り，生成されたテキストを取り出す。
    pub fn generate_text(&self, prompt: &str) -> Result<String, GeminiError> {
        let request = GenerateContentRequest {
            contents: vec![Content {
                role: "user",
                parts: vec![Part { text: prompt }],
            }],
            generation_config: GenerationConfig {
                temperature: 0.2,
                response_mime_type: "text/plain",
            },
        };
        let url = format!(
            "{}/models/{}:generateContent",
            self.base_url.trim_end_matches('/'),
            self.model
        );
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|error| GeminiError::Http(error.to_string()))?;
        let response = client
            .post(url)
            .header("x-goog-api-key", &self.api_key)
            .json(&request)
            .send()
            .map_err(|error| GeminiError::Http(error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .map_err(|error| GeminiError::Http(error.to_string()))?;

        if !status.is_success() {
            return Err(GeminiError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let response = serde_json::from_str::<GenerateContentResponse>(&body)
            .map_err(|error| GeminiError::Response(error.to_string()))?;
        let text = response
            .candidates
            .first()
            .and_then(|candidate| candidate.content.parts.first())
            .map(|part| strip_markdown_code_fence(&part.text))
            .filter(|text| !text.trim().is_empty())
            .ok_or(GeminiError::EmptyResponse)?;

        Ok(text)
    }
}

/// GeminiがYAMLをコードフェンスで包んだ場合に中身だけ取り出す。
pub fn strip_markdown_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }

    let mut lines = trimmed.lines();
    let Some(first_line) = lines.next() else {
        return trimmed.to_string();
    };
    if !first_line.trim_start().starts_with("```") {
        return trimmed.to_string();
    }

    let mut body = lines.collect::<Vec<_>>();
    if body
        .last()
        .is_some_and(|line| line.trim_start().starts_with("```"))
    {
        body.pop();
    }
    body.join("\n").trim().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiError {
    Http(String),
    Api { status: u16, body: String },
    Response(String),
    EmptyResponse,
}

impl std::fmt::Display for GeminiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(message) => write!(f, "Gemini APIへのHTTP通信に失敗しました: {message}"),
            Self::Api { status, body } => {
                write!(
                    f,
                    "Gemini APIがエラーを返しました: status={status}, body={body}"
                )
            }
            Self::Response(message) => write!(f, "Gemini APIレスポンスを読めません: {message}"),
            Self::EmptyResponse => write!(f, "Gemini APIレスポンスにテキストがありません"),
        }
    }
}

impl std::error::Error for GeminiError {}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentRequest<'a> {
    contents: Vec<Content<'a>>,
    generation_config: GenerationConfig<'a>,
}

#[derive(Debug, Serialize)]
struct Content<'a> {
    role: &'a str,
    parts: Vec<Part<'a>>,
}

#[derive(Debug, Serialize)]
struct Part<'a> {
    text: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig<'a> {
    temperature: f32,
    response_mime_type: &'a str,
}

#[derive(Debug, Deserialize)]
struct GenerateContentResponse {
    candidates: Vec<Candidate>,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    content: ResponseContent,
}

#[derive(Debug, Deserialize)]
struct ResponseContent {
    parts: Vec<ResponsePart>,
}

#[derive(Debug, Deserialize)]
struct ResponsePart {
    text: String,
}
