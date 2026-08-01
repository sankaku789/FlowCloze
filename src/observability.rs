//! 生成処理の本文を保持しない軽量な観測イベント。

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 同一プロセス内で一意な生成実行を表す識別子。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunContext {
    pub run_id: String,
}

impl RunContext {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self::with_clock_and_counter(nanos, &RUN_COUNTER)
    }

    /// 時計とcounterを注入して、時刻に依存しないテストを可能にする。
    pub fn with_clock_and_counter(timestamp_nanos: u128, counter: &AtomicU64) -> Self {
        let sequence = counter.fetch_add(1, Ordering::Relaxed);
        Self {
            run_id: format!("{}-{timestamp_nanos}-{sequence}", std::process::id()),
        }
    }
}

impl Default for RunContext {
    fn default() -> Self {
        Self::new()
    }
}

/// 観測イベントに含める分類。本文、prompt、応答、認証情報は持たない。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComposeEventKind {
    BatchStarted,
    RewriteDecision,
    Attempt,
    Validation,
    Fallback,
    RetryDelay,
    Summary,
}

/// JSON Linesへ安全に書き出せる観測値だけから成るイベント。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ComposeEvent {
    pub event: ComposeEventKind,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_chars: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_chars: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_delay_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_batches: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<MetricsSummary>,
}

impl ComposeEvent {
    pub fn new(event: ComposeEventKind, context: &RunContext) -> Self {
        Self {
            event,
            run_id: context.run_id.clone(),
            batch_id: None,
            task_id: None,
            attempt: None,
            compose_mode: None,
            provider: None,
            model: None,
            prompt_version: None,
            prompt_hash: None,
            source_hash: None,
            estimated_tokens: None,
            input_chars: None,
            output_chars: None,
            latency_ms: None,
            error_class: None,
            retry_delay_ms: None,
            validation_result: None,
            fallback_reason: None,
            max_concurrent_batches: None,
            metrics: None,
        }
    }
}

/// 実行中に観測した集計値。生成JSONとは独立している。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MetricsSummary {
    pub batches: u64,
    pub tasks: u64,
    pub attempts: u64,
    pub successes: u64,
    pub content_failures: u64,
    pub transport_failures: u64,
    pub fallbacks: u64,
    pub retry_delays: u64,
    pub retry_delay_ms_total: u128,
    pub latency_ms_total: u128,
    pub latency_ms_max: u128,
}

impl MetricsSummary {
    pub fn observe(&mut self, event: &ComposeEvent) {
        match event.event {
            ComposeEventKind::BatchStarted => self.batches += 1,
            ComposeEventKind::Attempt => {
                self.attempts += 1;
                self.tasks += u64::from(event.attempt == Some(0));
                if let Some(latency) = event.latency_ms {
                    self.latency_ms_total += latency;
                    self.latency_ms_max = self.latency_ms_max.max(latency);
                }
                match event.error_class.as_deref() {
                    Some("content") => self.content_failures += 1,
                    Some(_) => self.transport_failures += 1,
                    None => {}
                }
            }
            ComposeEventKind::Validation => match event.validation_result.as_deref() {
                Some("success") => self.successes += 1,
                Some(_) => self.content_failures += 1,
                None => {}
            },
            ComposeEventKind::Fallback => self.fallbacks += 1,
            ComposeEventKind::RetryDelay => {
                self.retry_delays += 1;
                self.retry_delay_ms_total += event.retry_delay_ms.unwrap_or(0);
            }
            ComposeEventKind::RewriteDecision | ComposeEventKind::Summary => {}
        }
    }
}

/// 観測イベントの出力先。Noopは既存ライブラリ利用時の既定である。
pub trait EventSink: Send + Sync {
    fn emit(&self, event: ComposeEvent);
    fn summary(&self) -> MetricsSummary {
        MetricsSummary::default()
    }
}

#[derive(Debug, Default)]
pub struct NoopEventSink;

impl EventSink for NoopEventSink {
    fn emit(&self, _: ComposeEvent) {}
}

/// debug時だけイベントをstderrへJSON Linesで出すsink。
pub struct JsonLinesEventSink {
    debug: bool,
    writer: Mutex<Box<dyn Write + Send>>,
    metrics: Mutex<MetricsSummary>,
}

impl JsonLinesEventSink {
    pub fn stderr(debug: bool) -> Self {
        Self::with_writer(debug, io::stderr())
    }

    pub fn with_writer(debug: bool, writer: impl Write + Send + 'static) -> Self {
        Self {
            debug,
            writer: Mutex::new(Box::new(writer)),
            metrics: Mutex::new(MetricsSummary::default()),
        }
    }
}

