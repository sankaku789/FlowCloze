//! 人間向けprogress出力へ時刻とINFO/WARN/ERRORラベルを付与する。
//!
//! PlainProgressSinkの表示内容とheartbeatはそのまま利用し、severityだけを
//! ProgressEventから決定して行頭へ付加する。provider待機heartbeatは
//! 改行を増やさず、同じ端末行を更新する。

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::progress::{PlainProgressSink, ProgressEvent, ProgressSink, RetryResult};

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

fn utc_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
        % 86_400;
    let hour = seconds / 3_600;
    let minute = (seconds % 3_600) / 60;
    let second = seconds % 60;
    format!("[{hour:02}:{minute:02}:{second:02}Z] ")
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

/// PlainProgressSinkの各行へUTC時刻とseverityラベルを付与するsink。
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
            line_buffer: Vec::new(),
            heartbeat_active: false,
            last_rendered_width: 0,
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
    line_buffer: Vec<u8>,
    heartbeat_active: bool,
    last_rendered_width: usize,
}

impl LabeledWriter {
    fn render_line(&self, line: &str) -> String {
        let level = self
            .level
            .lock()
            .map(|level| *level)
            .unwrap_or(LogLevel::Info);
        format!("{}{}{line}", utc_timestamp(), level.label())
    }

    fn flush_complete_line(&mut self) -> io::Result<()> {
        let line = String::from_utf8_lossy(&self.line_buffer).into_owned();
        self.line_buffer.clear();
        let rendered = self.render_line(&line);
        let is_heartbeat = line.contains("provider待機中...");

        if is_heartbeat {
            self.inner.write_all(b"\r")?;
            self.inner.write_all(rendered.as_bytes())?;
            if self.last_rendered_width > rendered.len() {
                self.inner
                    .write_all(&vec![b' '; self.last_rendered_width - rendered.len()])?;
                self.inner.write_all(b"\r")?;
                self.inner.write_all(rendered.as_bytes())?;
            }
            self.heartbeat_active = true;
            self.last_rendered_width = rendered.len();
            self.inner.flush()?;
            return Ok(());
        }

        if self.heartbeat_active {
            self.inner.write_all(b"\r")?;
            self.inner.write_all(rendered.as_bytes())?;
            if self.last_rendered_width > rendered.len() {
                self.inner
                    .write_all(&vec![b' '; self.last_rendered_width - rendered.len()])?;
                self.inner.write_all(b"\r")?;
                self.inner.write_all(rendered.as_bytes())?;
            }
        } else {
            self.inner.write_all(rendered.as_bytes())?;
        }
        self.inner.write_all(b"\n")?;
        self.heartbeat_active = false;
        self.last_rendered_width = 0;
        Ok(())
    }
}

impl Write for LabeledWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &byte in buf {
            if byte == b'\n' {
                self.flush_complete_line()?;
            } else {
                self.line_buffer.push(byte);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.line_buffer.is_empty() {
            let pending = std::mem::take(&mut self.line_buffer);
            self.inner.write_all(&pending)?;
        }
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

    fn strip_timestamp(line: &str) -> &str {
        assert_eq!(line.as_bytes().get(0), Some(&b'['));
        assert_eq!(line.as_bytes().get(9), Some(&b'Z'));
        assert_eq!(line.as_bytes().get(10), Some(&b']'));
        assert_eq!(line.as_bytes().get(11), Some(&b' '));
        &line[12..]
    }

    #[test]
    fn labels_info_warn_error_with_utc_time_and_exposes_retry_cause() {
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

        let output = String::from_utf8(writer.0.lock().unwrap().clone()).unwrap();
        let lines = output.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            strip_timestamp(lines[0]),
            "[INFO] [1/4] Markdown解析: 2 tasks"
        );
        assert_eq!(
            strip_timestamp(lines[1]),
            "[WARN]       retry: task=qblock-003 attempt=1 cause=missing_sentinel result=success"
        );
        assert_eq!(
            strip_timestamp(lines[2]),
            "[ERROR] [failed] stage=generate class=content detail=provider output was invalid or failed content checks"
        );
    }

    #[test]
    fn provider_wait_heartbeat_rewrites_one_terminal_line() {
        let writer = SharedWriter::default();
        let level = Arc::new(Mutex::new(LogLevel::Warn));
        let mut labeled = LabeledWriter {
            inner: Box::new(writer.clone()),
            level,
            line_buffer: Vec::new(),
            heartbeat_active: false,
            last_rendered_width: 0,
        };

        writeln!(labeled, "      batch 4/8: provider待機中... 5s").unwrap();
        writeln!(labeled, "      batch 4/8: provider待機中... 10s").unwrap();
        writeln!(labeled, "      batch 4/8: 3成功, 0 retry").unwrap();

        let output = String::from_utf8(writer.0.lock().unwrap().clone()).unwrap();
        assert_eq!(output.matches("provider待機中...").count(), 2);
        assert!(output.starts_with('\r'));
        assert!(output.contains("5s\r"));
        assert!(output.contains("10s\r"));
        assert_eq!(output.matches('\n').count(), 1);
        assert!(output.ends_with("3成功, 0 retry\n"));
    }
}
