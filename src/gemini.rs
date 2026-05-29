//! Gemini APIへプロンプトを送り，文章補完問題JSONを取得する．

use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const MAX_API_ATTEMPTS: u32 = 4;
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(2);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(20);

/// Gemini generateContent APIを呼び出す同期クライアント．
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

    /// プロンプトをGeminiへ送り，候補レスポンスのテキスト部分を取り出す．
    pub fn generate_text(&self, prompt: &str) -> Result<String, GeminiError> {
        let request = GenerateContentRequest {
            contents: vec![Content {
                role: "user",
                parts: vec![Part { text: prompt }],
            }],
            generation_config: GenerationConfig {
                temperature: 0.0,
                response_mime_type: "application/json",
                response_json_schema: generated_document_schema(),
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
        let mut last_error = None;
        for attempt in 1..=MAX_API_ATTEMPTS {
            let response = match client
                .post(&url)
                .header("x-goog-api-key", &self.api_key)
                .json(&request)
                .send()
            {
                Ok(response) => response,
                Err(error) if is_retryable_http_error(&error) && attempt < MAX_API_ATTEMPTS => {
                    last_error = Some(error.to_string());
                    std::thread::sleep(retry_delay(attempt, None));
                    continue;
                }
                Err(error) => return Err(GeminiError::Http(error.to_string())),
            };

            let status = response.status();
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_retry_after);
            let body = response
                .text()
                .map_err(|error| GeminiError::Http(error.to_string()))?;

            if status.is_success() {
                let response = serde_json::from_str::<GenerateContentResponse>(&body)
                    .map_err(|error| GeminiError::Response(error.to_string()))?;
                let text = response
                    .candidates
                    .first()
                    .and_then(|candidate| candidate.content.parts.first())
                    .map(|part| strip_markdown_code_fence(&part.text))
                    .filter(|text| !text.trim().is_empty())
                    .ok_or(GeminiError::EmptyResponse)?;

                return Ok(text);
            }

            if is_retryable_status(status) && attempt < MAX_API_ATTEMPTS {
                last_error = Some(format!("status={}, body={}", status.as_u16(), body));
                std::thread::sleep(retry_delay(attempt, retry_after));
                continue;
            }

            return Err(GeminiError::Api {
                status: status.as_u16(),
                body,
                attempts: attempt,
            });
        }

        Err(GeminiError::Http(last_error.unwrap_or_else(|| {
            "Gemini APIへのリトライに失敗しました".to_string()
        })))
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::BAD_GATEWAY
        || status == StatusCode::SERVICE_UNAVAILABLE
        || status == StatusCode::GATEWAY_TIMEOUT
}

fn is_retryable_http_error(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout()
}

fn retry_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    retry_after
        .map(|duration| duration.min(MAX_RETRY_DELAY))
        .unwrap_or_else(|| {
            INITIAL_RETRY_DELAY
                .saturating_mul(2_u32.saturating_pow(attempt.saturating_sub(1)))
                .min(MAX_RETRY_DELAY)
        })
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .map(|duration| duration.min(MAX_RETRY_DELAY))
}

/// GeminiがJSONをMarkdownコードフェンスで包んだ場合に中身だけ取り出す．
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
    Api {
        status: u16,
        body: String,
        attempts: u32,
    },
    Response(String),
    EmptyResponse,
}

impl std::fmt::Display for GeminiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(message) => write!(f, "Gemini APIへのHTTP通信に失敗しました: {message}"),
            Self::Api {
                status,
                body,
                attempts,
            } => {
                write!(
                    f,
                    "Gemini APIがエラーを返しました: status={status}, attempts={attempts}, body={body}"
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
    response_json_schema: Value,
}

fn generated_document_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "questions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "Input qblock id."
                        },
                        "section": {
                            "type": "string",
                            "description": "Input qblock section heading. Use an empty string if missing."
                        },
                        "type": {
                            "type": "string",
                            "description": "Always context-cloze."
                        },
                        "targets": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "answer": {
                                        "type": "string",
                                        "description": "Exact target answer from the input."
                                    },
                                    "type": {
                                        "type": "string",
                                        "description": "Exact target type from the input."
                                    }
                                },
                                "required": ["answer", "type"]
                            }
                        },
                        "question": {
                            "type": "string",
                            "description": "Context cloze question using ＿＿＿ for every blank."
                        },
                        "answers": {
                            "type": "array",
                            "items": {
                                "type": "string"
                            },
                            "description": "Answers in the same order as blanks in question."
                        },
                        "source_text": {
                            "type": "string",
                            "description": "Source text from the qblock."
                        },
                        "explanation": {
                            "type": "string",
                            "description": "Short explanation. Use an empty string if unnecessary."
                        },
                        "tags": {
                            "type": "array",
                            "items": {
                                "type": "string"
                            }
                        },
                        "warnings": {
                            "type": "array",
                            "items": {
                                "type": "string"
                            }
                        }
                    },
                    "required": [
                        "id",
                        "section",
                        "type",
                        "targets",
                        "question",
                        "answers",
                        "source_text",
                        "explanation",
                        "tags",
                        "warnings"
                    ]
                }
            }
        },
        "required": ["questions"]
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry対象のapiステータスを判定できる() {
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn retry_after秒数を優先して上限で丸める() {
        assert_eq!(
            retry_delay(1, Some(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
        assert_eq!(
            retry_delay(1, Some(Duration::from_secs(100))),
            MAX_RETRY_DELAY
        );
    }

    #[test]
    fn retry_delayは指数バックオフする() {
        assert_eq!(retry_delay(1, None), Duration::from_secs(2));
        assert_eq!(retry_delay(2, None), Duration::from_secs(4));
        assert_eq!(retry_delay(4, None), Duration::from_secs(16));
        assert_eq!(retry_delay(10, None), MAX_RETRY_DELAY);
    }
}
