//! 人間が読む進捗表示。JSON Lines の観測イベントとは独立している。

use std::io::{self, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    MissingPlaceholder,
    DuplicatePlaceholder,
    PlaceholderOrder,
    MalformedPlaceholder,
    UnknownPlaceholder,
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
            Self::MissingPlaceholder => "missing_placeholder",
            Self::DuplicatePlaceholder => "duplicate_placeholder",
            Self::PlaceholderOrder => "placeholder_order",
            Self::MalformedPlaceholder => "malformed_placeholder",
            Self::UnknownPlaceholder => "unknown_placeholder",
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
const JST_OFFSET_SECONDS: u64 = 9 * 60 * 60;

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

struct WriterState {
    writer: Box<dyn Write + Send>,
    transient_width: usize,
}

impl WriterState {
    fn new(writer: impl Write + Send + 'static) -> Self {
        Self {
            writer: Box::new(writer),
            transient_width: 0,
        }
    }

    fn write_line(&mut self, severity: &'static str, line: &str) {
        let rendered = render_log_line(severity, line);
        if self.transient_width > 0 {
            let padding = self.transient_width.saturating_sub(rendered.chars().count());
            let _ = write!(self.writer, "\r{rendered}{}\n", " ".repeat(padding));
            self.transient_width = 0;
        } else {
            let _ = writeln!(self.writer, "{rendered}");
        }
        let _ = self.writer.flush();
    }

    fn write_transient(&mut self, severity: &'static str, line: &str) {
        let rendered = render_log_line(severity, line);
        let width = rendered.chars().count();
        let padding = self.transient_width.saturating_sub(width);
        let _ = write!(self.writer, "\r{rendered}{}", " ".repeat(padding));
        self.transient_width = width;
        let _ = self.writer.flush();
    }

    fn finish_transient(&mut self) {
        if self.transient_width > 0 {
            let _ = writeln!(self.writer);
            self.transient_width = 0;
            let _ = self.writer.flush();
        }
    }
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

fn format_jst_hms_from_unix(unix_seconds: u64) -> String {
    let seconds = (unix_seconds + JST_OFFSET_SECONDS) % (24 * 60 * 60);
    let hour = seconds / 3600;
    let minute = (seconds % 3600) / 60;
    let second = seconds % 60;
    format!("{hour:02}:{minute:02}:{second:02}")
}

fn current_jst_hms() -> String {
    let unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format_jst_hms_from_unix(unix_seconds)
}

fn render_log_line(severity: &'static str, line: &str) -> String {
    format!("[{}] [{severity}] {line}", current_jst_hms())
}

fn heartbeat_loop(
    rx: mpsc::Receiver<HeartbeatCommand>,
    writer: Arc<Mutex<WriterState>>,
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
                    if let Ok(mut writer) = writer.lock() {
                        writer.write_transient("WARN", &format_heartbeat(*phase, started.elapsed()));
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// stderr 等へ固定書式で書く sink。識別子とパスは制御文字を可視化して一行性を守る。
pub struct PlainProgressSink {
    writer: Arc<Mutex<WriterState>>,
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
            writer: Arc::new(Mutex::new(WriterState::new(writer))),
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
        if let Some(mut worker) = slot.take() {
            let _ = worker.tx.send(HeartbeatCommand::Stop);
            if let Some(handle) = worker.handle.take() {
                let _ = handle.join();
            }
        }
        if let Ok(mut writer) = self.writer.lock() {
            writer.finish_transient();
        }
    }
}

impl ProgressSink for PlainProgressSink {
    fn emit(&self, event: ProgressEvent) {
        let heartbeat_event = event.clone();
        let (severity, line) = match event {
            ProgressEvent::Parsed { tasks } => ("INFO", format!("[1/4] Markdown解析: {tasks} tasks")),
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
                (
                    "INFO",
                    format!("[2/4] 生成開始: {label} / {initial_batches} batches{counts}"),
                )
            }
            ProgressEvent::BatchComplete {
                number,
                total,
                successes,
                retries,
            } => (
                if retries > 0 { "WARN" } else { "INFO" },
                format!("      batch {number}/{total}: {successes}成功, {retries} retry"),
            ),
            ProgressEvent::Retry {
                task_id,
                attempt,
                cause,
                result,
            } => (
                "WARN",
                format!(
                    "      retry: task={} attempt={attempt} cause={} result={}",
                    escape(&task_id),
                    cause.as_str(),
                    result.as_str()
                ),
            ),
            ProgressEvent::Fallback { task_id, reason } => (
                "WARN",
                format!(
                    "      fallback: task={} reason={} detail={}",
                    escape(&task_id),
                    reason.as_str(),
                    reason.detail()
                ),
            ),
            ProgressEvent::Validated { tasks } => {
                ("INFO", format!("[3/4] 検証完了: {tasks}/{tasks}"))
            }
            ProgressEvent::Saved { path } => {
                ("INFO", format!("[4/4] 保存完了: {}", escape(&path)))
            }
            ProgressEvent::Stdout => ("INFO", "[4/4] stdout出力完了".to_string()),
            ProgressEvent::ProviderError {
                class,
                status,
                rate_limit,
            } => ("ERROR", format_provider_error(class, status, rate_limit)),
            ProgressEvent::Failed { stage, class } => (
                "ERROR",
                format!(
                    "[failed] stage={} class={} detail={}",
                    stage.as_str(),
                    class.as_str(),
                    class.detail()
                ),
            ),
        };
        if let Ok(mut writer) = self.writer.lock() {
            writer.write_line(severity, &line);
        }
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
    use super::*;

    #[test]
    fn jst_timestamp_wraps_at_midnight() {
        assert_eq!(format_jst_hms_from_unix(0), "09:00:00");
        assert_eq!(format_jst_hms_from_unix(15 * 60 * 60), "00:00:00");
    }

    #[test]
    fn heartbeat_text_is_single_line_payload() {
        assert_eq!(
            format_heartbeat(
                HeartbeatPhase::Batch {
                    number: 4,
                    total: 8,
                },
                Duration::from_secs(15),
            ),
            "      batch 4/8: provider待機中... 15s"
        );
    }

    #[test]
    fn placeholder_retry_cause_has_current_name() {
        assert_eq!(
            RetryCause::MissingPlaceholder.as_str(),
            "missing_placeholder"
        );
    }
}
