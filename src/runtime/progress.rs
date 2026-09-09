//! 人間が読む進捗表示。JSON Lines の観測イベントとは独立している。

use std::io::{self, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::rate_limit::RateLimitKind;

/// 失敗した処理段階。表示・機械判定の双方で安定した有限集合にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressStage {
    Config,
    Read,
    Parse,
    Plan,
    Generate,
    Validate,
    Serialize,
    Save,
    Output,
}

impl ProgressStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Read => "read",
            Self::Parse => "parse",
            Self::Plan => "plan",
            Self::Generate => "generate",
            Self::Validate => "validate",
            Self::Serialize => "serialize",
            Self::Save => "save",
            Self::Output => "output",
        }
    }
}

/// 本文や生のエラーを含まない失敗分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    Configuration,
    Authentication,
    RateLimited,
    Timeout,
    Transport,
    Api,
    Content,
    Validation,
    InvalidInput,
    Serialization,
    Io,
}

impl FailureClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Authentication => "authentication",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::Api => "api",
            Self::Content => "content",
            Self::Validation => "validation",
            Self::InvalidInput => "invalid_input",
            Self::Serialization => "serialization",
            Self::Io => "io",
        }
    }

    const fn detail(self) -> &'static str {
        match self {
            Self::Configuration => "invalid configuration",
            Self::Authentication => "authentication failed",
            Self::RateLimited => "provider rate limit reached",
            Self::Timeout => "provider request timed out",
            Self::Transport => "provider connection failed",
            Self::Api => "provider API request failed",
            Self::Content => "provider output was invalid or failed content checks",
            Self::Validation => "generated content failed validation",
            Self::InvalidInput => "input could not be parsed or validated",
            Self::Serialization => "output serialization failed",
            Self::Io => "file or stream I/O failed",
        }
    }
}

/// progress sink に渡す、本文を持たないイベント。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    Parsed {
        tasks: usize,
    },
    Planned {
        initial_batches: usize,
        provider_tasks: usize,
        identity_tasks: usize,
    },
    BatchComplete {
        number: usize,
        total: usize,
        successes: usize,
        retries: usize,
    },
    Retry {
        task_id: String,
        attempt: u32,
        cause: RetryCause,
        result: RetryResult,
    },
    Fallback {
        task_id: String,
        reason: FailureClass,
    },
    Validated {
        tasks: usize,
    },
    Saved {
        path: String,
    },
    Stdout,
    ProviderError {
        class: FailureClass,
        status: Option<u16>,
        rate_limit: Option<RateLimitKind>,
    },
    Failed {
        stage: ProgressStage,
        class: FailureClass,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryCause {
    InvalidProviderContent,
    IdMismatch,
    MissingId,
    EmptyQuestion,
    BlankCountMismatch,
    AnswerNotInTargets,
    AnswerLeakage,
    MissingTargetAnswer,
    FixedFieldMismatch,
    DuplicateId,
    UnknownId,
    OrderMismatch,
    AnonymousBlank,
    MissingSentinel,
    DuplicateSentinel,
    SentinelOrder,
    MalformedSentinel,
    UnknownSentinel,
    ForeignSentinel,
    ContentValidation,
}

impl RetryCause {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidProviderContent => "invalid_provider_content",
            Self::IdMismatch => "id_mismatch",
            Self::MissingId => "missing_id",
            Self::EmptyQuestion => "empty_question",
            Self::BlankCountMismatch => "blank_count_mismatch",
            Self::AnswerNotInTargets => "answer_not_in_targets",
            Self::AnswerLeakage => "answer_leakage",
            Self::MissingTargetAnswer => "missing_target_answer",
            Self::FixedFieldMismatch => "fixed_field_mismatch",
            Self::DuplicateId => "duplicate_id",
            Self::UnknownId => "unknown_id",
            Self::OrderMismatch => "order_mismatch",
            Self::AnonymousBlank => "anonymous_blank",
            Self::MissingSentinel => "missing_sentinel",
            Self::DuplicateSentinel => "duplicate_sentinel",
            Self::SentinelOrder => "sentinel_order",
            Self::MalformedSentinel => "malformed_sentinel",
            Self::UnknownSentinel => "unknown_sentinel",
            Self::ForeignSentinel => "foreign_sentinel",
            Self::ContentValidation => "content_validation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryResult {
    Success,
    Retry,
    Failed,
}
impl RetryResult {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Retry => "retry",
            Self::Failed => "failed",
        }
    }
}