impl EventSink for JsonLinesEventSink {
    fn emit(&self, event: ComposeEvent) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.observe(&event);
        }
        if self.debug {
            if let (Ok(mut writer), Ok(line)) = (self.writer.lock(), serde_json::to_string(&event))
            {
                let _ = writeln!(writer, "{line}");
            }
        }
    }

    fn summary(&self) -> MetricsSummary {
        self.metrics.lock().map(|x| x.clone()).unwrap_or_default()
    }
}

/// 固定のFNV-1a 64-bit hashを16桁小文字hexで返す。
pub fn fnv1a_64(value: &str) -> String {
    let hash = value
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    use super::*;

    #[test]
    fn run_ids_are_unique_at_the_same_clock_value() {
        let counter = AtomicU64::new(0);
        let first = RunContext::with_clock_and_counter(7, &counter);
        let second = RunContext::with_clock_and_counter(7, &counter);
        assert_ne!(first.run_id, second.run_id);
    }

    #[test]
    fn run_ids_remain_unique_across_threads() {
        let counter = Arc::new(AtomicU64::new(0));
        let ids = (0..8)
            .map(|_| {
                let counter = Arc::clone(&counter);
                std::thread::spawn(move || RunContext::with_clock_and_counter(7, &counter).run_id)
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 8);
    }

    #[test]
    fn fnv_vectors_are_stable() {
        assert_eq!(fnv1a_64(""), "cbf29ce484222325");
        assert_eq!(fnv1a_64("hello"), "a430d84680aabd0b");
    }

    #[test]
    fn json_event_has_no_body_fields() {
        let event = ComposeEvent::new(ComposeEventKind::Attempt, &RunContext::default());
        let text = serde_json::to_string(&event).unwrap();
        for prohibited in ["question", "answer", "api_key", "response"] {
            assert!(
                !text.contains(prohibited),
                "{prohibited} must not be serialized"
            );
        }
    }

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
    fn json_lines_has_safe_fields_and_debug_can_be_disabled() {
        let writer = SharedWriter::default();
        let sink = JsonLinesEventSink::with_writer(true, writer.clone());
        let context = RunContext::default();
        let mut event = ComposeEvent::new(ComposeEventKind::Attempt, &context);
        event.task_id = Some("task-1".to_string());
        event.source_hash = Some(fnv1a_64("SECRET_SOURCE_MARKER"));
        sink.emit(event);
        let output = String::from_utf8(writer.0.lock().unwrap().clone()).unwrap();
        assert!(output.contains("\"source_hash\""));
        for marker in [
            "SECRET_SOURCE_MARKER",
            "SECRET_PROMPT_MARKER",
            "SECRET_RESPONSE_MARKER",
        ] {
            assert!(!output.contains(marker));
        }

        let quiet_writer = SharedWriter::default();
        let quiet = JsonLinesEventSink::with_writer(false, quiet_writer.clone());
        quiet.emit(ComposeEvent::new(ComposeEventKind::Attempt, &context));
        assert!(quiet_writer.0.lock().unwrap().is_empty());
    }

    #[test]
    fn metrics_aggregate_attempts_failures_and_latency() {
        let context = RunContext::default();
        let mut metrics = MetricsSummary::default();
        let mut attempt = ComposeEvent::new(ComposeEventKind::Attempt, &context);
        attempt.attempt = Some(0);
        attempt.latency_ms = Some(4);
        metrics.observe(&attempt);
        let mut failure = ComposeEvent::new(ComposeEventKind::Attempt, &context);
        failure.attempt = Some(1);
        failure.error_class = Some("content".to_string());
        failure.latency_ms = Some(9);
        metrics.observe(&failure);
        metrics.observe(&ComposeEvent::new(ComposeEventKind::Fallback, &context));
        let mut retry = ComposeEvent::new(ComposeEventKind::RetryDelay, &context);
        retry.retry_delay_ms = Some(2_000);
        metrics.observe(&retry);
        assert_eq!(metrics.tasks, 1);
        assert_eq!(metrics.attempts, 2);
        assert_eq!(metrics.content_failures, 1);
        assert_eq!(metrics.fallbacks, 1);
        assert_eq!(metrics.retry_delays, 1);
        assert_eq!(metrics.retry_delay_ms_total, 2_000);
        assert_eq!(metrics.latency_ms_total, 13);
        assert_eq!(metrics.latency_ms_max, 9);
    }
}
