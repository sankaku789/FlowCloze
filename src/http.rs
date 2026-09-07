//! provider adapterが共有する、本文サイズを制限したblocking HTTP transport。

use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RETRY_AFTER};

pub const MAX_RESPONSE_BODY_BYTES: usize = 1024 * 1024;
const MAX_ATTEMPTS: u32 = 3;
const MAX_BACKOFF: Duration = Duration::from_secs(20);

/// retry待機の観測に必要な、本文を含まない値だけを渡す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryDelay {
    pub attempt: u32,
    pub delay_ms: u128,
    pub error_class: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    Configuration,
    Authentication {
        status: u16,
    },
    RateLimited {
        status: u16,
    },
    Timeout,
    Transport,
    Api {
        status: u16,
        retryable: bool,
        body: String,
    },
}

impl HttpError {
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Authentication { status }
            | Self::RateLimited { status }
            | Self::Api { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let class = match self {
            Self::Configuration => "configuration",
            Self::Authentication { .. } => "authentication",
            Self::RateLimited { .. } => "rate-limited",
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::Api {
                retryable: true, ..
            } => "api-retryable",
            Self::Api {
                retryable: false, ..
            } => "api",
        };
        if let Some(status) = self.status() {
            write!(f, "HTTP error: {class} status={status}")
        } else {
            write!(f, "HTTP error: {class}")
        }
    }
}
impl std::error::Error for HttpError {}

#[derive(Clone)]
pub struct HttpTransport {
    client: Client,
    connect_timeout: Duration,
    request_timeout: Duration,
    sleeper: Arc<dyn Fn(Duration) + Send + Sync>,
    retry_observer: Option<Arc<dyn Fn(RetryDelay) + Send + Sync>>,
}

impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTransport").finish_non_exhaustive()
    }
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::new(Duration::from_secs(10), Duration::from_secs(120))
    }
}
impl HttpTransport {
    pub fn new(connect_timeout: Duration, request_timeout: Duration) -> Self {
        let client = Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .build()
            .expect("valid reqwest client configuration");
        Self {
            client,
            connect_timeout,
            request_timeout,
            sleeper: Arc::new(std::thread::sleep),
            retry_observer: None,
        }
    }
    /// テストでは待機を差し替え、retry回数だけを検証できる。
    pub fn with_sleeper(mut self, sleeper: impl Fn(Duration) + Send + Sync + 'static) -> Self {
        self.sleeper = Arc::new(sleeper);
        self
    }
    /// 既存clientはobserver未設定のまま利用できる。
    pub fn with_retry_observer(
        mut self,
        observer: impl Fn(RetryDelay) + Send + Sync + 'static,
    ) -> Self {
        self.retry_observer = Some(Arc::new(observer));
        self
    }
    pub fn post_json(
        &self,
        url: &str,
        headers: HeaderMap,
        body: &str,
    ) -> Result<String, HttpError> {
        if url.trim().is_empty() {
            return Err(HttpError::Configuration);
        }
        for attempt in 1..=MAX_ATTEMPTS {
            let response = self
                .client
                .post(url)
                .headers(headers.clone())
                .body(body.to_owned())
                .send();
            match response {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let retry_after = response
                        .headers()
                        .get(RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(retry_after);
                    let response_body = bounded_body(response)?;
                    if (200..300).contains(&status) {
                        return Ok(response_body);
                    }
                    let retryable = status == 408 || status == 429 || (500..600).contains(&status);
                    if retryable && attempt < MAX_ATTEMPTS {
                        let delay = retry_after.unwrap_or_else(|| backoff(attempt));
                        self.observe_retry(
                            attempt,
                            delay,
                            if status == 429 { "rate_limited" } else { "api" },
                        );
                        (self.sleeper)(delay);
                        continue;
                    }
                    return Err(match status {
                        401 | 403 => HttpError::Authentication { status },
                        429 => HttpError::RateLimited { status },
                        _ => HttpError::Api {
                            status,
                            retryable,
                            body: response_body,
                        },
                    });
                }
                Err(error) => {
                    let timeout = error.is_timeout();
                    let retryable = timeout || error.is_connect();
                    if retryable && attempt < MAX_ATTEMPTS {
                        let delay = backoff(attempt);
                        self.observe_retry(
                            attempt,
                            delay,
                            if timeout { "timeout" } else { "transport" },
                        );
                        (self.sleeper)(delay);
                        continue;
                    }
                    return Err(if timeout {
                        HttpError::Timeout
                    } else {
                        HttpError::Transport
                    });
                }
            }
        }
        Err(HttpError::Transport)
    }
    pub fn timeouts(&self) -> (Duration, Duration) {
        (self.connect_timeout, self.request_timeout)
    }
    fn observe_retry(&self, attempt: u32, delay: Duration, error_class: &'static str) {
        if let Some(observer) = &self.retry_observer {
            observer(RetryDelay {
                attempt,
                delay_ms: delay.as_millis(),
                error_class,
            });
        }
    }
}

fn bounded_body(response: reqwest::blocking::Response) -> Result<String, HttpError> {
    let mut bytes = Vec::with_capacity(4096);
    response
        .take((MAX_RESPONSE_BODY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| HttpError::Transport)?;
    if bytes.len() > MAX_RESPONSE_BODY_BYTES {
        return Err(HttpError::Api {
            status: 0,
            retryable: false,
            body: String::new(),
        });
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(2_u64.saturating_pow(attempt.saturating_sub(1))).min(MAX_BACKOFF)
}
fn retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds).min(MAX_BACKOFF));
    }
    httpdate::parse_http_date(value).ok().map(|when| {
        when.duration_since(SystemTime::now())
            .unwrap_or_default()
            .min(MAX_BACKOFF)
    })
}
pub fn json_headers(extra: impl IntoIterator<Item = (HeaderName, HeaderValue)>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.extend(extra);
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_after_accepts_seconds_and_date() {
        assert_eq!(retry_after("2"), Some(Duration::from_secs(2)));
        assert!(retry_after("Wed, 21 Oct 2015 07:28:00 GMT").is_some());
    }

    #[test]
    fn retry_observer_receives_only_retry_metadata() {
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let capture = Arc::clone(&observed);
        let transport = HttpTransport::default().with_retry_observer(move |event| {
            capture.lock().unwrap().push(event);
        });
        transport.observe_retry(1, Duration::from_secs(2), "api");
        assert_eq!(
            observed.lock().unwrap().as_slice(),
            [RetryDelay {
                attempt: 1,
                delay_ms: 2000,
                error_class: "api"
            }]
        );
    }
}
