//! 人間向けprogress出力へINFO/WARN/ERRORラベルを付与する。
//!
//! PlainProgressSinkの表示内容とheartbeatはそのまま利用し、severityだけを
//! ProgressEventから決定して行頭へ付加する。

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use crate::progress::{
    PlainProgressSink, ProgressEvent, ProgressSink, RetryResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    const fn label(self) -> &'static str {
        match self {
            Self::Info => "[INFO] ",
            Self::Warn => "[WARN] ",
            Self::Error => "[ERROR] ",
        }
    }
}

fn level_for_event(event: &ProgressEvent) -> LogLevel {
    match event {
        ProgressEvent::Parsed { .. }
        | ProgressEvent::Planned { .. }
        | ProgressEvent::Validated { .. }
        | ProgressEvent::Saved { .. }
        | ProgressEvent::Stdout => LogLevel::Info,
        ProgressEvent::BatchComplete { retries: 0, .. } => LogLevel::Info,
        ProgressEvent::BatchComplete { .. } | ProgressEvent::Fallback { .. } => LogLevel::Warn,
        ProgressEvent::Retry {
            result: RetryResult::Failed,
            ..
        }
        | ProgressEvent::ProviderError { .. }
        | ProgressEvent::Failed { .. } => LogLevel::Error,
        ProgressEvent::Retry { .. } => LogLevel::Warn,
    }
}

/// PlainProgressSinkの各行へseverityラベルを付与するsink。
/// retryはWARN、retry失敗・provider error・終端失敗はERRORとして表示する。
pub struct LabeledProgressSink {
    inner: PlainProgressSink,
    level: Arc<Mutex<LogLevel>>,
}

impl LabeledProgressSink {
    pub fn stderr(label: impl Into<String>) -> Self {
        Self::new(io::stderr(), label)
    }

    pub fn new(writer: impl Write + Send + 'static, label: impl Into<String>) -> Self {
        let level = Arc::new(Mutex::new(LogLevel::Info));
        let labeled_writer = LabeledWriter {
            inner: Box::new(writer),
            level: Arc::clone(&level),
            at_line_start: true,
        };
        Self {
            inner: PlainProgressSink::new(labeled_writer, label),
            level,
        }
    }

    pub fn set_label(&self, label: impl Into<String>) {
        self.inner.set_label(label);
    }
}

impl ProgressSink for LabeledProgressSink {
    fn emit(&self, event: ProgressEvent) {
        if let Ok(mut level) = self.level.lock() {
            *level = level_for_event(&event);
        }
        self.inner.emit(event);
    }
}

struct LabeledWriter {
    inner: Box<dyn Write + Send>,
    level: Arc<Mutex<LogLevel>>,
    at_line_start: bool,
}

impl Write for LabeledWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let level = self.level.lock().map(|level| *level).unwrap_or(LogLevel::Info);
        let prefix = level.label().as_bytes();
        let mut output = Vec::with_capacity(buf.len() + prefix.len());
        for &byte in buf {
            if self.at_line_start {
                output.extend_from_slice(prefix);
                self.at_line_start = false;
            }
            output.push(byte);
            if byte == b'\n' {
                self.at_line_start = true;
            }
        }
        self.inner.write_all(&output)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::progress::{FailureClass, ProgressStage, RetryCause};

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
    fn labels_info_warn_error_and_exposes_retry_cause() {
        let writer = SharedWriter::default();
        let sink = LabeledProgressSink::new(writer.clone(), "Provider");

        sink.emit(ProgressEvent::Parsed { tasks: 2 });
        sink.emit(ProgressEvent::Retry {
            task_id: "qblock-003".to_string(),
            attempt: 1,
            cause: RetryCause::MissingSentinel,
            result: RetryResult::Success,
        });
        sink.emit(ProgressEvent::Failed {
            stage: ProgressStage::Generate,
            class: FailureClass::Content,
        });

        assert_eq!(
            String::from_utf8(writer.0.lock().unwrap().clone()).unwrap(),
            "[INFO] [1/4] Markdown解析: 2 tasks\n[WARN]       retry: task=qblock-003 attempt=1 cause=missing_sentinel result=success\n[ERROR] [failed] stage=generate class=content detail=provider output was invalid or failed content checks\n"
        );
    }
}