pub trait ProgressSink: Send + Sync {
    fn emit(&self, event: ProgressEvent);
}

#[derive(Debug, Default)]
pub struct NoopProgressSink;
impl ProgressSink for NoopProgressSink {
    fn emit(&self, _: ProgressEvent) {}
}

const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeartbeatPhase {
    Batch { number: usize, total: usize },
    Retry { pending: usize },
}

enum HeartbeatCommand {
    Set(HeartbeatPhase),
    Pause,
    Stop,
}

struct HeartbeatWorker {
    tx: mpsc::Sender<HeartbeatCommand>,
    handle: Option<JoinHandle<()>>,
}

fn format_heartbeat(phase: HeartbeatPhase, elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match phase {
        HeartbeatPhase::Batch { number, total } => {
            format!("      batch {number}/{total}: provider待機中... {seconds}s")
        }
        HeartbeatPhase::Retry { pending } => {
            format!("      retry: provider待機中... {seconds}s ({pending} tasks pending)")
        }
    }
}

fn write_progress_line(writer: &Arc<Mutex<Box<dyn Write + Send>>>, line: &str) {
    if let Ok(mut writer) = writer.lock() {
        let _ = writeln!(writer, "{line}");
        let _ = writer.flush();
    }
}

fn heartbeat_loop(
    rx: mpsc::Receiver<HeartbeatCommand>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    interval: Duration,
) {
    let mut state: Option<(HeartbeatPhase, Instant)> = None;
    loop {
        if state.is_none() {
            match rx.recv() {
                Ok(HeartbeatCommand::Set(phase)) => state = Some((phase, Instant::now())),
                Ok(HeartbeatCommand::Pause) => {}
                Ok(HeartbeatCommand::Stop) | Err(_) => break,
            }
            continue;
        }

        match rx.recv_timeout(interval) {
            Ok(HeartbeatCommand::Set(phase)) => state = Some((phase, Instant::now())),
            Ok(HeartbeatCommand::Pause) => state = None,
            Ok(HeartbeatCommand::Stop) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some((phase, started)) = &state {
                    write_progress_line(&writer, &format_heartbeat(*phase, started.elapsed()));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// stderr 等へ固定書式で書く sink。識別子とパスは制御文字を可視化して一行性を守る。
pub struct PlainProgressSink {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    label: Mutex<String>,
    heartbeat: Mutex<Option<HeartbeatWorker>>,
    pending_retries: Mutex<usize>,
    heartbeat_interval: Duration,
}
impl PlainProgressSink {
    pub fn stderr(label: impl Into<String>) -> Self {
        Self::with_writer(io::stderr(), label)
    }

    pub fn new(writer: impl Write + Send + 'static, label: impl Into<String>) -> Self {
        Self::with_writer(writer, label)
    }

    pub fn with_writer(writer: impl Write + Send + 'static, label: impl Into<String>) -> Self {
        Self::with_writer_and_interval(writer, label, DEFAULT_HEARTBEAT_INTERVAL)
    }

    fn with_writer_and_interval(
        writer: impl Write + Send + 'static,
        label: impl Into<String>,
        heartbeat_interval: Duration,
    ) -> Self {
        Self {
            writer: Arc::new(Mutex::new(Box::new(writer))),
            label: Mutex::new(escape(&label.into())),
            heartbeat: Mutex::new(None),
            pending_retries: Mutex::new(0),
            heartbeat_interval,
        }
    }

    /// 設定解決後の実行modeを、生成開始イベントより前に反映する。
    pub fn set_label(&self, label: impl Into<String>) {
        if let Ok(mut current) = self.label.lock() {
            *current = escape(&label.into());
        }
    }

    fn send_heartbeat(&self, command: HeartbeatCommand) {
        let Ok(mut slot) = self.heartbeat.lock() else {
            return;
        };

        let needs_worker = matches!(&command, HeartbeatCommand::Set(_)) && slot.is_none();
        if needs_worker {
            let (tx, rx) = mpsc::channel();
            let writer = Arc::clone(&self.writer);
            let interval = self.heartbeat_interval;
            let handle = thread::spawn(move || heartbeat_loop(rx, writer, interval));
            *slot = Some(HeartbeatWorker {
                tx,
                handle: Some(handle),
            });
        }

        if let Some(worker) = slot.as_ref() {
            let _ = worker.tx.send(command);
        }
    }

    fn update_heartbeat(&self, event: &ProgressEvent) {
        let command = match event {
            ProgressEvent::Parsed { .. } => {
                if let Ok(mut pending) = self.pending_retries.lock() {
                    *pending = 0;
                }
                Some(HeartbeatCommand::Pause)
            }
            ProgressEvent::Planned {
                initial_batches, ..
            } if *initial_batches > 0 => {
                if let Ok(mut pending) = self.pending_retries.lock() {
                    *pending = 0;
                }
                Some(HeartbeatCommand::Set(HeartbeatPhase::Batch {
                    number: 1,
                    total: *initial_batches,
                }))
            }
            ProgressEvent::BatchComplete {
                number,
                total,
                retries,
                ..
            } => {
                let pending = if let Ok(mut pending) = self.pending_retries.lock() {
                    *pending = pending.saturating_add(*retries);
                    *pending
                } else {
                    0
                };
                if number < total {
                    Some(HeartbeatCommand::Set(HeartbeatPhase::Batch {
                        number: number + 1,
                        total: *total,
                    }))
                } else if pending > 0 {
                    Some(HeartbeatCommand::Set(HeartbeatPhase::Retry { pending }))
                } else {
                    Some(HeartbeatCommand::Pause)
                }
            }
            ProgressEvent::Retry { result, .. } => {
                let pending = if let Ok(mut pending) = self.pending_retries.lock() {
                    if matches!(result, RetryResult::Success | RetryResult::Failed) {
                        *pending = pending.saturating_sub(1);
                    }
                    *pending
                } else {
                    0
                };
                if pending > 0 {
                    Some(HeartbeatCommand::Set(HeartbeatPhase::Retry { pending }))
                } else {
                    Some(HeartbeatCommand::Pause)
                }
            }
            ProgressEvent::Validated { .. }
            | ProgressEvent::Saved { .. }
            | ProgressEvent::Stdout
            | ProgressEvent::ProviderError { .. }
            | ProgressEvent::Failed { .. } => Some(HeartbeatCommand::Pause),
            ProgressEvent::Fallback { .. } => None,
            ProgressEvent::Planned { .. } => Some(HeartbeatCommand::Pause),
        };

        if let Some(command) = command {
            self.send_heartbeat(command);
        }
    }
}

impl Drop for PlainProgressSink {
    fn drop(&mut self) {
        let Ok(mut slot) = self.heartbeat.lock() else {
            return;
        };
        let Some(mut worker) = slot.take() else {
            return;
        };
        let _ = worker.tx.send(HeartbeatCommand::Stop);
        if let Some(handle) = worker.handle.take() {
            let _ = handle.join();
        }
    }
}

impl ProgressSink for PlainProgressSink {
    fn emit(&self, event: ProgressEvent) {
        let heartbeat_event = event.clone();
        let line = match event {
            ProgressEvent::Parsed { tasks } => format!("[1/4] Markdown解析: {tasks} tasks"),
            ProgressEvent::Planned {
                initial_batches,
                provider_tasks,
                identity_tasks,
            } => {
                let label = self
                    .label
                    .lock()
                    .map(|label| label.clone())
                    .unwrap_or_else(|_| "Generate".to_string());
                let counts = match (provider_tasks, identity_tasks) {
                    (0, _) => String::new(),
                    (_, 0) => String::new(),
                    _ => format!(" (provider: {provider_tasks}, identity: {identity_tasks})"),
                };
                format!(
                    "[2/4] 生成開始: {} / {initial_batches} batches{counts}",
                    label
                )
            }
            ProgressEvent::BatchComplete {
                number,
                total,
                successes,
                retries,
            } => format!("      batch {number}/{total}: {successes}成功, {retries} retry"),
            ProgressEvent::Retry {
                task_id,
                attempt,
                cause,
                result,
            } => format!(
                "      retry: task={} attempt={attempt} cause={} result={}",
                escape(&task_id),
                cause.as_str(),
                result.as_str()
            ),
            ProgressEvent::Fallback { task_id, reason } => format!(
                "      fallback: task={} reason={} detail={}",
                escape(&task_id),
                reason.as_str(),
                reason.detail()
            ),
            ProgressEvent::Validated { tasks } => format!("[3/4] 検証完了: {tasks}/{tasks}"),
            ProgressEvent::Saved { path } => format!("[4/4] 保存完了: {}", escape(&path)),
            ProgressEvent::Stdout => "[4/4] stdout出力完了".to_string(),
            ProgressEvent::ProviderError {
                class,
                status,
                rate_limit,
            } => format_provider_error(class, status, rate_limit),
            ProgressEvent::Failed { stage, class } => format!(
                "[failed] stage={} class={} detail={}",
                stage.as_str(),
                class.as_str(),
                class.detail()
            ),
        };
        write_progress_line(&self.writer, &line);
        self.update_heartbeat(&heartbeat_event);
    }
}

fn format_provider_error(
    class: FailureClass,
    status: Option<u16>,
    rate_limit: Option<RateLimitKind>,
) -> String {
    let quota = rate_limit
        .filter(|kind| *kind != RateLimitKind::Unknown)
        .map(|kind| format!(", quota={}", kind.as_str()))
        .unwrap_or_default();

    if let Some(status) = status {
        let details = format!(
            "class={}, detail={}{}",
            class.as_str(),
            class.detail(),
            quota
        );
        if let Some(reason) = http_status_reason(status) {
            return format!("Provider error: HTTP {status} {reason} ({details})");
        }
        return format!("Provider error: HTTP {status} ({details})");
    }

    format!(
        "Provider error: {} (class={}{})",
        class.detail(),
        class.as_str(),
        quota
    )
}

fn http_status_reason(status: u16) -> Option<&'static str> {
    match status {
        400 => Some("Bad Request"),
        401 => Some("Unauthorized"),
        403 => Some("Forbidden"),
        404 => Some("Not Found"),
        408 => Some("Request Timeout"),
        429 => Some("Too Many Requests"),
        500 => Some("Internal Server Error"),
        502 => Some("Bad Gateway"),
        503 => Some("Service Unavailable"),
        504 => Some("Gateway Timeout"),
        _ => None,
    }
}

fn escape(value: &str) -> String {
    value.escape_debug().to_string()
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn plain_identity_output_is_stable_and_escapes_paths() {
        let writer = SharedWriter::default();
        let sink = PlainProgressSink::with_writer(writer.clone(), "Identity");
        sink.emit(ProgressEvent::Parsed { tasks: 1 });
        sink.emit(ProgressEvent::Planned {
            initial_batches: 1,
            provider_tasks: 0,
            identity_tasks: 1,
        });
        sink.emit(ProgressEvent::BatchComplete {
            number: 1,
            total: 1,
            successes: 1,
            retries: 0,
        });
        sink.emit(ProgressEvent::Validated { tasks: 1 });
        sink.emit(ProgressEvent::Saved {
            path: "a\nb.json".to_string(),
        });
        assert_eq!(String::from_utf8(writer.0.lock().unwrap().clone()).unwrap(), "[1/4] Markdown解析: 1 tasks\n[2/4] 生成開始: Identity / 1 batches\n      batch 1/1: 1成功, 0 retry\n[3/4] 検証完了: 1/1\n[4/4] 保存完了: a\\nb.json\n");
    }

    #[test]
    fn retry_output_explains_content_validation_cause() {
        let writer = SharedWriter::default();
        let sink = PlainProgressSink::with_writer(writer.clone(), "Gemini");
        sink.emit(ProgressEvent::BatchComplete {
            number: 1,
            total: 2,
            successes: 2,
            retries: 1,
        });
        sink.emit(ProgressEvent::Retry {
            task_id: "qblock-003".to_string(),
            attempt: 1,
            cause: RetryCause::AnswerLeakage,
            result: RetryResult::Success,
        });

        assert_eq!(
            String::from_utf8(writer.0.lock().unwrap().clone()).unwrap(),
            "      batch 1/2: 2成功, 1 retry\n      retry: task=qblock-003 attempt=1 cause=answer_leakage result=success\n"
        );
    }

    #[test]
    fn provider_error_renders_http_status_and_reason() {
        let writer = SharedWriter::default();
        let sink = PlainProgressSink::with_writer(writer.clone(), "Gemini");
        sink.emit(ProgressEvent::ProviderError {
            class: FailureClass::RateLimited,
            status: Some(429),
            rate_limit: Some(RateLimitKind::RequestsPerDay),
        });

        assert_eq!(
            String::from_utf8(writer.0.lock().unwrap().clone()).unwrap(),
            "Provider error: HTTP 429 Too Many Requests (class=rate_limited, detail=provider rate limit reached, quota=rpd)\n"
        );
    }

    #[test]
    fn provider_error_without_status_is_still_human_readable() {
        let writer = SharedWriter::default();
        let sink = PlainProgressSink::with_writer(writer.clone(), "Gemini");
        sink.emit(ProgressEvent::ProviderError {
            class: FailureClass::Timeout,
            status: None,
            rate_limit: None,
        });

        assert_eq!(
            String::from_utf8(writer.0.lock().unwrap().clone()).unwrap(),
            "Provider error: provider request timed out (class=timeout)\n"
        );
    }

    #[test]
    fn failed_output_includes_sanitized_detail() {
        let writer = SharedWriter::default();
        let sink = PlainProgressSink::with_writer(writer.clone(), "Gemini");
        sink.emit(ProgressEvent::Failed {
            stage: ProgressStage::Generate,
            class: FailureClass::Content,
        });

        assert_eq!(
            String::from_utf8(writer.0.lock().unwrap().clone()).unwrap(),
            "[failed] stage=generate class=content detail=provider output was invalid or failed content checks\n"
        );
    }

    #[test]
    fn heartbeat_lines_report_elapsed_provider_wait() {
        assert_eq!(
            format_heartbeat(
                HeartbeatPhase::Batch {
                    number: 2,
                    total: 4,
                },
                Duration::from_secs(15),
            ),
            "      batch 2/4: provider待機中... 15s"
        );
        assert_eq!(
            format_heartbeat(
                HeartbeatPhase::Retry { pending: 2 },
                Duration::from_secs(10),
            ),
            "      retry: provider待機中... 10s (2 tasks pending)"
        );
    }

    #[test]
    fn noop_never_writes_or_panics() {
        NoopProgressSink.emit(ProgressEvent::Stdout);
    }

    #[test]
    fn label_can_be_resolved_after_config_loading() {
        let writer = SharedWriter::default();
        let sink = PlainProgressSink::with_writer(writer.clone(), "Generate");
        sink.set_label("Auto(Gemini)");
        sink.emit(ProgressEvent::Planned {
            initial_batches: 2,
            provider_tasks: 1,
            identity_tasks: 1,
        });

        assert_eq!(
            String::from_utf8(writer.0.lock().unwrap().clone()).unwrap(),
            "[2/4] 生成開始: Auto(Gemini) / 2 batches (provider: 1, identity: 1)\n"
        );
    }
}
