use std::env;
use std::fs;
use std::io::{self, Write};
use std::process;
use std::sync::Arc;

use flowcloze::config::auth_store::AuthStore;
use flowcloze::{
    build_adapter, ComposeEvent, ComposeEventKind, EventSink, FailureClass, GenerationConfig,
    IdentityComposer, IntermediateDocument, JsonLinesEventSink, ProgressEvent, ProgressSink,
    ProgressStage, RunContext,
};

pub(crate) fn inspect_scaffold(input_path: &str, output_path: Option<&str>) {
    let markdown = fs::read_to_string(input_path).unwrap_or_else(|e| {
        eprintln!("{input_path} を読めませんでした: {e}");
        process::exit(1)
    });
    let qblocks = flowcloze::parse_markdown(&markdown).unwrap_or_else(|e| {
        eprintln!("Markdownの解析に失敗しました: {e}");
        process::exit(1)
    });
    let intermediate = IntermediateDocument::from_qblocks(input_path, &qblocks);
    let scaffold = flowcloze::scaffold::build_scaffold_document(&intermediate);
    let json = serde_json::to_string_pretty(&scaffold).unwrap_or_else(|e| {
        eprintln!("scaffold JSONへの変換に失敗しました: {e}");
        process::exit(1)
    });
    if let Some(path) = output_path {
        fs::write(path, json).unwrap_or_else(|e| {
            eprintln!("{path} へ書き込めませんでした: {e}");
            process::exit(1)
        });
    } else {
        print!("{json}");
    }
}

pub(crate) fn run(
    input_path: &str,
    output_path: Option<&str>,
    config: &GenerationConfig,
    skip_constraints: bool,
    verbose: bool,
    progress: &dyn ProgressSink,
) {
    let markdown = fs::read_to_string(input_path).unwrap_or_else(|e| {
        progress.emit(ProgressEvent::Failed {
            stage: ProgressStage::Read,
            class: FailureClass::Io,
        });
        eprintln!("{input_path} を読めませんでした: {e}");
        process::exit(1)
    });
    let mut options = flowcloze::GenerateMarkdownOptions::new(input_path);
    options.policy = config.execution_policy();
    options.quota = config.quota.clone();
    options.fallback = config.fallback;
    let debug_events = verbose || matches!(env::var("FLOWCLOZE_LOG").as_deref(), Ok("debug"));
    let context = Arc::new(RunContext::new());
    let sink = Arc::new(JsonLinesEventSink::stderr(debug_events));
    let retry_context = Arc::clone(&context);
    let retry_sink = Arc::clone(&sink);
    let transport = flowcloze::http::HttpTransport::default()
        .with_quota_profile(config.quota.clone())
        .with_retry_observer(move |retry| {
            let mut event = ComposeEvent::new(ComposeEventKind::RetryDelay, &retry_context);
            event.attempt = Some(retry.attempt);
            event.retry_delay_ms = Some(retry.delay_ms);
            event.error_class = Some(retry.error_class.to_string());
            retry_sink.emit(event);
        });
    options.extra_constraints = if !config.offline && !skip_constraints {
        read_constraints()
    } else {
        Vec::new()
    };
    let outcome = if config.offline {
        flowcloze::generate_markdown_with_composer_observed_with_progress(
            &markdown,
            options,
            &IdentityComposer,
            &context,
            &*sink,
            progress,
        )
    } else {
        let model = config.model.as_ref().expect("online config has a model");
        let auth = AuthStore::load().unwrap_or_else(|e| {
            eprintln!("{e}");
            process::exit(2)
        });
        let adapter = build_adapter(model, &auth)
            .unwrap_or_else(|e| {
                eprintln!("{e}");
                process::exit(2)
            })
            .with_transport(transport);
        flowcloze::generate_markdown_with_composer_observed_with_progress(
            &markdown, options, &adapter, &context, &*sink, progress,
        )
    }
    .unwrap_or_else(|e| {
        eprintln!("{e}");
        process::exit(1)
    });
    if debug_events {
        let mut event = ComposeEvent::new(ComposeEventKind::Summary, &context);
        event.metrics = Some(sink.summary());
        sink.emit(event);
    }
    let json = serde_json::to_string_pretty(&outcome.document).unwrap_or_else(|e| {
        progress.emit(ProgressEvent::Failed {
            stage: ProgressStage::Serialize,
            class: FailureClass::Serialization,
        });
        eprintln!("生成結果JSONへの変換に失敗しました: {e}");
        process::exit(1)
    });
    if let Some(path) = output_path {
        fs::write(path, json).unwrap_or_else(|e| {
            progress.emit(ProgressEvent::Failed {
                stage: ProgressStage::Save,
                class: FailureClass::Io,
            });
            eprintln!("{path} へ書き込めませんでした: {e}");
            process::exit(1)
        });
        progress.emit(ProgressEvent::Saved {
            path: path.to_string(),
        });
    } else {
        write_stdout_json(io::stdout().lock(), &json).unwrap_or_else(|e| {
            progress.emit(ProgressEvent::Failed {
                stage: ProgressStage::Output,
                class: FailureClass::Io,
            });
            eprintln!("stdout へ書き込めませんでした: {e}");
            process::exit(1)
        });
        progress.emit(ProgressEvent::Stdout);
    }
}

fn read_constraints() -> Vec<String> {
    let mut constraints = Vec::new();
    let mut input = String::new();
    eprintln!("追加制約を入力してください．空行で終了します．");
    let _ = io::stderr().flush();
    loop {
        input.clear();
        match io::stdin().read_line(&mut input) {
            Ok(0) | Err(_) => break,
            Ok(_) if input.trim_end().is_empty() => break,
            Ok(_) => constraints.push(input.trim_end().to_string()),
        }
    }
    constraints
}

fn write_stdout_json(mut writer: impl Write, json: &str) -> io::Result<()> {
    writer.write_all(json.as_bytes())?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn stdout_json_bytes_match_generated_json() {
        let mut output = Vec::new();
        write_stdout_json(&mut output, "{\"questions\":[]}").unwrap();
        assert_eq!(output, b"{\"questions\":[]}");
    }
    #[test]
    fn stdout_write_failure_is_returned() {
        assert_eq!(
            write_stdout_json(BrokenWriter, "{}").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
