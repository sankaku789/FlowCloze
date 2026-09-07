//! 人間が読む進捗表示。JSON Lines の観測イベントとは独立している。

use std::io::{self, Write};
use std::sync::Mutex;

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
    Failed {
        stage: ProgressStage,
        class: FailureClass,
    },
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

/// stderr 等へ固定書式で書く sink。識別子とパスは制御文字を可視化して一行性を守る。
pub struct PlainProgressSink {
    writer: Mutex<Box<dyn Write + Send>>,
    label: Mutex<String>,
}
impl PlainProgressSink {
    pub fn stderr(label: impl Into<String>) -> Self {
        Self::with_writer(io::stderr(), label)
    }

    pub fn new(writer: impl Write + Send + 'static, label: impl Into<String>) -> Self {
        Self::with_writer(writer, label)
    }

    pub fn with_writer(writer: impl Write + Send + 'static, label: impl Into<String>) -> Self {
        Self {
            writer: Mutex::new(Box::new(writer)),
            label: Mutex::new(escape(&label.into())),
        }
    }

    /// 設定解決後の実行modeを、生成開始イベントより前に反映する。
    pub fn set_label(&self, label: impl Into<String>) {
        if let Ok(mut current) = self.label.lock() {
            *current = escape(&label.into());
        }
    }
}
impl ProgressSink for PlainProgressSink {
    fn emit(&self, event: ProgressEvent) {
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
                result,
            } => format!(
                "      retry: task={} attempt={attempt} result={}",
                escape(&task_id),
                result.as_str()
            ),
            ProgressEvent::Fallback { task_id, reason } => format!(
                "      fallback: task={} reason={}",
                escape(&task_id),
                reason.as_str()
            ),
            ProgressEvent::Validated { tasks } => format!("[3/4] 検証完了: {tasks}/{tasks}"),
            ProgressEvent::Saved { path } => format!("[4/4] 保存完了: {}", escape(&path)),
            ProgressEvent::Stdout => "[4/4] stdout出力完了".to_string(),
            ProgressEvent::Failed { stage, class } => {
                format!("[failed] stage={} class={}", stage.as_str(), class.as_str())
            }
        };
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writeln!(writer, "{line}");
        }
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
