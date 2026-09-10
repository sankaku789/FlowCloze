//! PlainProgressSinkの互換ラッパー。
//!
//! severity/JST timestamp/heartbeatはPlainProgressSink側で一元管理する。

use std::io::{self, Write};

use crate::progress::{PlainProgressSink, ProgressEvent, ProgressSink};

pub struct LabeledProgressSink {
    inner: PlainProgressSink,
}

impl LabeledProgressSink {
    pub fn stderr(label: impl Into<String>) -> Self {
        Self::new(io::stderr(), label)
    }

    pub fn new(writer: impl Write + Send + 'static, label: impl Into<String>) -> Self {
        Self {
            inner: PlainProgressSink::new(writer, label),
        }
    }

    pub fn set_label(&self, label: impl Into<String>) {
        self.inner.set_label(label);
    }
}

impl ProgressSink for LabeledProgressSink {
    fn emit(&self, event: ProgressEvent) {
        self.inner.emit(event);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::progress::{FailureClass, ProgressStage, RetryCause, RetryResult};

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

    fn strip_timestamp(line: &str) -> &str {
        assert_eq!(line.as_bytes().first(), Some(&b'['));
        assert_eq!(line.as_bytes().get(9), Some(&b']'));
        assert_eq!(line.as_bytes().get(10), Some(&b' '));
        &line[11..]
    }

    #[test]
    fn delegates_labeled_output_to_plain_progress_sink() {
        let writer = SharedWriter::default();
        let sink = LabeledProgressSink::new(writer.clone(), "Provider");

        sink.emit(ProgressEvent::Parsed { tasks: 2 });
        sink.emit(ProgressEvent::Retry {
            task_id: "qblock-003".to_string(),
            attempt: 1,
            cause: RetryCause::MissingPlaceholder,
            result: RetryResult::Success,
        });
        sink.emit(ProgressEvent::Failed {
            stage: ProgressStage::Generate,
            class: FailureClass::Content,
        });

        let output = String::from_utf8(writer.0.lock().unwrap().clone()).unwrap();
        let lines = output.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            strip_timestamp(lines[0]),
            "[INFO] [1/4] Markdown解析: 2 tasks"
        );
        assert_eq!(
            strip_timestamp(lines[1]),
            "[WARN]       retry: task=qblock-003 attempt=1 cause=missing_placeholder result=success"
        );
        assert_eq!(
            strip_timestamp(lines[2]),
            "[ERROR] [failed] stage=generate class=content detail=provider output was invalid or failed content checks"
        );
    }
}
