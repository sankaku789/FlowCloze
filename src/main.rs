//! FlowCloze CLIの引数解析と各サブコマンドの実行入口．

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process;
use std::sync::Arc;

use flowcloze::{
    compile_pdf, default_pdf_output_path, parse_markdown, to_ankilot_csv, to_intermediate_json,
    validate_generated_json, CliOverrides, ComposeEvent, ComposeEventKind, EventSink, FailureClass,
    GeneratedDocument, GenerationConfig, IdentityComposer, IntermediateDocument,
    JsonLinesEventSink, OpenAiCompatibleAdapter, OpenAiCompatiblePool, OpenAiEndpointConfig,
    PdfOptions, PlainProgressSink, ProgressEvent, ProgressSink, ProgressStage, Provider,
    RewritePolicy, RunContext,
};

mod view;

fn main() {
    let _ = dotenvy::dotenv();

    let args = match Args::parse(env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            print_usage();
            process::exit(2);
        }
    };

    match &args.command {
        Command::Help => {
            print_help();
            return;
        }
        Command::Version => {
            print_version();
            return;
        }
        Command::Local { action } => {
            if let Err(error) = run_local_command(action) {
                eprintln!("{error}");
                process::exit(1);
            }
            return;
        }
        Command::ApiSet { api_key } => {
            eprintln!(
                "warning: api set は非推奨です。api_key_env が示す環境変数を設定してください。"
            );
            if let Err(error) = save_api_settings(api_key) {
                eprintln!("{error}");
                process::exit(1);
            }
            println!(".env を更新しました．");
            return;
        }
        Command::View { generated_path } => {
            view_generated_json(generated_path);
            return;
        }
        Command::Csv => {
            let generated_path = args
                .input_path
                .as_deref()
                .expect("csvには生成結果JSONパスが必要です");
            export_ankilot_csv(generated_path, args.output_path.as_deref());
            return;
        }
        Command::Validate {
            intermediate_path,
            generated_path,
        } => {
            validate_files(intermediate_path, generated_path);
            return;
        }
        Command::Generate { backend } => {
            let input_path = args
                .input_path
                .as_deref()
                .expect("generateには入力パスが必要です");
            let progress = PlainProgressSink::stderr("Generate");
            let config = match flowcloze::config::load(CliOverrides {
                provider: backend.as_ref().map(backend_name),
                model: args.model.clone(),
                rewrite: args.rewrite.clone(),
                fallback: args.fallback.clone(),
                structured_output: args.structured_output.clone(),
                batch: args.batch_policy.as_ref().map(batch_name),
            }) {
                Ok(config) => config,
                Err(error) => {
                    progress.emit(ProgressEvent::Failed {
                        stage: ProgressStage::Config,
                        class: FailureClass::Configuration,
                    });
                    eprintln!("{error}");
                    process::exit(2);
                }
            };
            progress.set_label(match (config.rewrite, config.provider) {
                (RewritePolicy::Never, _) => "Identity",
                (RewritePolicy::Always, Provider::Gemini) => "Gemini",
                (RewritePolicy::Always, Provider::OpenAiCompatible) => "OpenAI-compatible",
                (RewritePolicy::Auto, Provider::Gemini) => "Auto(Gemini)",
                (RewritePolicy::Auto, Provider::OpenAiCompatible) => "Auto(OpenAI-compatible)",
            });
            generate_with_llm(
                input_path,
                args.output_path.as_deref(),
                &config,
                args.skip_constraints,
                args.verbose,
                &progress,
            );
            return;
        }
        Command::InspectScaffold => {
            let input_path = args
                .input_path
                .as_deref()
                .expect("inspect-scaffoldには入力パスが必要です");
            inspect_scaffold(input_path, args.output_path.as_deref());
            return;
        }
        Command::Pdf { template_path } => {
            let input_path = args
                .input_path
                .as_deref()
                .expect("pdfには入力パスが必要です");
            compile_pdf_file(input_path, args.output_path.as_deref(), template_path);
            return;
        }
        Command::Parse => {}
    }

    let input_path = args
        .input_path
        .as_deref()
        .expect("parseには入力パスが必要です");

    let markdown = match fs::read_to_string(input_path) {
        Ok(markdown) => markdown,
        Err(error) => {
            eprintln!("{input_path} を読めませんでした: {error}");
            process::exit(1);
        }
    };

    let qblocks = match parse_markdown(&markdown) {
        Ok(qblocks) => qblocks,
        Err(error) => {
            eprintln!("Markdownの解析に失敗しました: {error}");
            process::exit(1);
        }
    };

    if args.json {
        let json = match to_intermediate_json(input_path, &qblocks) {
            Ok(json) => json,
            Err(error) => {
                eprintln!("JSONへの変換に失敗しました: {error}");
                process::exit(1);
            }
        };

        if let Some(output_path) = args.output_path {
            if let Err(error) = fs::write(&output_path, json) {
                eprintln!("{output_path} へ書き込めませんでした: {error}");
                process::exit(1);
            }
        } else {
            print!("{json}");
        }
        return;
    }

    print_text_summary(qblocks);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    command: Command,
    input_path: Option<String>,
    output_path: Option<String>,
    json: bool,
    skip_constraints: bool,
    batch_policy: Option<BatchPolicyOverride>,
    model: Option<String>,
    rewrite: Option<String>,
    fallback: Option<String>,
    structured_output: Option<String>,
    verbose: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Help,
    Version,
    Local {
        action: LocalCommand,
    },
    ApiSet {
        api_key: String,
    },
    View {
        generated_path: String,
    },
    Csv,
    Parse,
    InspectScaffold,
    Generate {
        backend: Option<LlmBackend>,
    },
    Pdf {
        template_path: String,
    },
    Validate {
        intermediate_path: String,
        generated_path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BatchPolicyOverride {
    Auto,
    Small,
    OneTask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LlmBackend {
    Gemini,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalCommand {
    Check,
}

impl Args {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut input_path = None;
        let mut output_path = None;
        let mut json = false;
        let mut skip_constraints = false;
        let mut batch_policy = None;
        let mut model = None;
        let mut rewrite = None;
        let mut fallback = None;
        let mut structured_output = None;
        let mut verbose = false;
        let mut command = Command::Parse;
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => command = Command::Help,
                "--version" | "-V" => command = Command::Version,
                "help" => return Err("help はオプションで指定してください (--help)".to_string()),
                "version" => {
                    return Err("version はオプションで指定してください (--version)".to_string())
                }
                "view" if input_path.is_none() && matches!(command, Command::Parse) => {
                    let Some(generated_path) = args.next() else {
                        return Err("viewには生成結果JSONパスが必要です".to_string());
                    };
                    if args.next().is_some() {
                        return Err("viewの引数が多すぎます".to_string());
                    }
                    return Ok(Self {
                        command: Command::View { generated_path },
                        input_path: None,
                        output_path: None,
                        json: false,
                        batch_policy: None,
                        model: None,
                        rewrite: None,
                        fallback: None,
                        structured_output: None,
                        skip_constraints,
                        verbose,
                    });
                }
                "api" if input_path.is_none() && matches!(command, Command::Parse) => {
                    let api_command = parse_api_command(&mut args)?;
                    return Ok(Self {
                        command: api_command,
                        input_path: None,
                        output_path: None,
                        json: false,
                        skip_constraints,
                        verbose,
                        batch_policy: None,
                        model: None,
                        rewrite: None,
                        fallback: None,
                        structured_output: None,
                    });
                }
                "local" if input_path.is_none() && matches!(command, Command::Parse) => {
                    let local_command = parse_local_command(&mut args)?;
                    return Ok(Self {
                        command: local_command,
                        input_path: None,
                        output_path: None,
                        json: false,
                        skip_constraints,
                        verbose,
                        batch_policy: None,
                        model: None,
                        rewrite: None,
                        fallback: None,
                        structured_output: None,
                    });
                }
                "generate" if input_path.is_none() && matches!(command, Command::Parse) => {
                    command = Command::Generate { backend: None };
                }
                "inspect-scaffold" if input_path.is_none() && matches!(command, Command::Parse) => {
                    command = Command::InspectScaffold;
                }
                "csv" if input_path.is_none() && matches!(command, Command::Parse) => {
                    command = Command::Csv;
                }
                "pdf" if input_path.is_none() && matches!(command, Command::Parse) => {
                    command = Command::Pdf {
                        template_path: "templates/cloze.typ".to_string(),
                    };
                }
                "validate" if input_path.is_none() => {
                    let Some(intermediate_path) = args.next() else {
                        return Err("validateには中間JSONパスが必要です".to_string());
                    };
                    let Some(generated_path) = args.next() else {
                        return Err("validateには生成結果JSONパスが必要です".to_string());
                    };
                    if args.next().is_some() {
                        return Err("validateの引数が多すぎます".to_string());
                    }
                    return Ok(Self {
                        command: Command::Validate {
                            intermediate_path,
                            generated_path,
                        },
                        input_path: None,
                        output_path: None,
                        json: false,
                        skip_constraints,
                        verbose,
                        batch_policy: None,
                        model: None,
                        rewrite: None,
                        fallback: None,
                        structured_output: None,
                    });
                }
                "--json" => json = true,
                "--verbose" => verbose = true,
                "-s" | "--skip-constraints" => skip_constraints = true,
                "--batch" => {
                    let Some(value) = args.next() else {
                        return Err(
                            "--batch には auto, small, one-task のいずれかが必要です".to_string()
                        );
                    };
                    if !matches!(command, Command::Generate { .. }) {
                        return Err("--batch はgenerateコマンドでのみ使えます".to_string());
                    }
                    batch_policy = Some(parse_batch_policy_override(&value)?);
                }
                "--provider" | "--backend" => {
                    let Some(value) = args.next() else {
                        return Err("--backend には gemini または local が必要です".to_string());
                    };
                    let selected = parse_backend(&value)?;
                    match &mut command {
                        Command::Generate {
                            backend: command_backend,
                            ..
                        } => {
                            if command_backend.is_some() {
                                return Err(
                                    "--provider と --backend は同時に指定できません".to_string()
                                );
                            }
                            *command_backend = Some(selected)
                        }
                        _ => {
                            return Err(
                                "--provider/--backend はgenerateコマンドでのみ使えます".to_string()
                            )
                        }
                    }
                }
                "--model" => {
                    model = Some(
                        args.next()
                            .ok_or_else(|| "--model には値が必要です".to_string())?,
                    )
                }
                "--rewrite" => {
                    rewrite = Some(args.next().ok_or_else(|| {
                        "--rewrite には always, never, auto のいずれかが必要です".to_string()
                    })?)
                }
                "--fallback" => {
                    fallback = Some(args.next().ok_or_else(|| {
                        "--fallback には error, draft のいずれかが必要です".to_string()
                    })?)
                }
                "--structured-output" => {
                    structured_output = Some(args.next().ok_or_else(|| {
                        "--structured-output には auto, on, off のいずれかが必要です".to_string()
                    })?)
                }
                "--template" => {
                    let Some(path) = args.next() else {
                        return Err("--template にはTypstテンプレートのパスが必要です".to_string());
                    };
                    match &mut command {
                        Command::Pdf { template_path } => *template_path = path,
                        _ => return Err("--template はpdfコマンドでのみ使えます".to_string()),
                    }
                }
                "-o" | "--output" => {
                    let Some(path) = args.next() else {
                        return Err(format!("{arg} には出力先パスが必要です"));
                    };
                    output_path = Some(path);
                }
                _ if arg.starts_with("-s") => {
                    return Err("-s は単独で指定してください".to_string());
                }
                _ if arg.starts_with('-') => return Err(format!("未知のオプションです: {arg}")),
                _ => {
                    if matches!(command, Command::Help | Command::Version) {
                        return Err("help/version には追加引数を指定できません".to_string());
                    }
                    if input_path.is_some() {
                        return Err(duplicate_input_error(&command));
                    }
                    input_path = Some(arg);
                }
            }
        }

        if input_path.is_none() {
            match command {
                Command::Parse | Command::Generate { .. } | Command::InspectScaffold => {
                    return Err("入力Markdownファイルを指定してください".to_string());
                }
                Command::Csv => {
                    return Err("csvには生成結果JSONパスが必要です".to_string());
                }
                Command::Pdf { .. } => {
                    return Err("pdfには生成結果JSONパスが必要です".to_string());
                }
                Command::Help
                | Command::Version
                | Command::Local { .. }
                | Command::ApiSet { .. }
                | Command::View { .. }
                | Command::Validate { .. } => {}
            }
        }

        if output_path.is_some() && matches!(command, Command::Parse) {
            json = true;
        }

        Ok(Self {
            command,
            input_path,
            output_path,
            json,
            skip_constraints,
            batch_policy,
            model,
            rewrite,
            fallback,
            structured_output,
            verbose,
        })
    }
}

/// `flowcloze local ...` 配下のサブコマンドを解析する．
fn parse_local_command(args: &mut impl Iterator<Item = String>) -> Result<Command, String> {
    let Some(subcommand) = args.next() else {
        return Err("local にはサブコマンドが必要です (install/check)".to_string());
    };

    match subcommand.as_str() {
        "check" => {
            if args.next().is_some() {
                return Err("local check は引数なしで実行してください".to_string());
            }
            Ok(Command::Local {
                action: LocalCommand::Check,
            })
        }
        other => Err(format!(
            "未知のlocalサブコマンドです: {other}。check を指定してください"
        )),
    }
}

/// コマンド種別に応じて，入力ファイルが重複指定された時のエラーメッセージを作る．
fn duplicate_input_error(command: &Command) -> String {
    match command {
        Command::Csv => "生成結果JSONファイルは1つだけ指定してください".to_string(),
        Command::Pdf { .. } => "生成結果JSONファイルは1つだけ指定してください".to_string(),
        _ => "入力Markdownファイルは1つだけ指定してください".to_string(),
    }
}

/// `flowcloze api ...` 配下のサブコマンドを解析する．
fn parse_api_command(args: &mut impl Iterator<Item = String>) -> Result<Command, String> {
    let Some(subcommand) = args.next() else {
        return Err("api にはサブコマンドが必要です (set)".to_string());
    };

    match subcommand.as_str() {
        "set" => {
            let mut api_key = None;

            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--key" => {
                        let Some(value) = args.next() else {
                            return Err("--key にはAPIキーが必要です".to_string());
                        };
                        api_key = Some(value);
                    }
                    "--model" => {
                        return Err(
                            "--model は廃止されました。Gemini API では gemini-2.5-flash を使用します"
                                .to_string(),
                        );
                    }
                    _ if arg.starts_with('-') => {
                        return Err(format!("未知のオプションです: {arg}"))
                    }
                    _ => return Err("api set はオプションのみ指定できます".to_string()),
                }
            }

            let Some(api_key) = api_key.filter(|value| !value.trim().is_empty()) else {
                return Err("api set には --key が必要です".to_string());
            };

            Ok(Command::ApiSet { api_key })
        }
        _ => Err("api のサブコマンドは set のみです".to_string()),
    }
}

/// 短い使い方をstderrへ表示する．
fn print_usage() {
    eprintln!("使い方 / Usage:");
    eprintln!("  flowcloze [--json] [-o output.json] <markdown-file>");
    eprintln!(
        "  flowcloze generate [-o output.json] [--verbose] [--provider gemini|local] [--model model] [--rewrite always|never|auto] [--fallback error|draft] [--structured-output auto|on|off] [--batch auto|small|one-task] <markdown-file>"
    );
    eprintln!("  flowcloze local check");
    eprintln!("  flowcloze inspect-scaffold [-o scaffold.json] <markdown-file>");
    eprintln!("  flowcloze validate <intermediate.json> <generated.json>");
    eprintln!("  flowcloze view <generated.json>");
    eprintln!("  flowcloze csv [-o output.csv] <generated.json>");
    eprintln!("  flowcloze pdf [-o output.pdf] [--template template.typ] <generated.json>");
    eprintln!("  flowcloze api set --key <api_key>");
}

/// 詳細ヘルプをstderrへ表示する．
fn print_help() {
    print_usage();
    eprintln!("\nコマンド / Commands:");
    eprintln!(
        "  (default)              Markdownを解析して概要を表示します / Parse markdown summary"
    );
    eprintln!(
        "  generate               providerで問題文JSONを生成します / Generate questions JSON"
    );
    eprintln!("  local check            Ollama/LM Studioのlocal server接続を確認します / Check the local server");
    eprintln!("  inspect-scaffold       LLM入力用scaffoldを表示します / Inspect scaffold JSON");
    eprintln!("  validate               中間JSONと生成JSONを検証します / Validate JSON pairs");
    eprintln!("  view                   生成JSONをTUIで表示します / View generated JSON in TUI");
    eprintln!("  csv                    生成JSONからAnkilot用CSVを作成します / Export Ankilot CSV");
    eprintln!("  pdf                    生成JSONからPDFを作成します / Build PDF from JSON");
    eprintln!("  api set                APIキーを.envに保存します / Save API key to .env");
    eprintln!("\nMarkdown記法 / Markdown Syntax:");
    eprintln!("  #qblock{{ ... }}        問題化範囲を囲みます / Mark a question range");
    eprintln!("  [答え]                 解答対象を指定します / Mark an answer target");
    eprintln!("  [答え]{{type}}          任意で出題観点を指定します / Optional target type");
    eprintln!("\nオプション / Options:");
    eprintln!("  --json                 中間JSONを出力します / Output intermediate JSON");
    eprintln!("  -s                     追加制約の入力をスキップします / Skip extra constraints");
    eprintln!("  -o, --output <path>     出力先を指定します / Set output path");
    eprintln!(
        "  --provider <provider>   generateで使うLLM providerを指定します(gemini/local)。--backendは別名 / LLM provider"
    );
    eprintln!(
        "  --batch <policy>        generateのbatch policyを指定します(auto/small/one-task) / Batch policy"
    );
    eprintln!("  --verbose               通常の進捗表示に観測JSON Linesをstderrへ追加します (FLOWCLOZE_LOG=debugでも有効)");
    eprintln!("                           max_concurrent_batchesは検証・観測のみで、現在は並列実行しません");
    eprintln!(
        "  --template <path>       pdfのTypstテンプレートを指定します / Typst template for pdf"
    );
    eprintln!("  -h, --help              ヘルプを表示します / Show help");
    eprintln!("  -V, --version           バージョンを表示します / Show version");
}

/// crate versionをCLIのバージョン表示として出力する．
fn print_version() {
    println!("flowcloze {}", env!("CARGO_PKG_VERSION"));
}

/// 生成JSONを読み込み，TUI viewerへ渡す．
fn view_generated_json(generated_path: &str) {
    let generated_json = match fs::read_to_string(generated_path) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("{generated_path} を読めませんでした: {error}");
            process::exit(1);
        }
    };
    let document = match serde_json::from_str::<GeneratedDocument>(&generated_json) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("生成結果JSONを読めません: {error}");
            process::exit(1);
        }
    };
    if let Err(error) = view::run_viewer(document) {
        eprintln!("TUIの表示に失敗しました: {error}");
        process::exit(1);
    }
}

/// 生成JSONをAnkilot向けCSVへ変換し，指定先またはstdoutへ出力する．
fn export_ankilot_csv(generated_path: &str, output_path: Option<&str>) {
    let generated_json = match fs::read_to_string(generated_path) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("{generated_path} を読めませんでした: {error}");
            process::exit(1);
        }
    };
    let document = match serde_json::from_str::<GeneratedDocument>(&generated_json) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("生成結果JSONを読めません: {error}");
            process::exit(1);
        }
    };
    let csv = to_ankilot_csv(&document);
    if let Some(output_path) = output_path {
        if let Err(error) = fs::write(output_path, csv) {
            eprintln!("{output_path} へ書き込めませんでした: {error}");
            process::exit(1);
        }
    } else {
        print!("{csv}");
    }
}

/// Gemini API keyを.envへ保存する．
fn save_api_settings(api_key: &str) -> Result<(), String> {
    if api_key.contains(['\r', '\n', '\0']) {
        return Err("APIキーに改行またはNULを含めることはできません".to_string());
    }
    let env_path = PathBuf::from(".env");
    let existing = fs::read_to_string(&env_path).unwrap_or_default();
    let mut lines = Vec::new();
    let mut has_key = false;

    for line in existing.lines() {
        if line.trim_start().starts_with("GEMINI_API_KEY=") {
            lines.push(format!("GEMINI_API_KEY={api_key}"));
            has_key = true;
        } else if !line.trim_start().starts_with("GEMINI_MODEL=") {
            lines.push(line.to_string());
        }
    }

    if !has_key {
        lines.push(format!("GEMINI_API_KEY={api_key}"));
    }

    let mut body = lines.join("\n");
    if !body.ends_with('\n') {
        body.push('\n');
    }
    atomic_write_env(&env_path, &body)
}

fn atomic_write_env(env_path: &std::path::Path, body: &str) -> Result<(), String> {
    let (temporary, mut temporary_file) = open_secure_temp(env_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary_file
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| ".env を更新できませんでした".to_string())?;
    }
    let write_result = temporary_file
        .write_all(body.as_bytes())
        .and_then(|_| temporary_file.sync_all());
    if write_result.is_err() {
        drop(temporary_file);
        let _ = fs::remove_file(&temporary);
        return Err(".env を更新できませんでした".to_string());
    }
    drop(temporary_file);
    if fs::rename(&temporary, env_path).is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(".env を更新できませんでした".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(env_path, fs::Permissions::from_mode(0o600))
            .map_err(|_| ".env を更新できませんでした".to_string())?;
    }
    Ok(())
}

fn open_secure_temp(env_path: &std::path::Path) -> Result<(PathBuf, fs::File), String> {
    use std::fs::OpenOptions;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let parent = env_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    for _ in 0..16 {
        let mut random = [0u8; 16];
        getrandom::getrandom(&mut random).map_err(|_| ".env を更新できませんでした".to_string())?;
        let path = parent.join(format!(
            ".{}.{}.tmp",
            env_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("env"),
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(".env を更新できませんでした".to_string()),
        }
    }
    Err(".env を更新できませんでした".to_string())
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod api_set_tests {
    use super::*;

    #[test]
    fn atomic_env_write_uses_private_permissions_and_redacts_failures() {
        let mut random = [0u8; 8];
        getrandom::getrandom(&mut random).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "flowcloze-api-set-{}",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join(".env");
        atomic_write_env(&path, "GEMINI_API_KEY=secret-marker\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "GEMINI_API_KEY=secret-marker\n"
        );
        let error = atomic_write_env(&directory.join("missing/.env"), "secret-marker").unwrap_err();
        assert!(!error.contains("secret-marker"));
        fs::remove_dir_all(directory).unwrap();
    }
}

/// local backendのセットアップ補助を実行する．
fn run_local_command(action: &LocalCommand) -> Result<(), String> {
    match action {
        LocalCommand::Check => {
            let config = flowcloze::config::load(CliOverrides {
                provider: Some("local".to_string()),
                ..CliOverrides::default()
            })?;
            check_local_server(&config)
        }
    }
}

/// OpenAI互換local serverが応答するか確認する．
fn check_local_server(config: &GenerationConfig) -> Result<(), String> {
    let client = reqwest::blocking::Client::new();
    let mut errors = Vec::new();

    for base_url in flowcloze::local_openai_url_candidates(config.base_url.as_deref()) {
        let url = format!("{}/models", base_url.trim_end_matches('/'));
        match client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
        {
            Ok(response) if response.status().is_success() => {
                println!("local server ok: {url}");
                return Ok(());
            }
            Ok(response) => errors.push(format!("{url}: HTTP {}", response.status())),
            Err(error) => errors.push(format!("{url}: {error}")),
        }
    }

    Err(format!(
        "local serverに接続できませんでした。OllamaまたはLM Studioのローカルサーバを起動してください。\n- {}",
        errors.join("\n- ")
    ))
}

/// 生成JSONとTypst templateからPDFを作成するCLI用ラッパー．
fn compile_pdf_file(generated_json_path: &str, output_path: Option<&str>, template_path: &str) {
    let output_pdf_path = output_path
        .map(PathBuf::from)
        .unwrap_or_else(|| default_pdf_output_path(generated_json_path));
    let options = PdfOptions {
        generated_json_path: PathBuf::from(generated_json_path),
        output_pdf_path: output_pdf_path.clone(),
        template_path: PathBuf::from(template_path),
    };

    if let Err(error) = compile_pdf(&options) {
        eprintln!("{error}");
        process::exit(1);
    }

    println!("{}", output_pdf_path.display());
}

/// Markdownを解析し，選択されたLLM backendで問題JSONを生成する．
fn generate_with_llm(
    input_path: &str,
    output_path: Option<&str>,
    config: &GenerationConfig,
    skip_constraints: bool,
    verbose: bool,
    progress: &dyn ProgressSink,
) {
    let markdown = match fs::read_to_string(input_path) {
        Ok(markdown) => markdown,
        Err(error) => {
            progress.emit(ProgressEvent::Failed {
                stage: ProgressStage::Read,
                class: FailureClass::Io,
            });
            eprintln!("{input_path} を読めませんでした: {error}");
            process::exit(1);
        }
    };
    let mut options = flowcloze::GenerateMarkdownOptions::new(input_path);
    options.policy.batch_policy = config.batch_policy();
    options.rewrite = config.rewrite;
    options.fallback = config.fallback;
    let debug_events = verbose || matches!(env::var("FLOWCLOZE_LOG").as_deref(), Ok("debug"));
    let context = Arc::new(RunContext::new());
    let sink = Arc::new(JsonLinesEventSink::stderr(debug_events));
    let retry_context = Arc::clone(&context);
    let retry_sink = Arc::clone(&sink);
    let retry_transport =
        flowcloze::http::HttpTransport::default().with_retry_observer(move |retry| {
            let mut event = ComposeEvent::new(ComposeEventKind::RetryDelay, &retry_context);
            event.attempt = Some(retry.attempt);
            event.retry_delay_ms = Some(retry.delay_ms);
            event.error_class = Some(retry.error_class.to_string());
            retry_sink.emit(event);
        });
    let needs_provider = match config.rewrite {
        RewritePolicy::Always => true,
        RewritePolicy::Never => false,
        RewritePolicy::Auto => parse_markdown(&markdown)
            .map(|qblocks| {
                qblocks.iter().any(|qblock| {
                    !flowcloze::orchestration::auto_rewrite_reasons(&qblock.source_text).is_empty()
                })
            })
            .unwrap_or(true),
    };
    // identityだけの実行ではstdinを読まず、provider requestがある時だけ聞く。
    options.extra_constraints = if needs_provider && !skip_constraints {
        read_additional_constraints()
    } else {
        Vec::new()
    };
    let outcome = if !needs_provider {
        flowcloze::generate_markdown_with_composer_observed_with_progress(
            &markdown,
            options,
            &IdentityComposer,
            &context,
            &*sink,
            progress,
        )
    } else {
        match config.provider {
            Provider::Gemini => {
                let key = config.api_key().unwrap_or_else(|error| {
                    eprintln!("{error}");
                    progress.emit(ProgressEvent::Failed {
                        stage: ProgressStage::Config,
                        class: FailureClass::Authentication,
                    });
                    process::exit(2)
                });
                let base_url = config.base_url.clone().unwrap_or_else(|| {
                    eprintln!("Gemini OpenAI-compatible endpointが設定されていません");
                    process::exit(2)
                });
                let endpoint = OpenAiEndpointConfig::new(base_url, config.model.clone())
                    .with_bearer(key)
                    .with_provider_label("gemini");
                let adapter = OpenAiCompatibleAdapter::from_endpoint(endpoint)
                    .with_structured_output(config.structured_output)
                    .with_transport(retry_transport.clone());
                flowcloze::generate_markdown_with_composer_observed_with_progress(
                    &markdown, options, &adapter, &context, &*sink, progress,
                )
            }
            Provider::OpenAiCompatible => {
                let adapter = OpenAiCompatiblePool::from_candidates(
                    config.base_url.as_deref(),
                    config.model.clone(),
                    env::var(&config.api_key_env).ok(),
                )
                .with_structured_output(config.structured_output)
                .with_transport(retry_transport.clone());
                flowcloze::generate_markdown_with_composer_observed_with_progress(
                    &markdown, options, &adapter, &context, &*sink, progress,
                )
            }
        }
    }
    .unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1)
    });
    if debug_events {
        let mut event = ComposeEvent::new(ComposeEventKind::Summary, &context);
        event.metrics = Some(sink.summary());
        sink.emit(event);
    }
    let generated_document = outcome.document;

    let generated_json = match serde_json::to_string_pretty(&generated_document) {
        Ok(json) => json,
        Err(error) => {
            progress.emit(ProgressEvent::Failed {
                stage: ProgressStage::Serialize,
                class: FailureClass::Serialization,
            });
            eprintln!("生成結果JSONへの変換に失敗しました: {error}");
            process::exit(1);
        }
    };

    if let Some(output_path) = output_path {
        if let Err(error) = fs::write(output_path, generated_json) {
            progress.emit(ProgressEvent::Failed {
                stage: ProgressStage::Save,
                class: FailureClass::Io,
            });
            eprintln!("{output_path} へ書き込めませんでした: {error}");
            process::exit(1);
        }
        progress.emit(ProgressEvent::Saved {
            path: output_path.to_string(),
        });
    } else {
        let stdout = io::stdout();
        if let Err(error) = write_stdout_json(stdout.lock(), &generated_json) {
            progress.emit(ProgressEvent::Failed {
                stage: ProgressStage::Output,
                class: FailureClass::Io,
            });
            eprintln!("stdout へ書き込めませんでした: {error}");
            process::exit(1);
        }
        progress.emit(ProgressEvent::Stdout);
    }
}

/// stdout は完了通知前に lock した writer へ全量を書き、失敗を呼び出し側へ返す。
fn write_stdout_json(mut writer: impl Write, json: &str) -> io::Result<()> {
    writer.write_all(json.as_bytes())?;
    writer.flush()
}

/// Markdownからscaffoldを構築し，LLMへ渡す下書きJSONとして出力する．
fn inspect_scaffold(input_path: &str, output_path: Option<&str>) {
    let markdown = match fs::read_to_string(input_path) {
        Ok(markdown) => markdown,
        Err(error) => {
            eprintln!("{input_path} を読めませんでした: {error}");
            process::exit(1);
        }
    };
    let qblocks = match parse_markdown(&markdown) {
        Ok(qblocks) => qblocks,
        Err(error) => {
            eprintln!("Markdownの解析に失敗しました: {error}");
            process::exit(1);
        }
    };
    let intermediate = IntermediateDocument::from_qblocks(input_path, &qblocks);
    let scaffold = flowcloze::scaffold::build_scaffold_document(&intermediate);
    let scaffold_json = match serde_json::to_string_pretty(&scaffold) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("scaffold JSONへの変換に失敗しました: {error}");
            process::exit(1);
        }
    };

    if let Some(output_path) = output_path {
        if let Err(error) = fs::write(output_path, scaffold_json) {
            eprintln!("{output_path} へ書き込めませんでした: {error}");
            process::exit(1);
        }
    } else {
        print!("{scaffold_json}");
    }
}

/// CLI/envの指定を選択backendで使うBatchPolicyへ解決する．
#[allow(dead_code)]
fn resolve_batch_policy(
    backend: &LlmBackend,
    cli_override: Option<&BatchPolicyOverride>,
) -> Result<flowcloze::planner::BatchPolicy, String> {
    let env_override = match env::var("FLOWCLOZE_BATCH_POLICY") {
        Ok(value) if !value.trim().is_empty() => Some(parse_batch_policy_override(&value)?),
        _ => None,
    };
    let selected = cli_override.or(env_override.as_ref());
    let mut policy = match selected {
        Some(BatchPolicyOverride::Auto) | None => match backend {
            LlmBackend::Gemini => flowcloze::planner::BatchPolicy::gemini_default(),
            LlmBackend::Local => flowcloze::planner::BatchPolicy::local_default(),
        },
        Some(BatchPolicyOverride::Small) => flowcloze::planner::BatchPolicy {
            max_tasks_per_batch: 2,
            max_estimated_input_tokens: 4_000,
            max_retry_count: 2,
            max_concurrent_batches: 1,
        },
        Some(BatchPolicyOverride::OneTask) => flowcloze::planner::BatchPolicy {
            max_tasks_per_batch: 1,
            max_estimated_input_tokens: 12_000,
            max_retry_count: 2,
            max_concurrent_batches: 1,
        },
    };

    // 数値系envはpolicy種別の初期値へ重ねる個別上書きとして扱う．
    override_usize_env(
        "FLOWCLOZE_MAX_TASKS_PER_BATCH",
        &mut policy.max_tasks_per_batch,
    )?;
    override_usize_env(
        "FLOWCLOZE_MAX_INPUT_TOKENS",
        &mut policy.max_estimated_input_tokens,
    )?;
    override_usize_env(
        "FLOWCLOZE_MAX_CONCURRENT_BATCHES",
        &mut policy.max_concurrent_batches,
    )?;

    if policy.max_tasks_per_batch == 0 {
        return Err("FLOWCLOZE_MAX_TASKS_PER_BATCH は1以上にしてください".to_string());
    }
    if policy.max_estimated_input_tokens == 0 {
        return Err("FLOWCLOZE_MAX_INPUT_TOKENS は1以上にしてください".to_string());
    }
    if policy.max_concurrent_batches == 0 {
        return Err("FLOWCLOZE_MAX_CONCURRENT_BATCHES は1以上にしてください".to_string());
    }

    Ok(policy)
}

/// CLI/envの指定から使用するLLM backendを決める．
#[allow(dead_code)]
fn resolve_backend(cli_backend: Option<&LlmBackend>) -> Result<LlmBackend, String> {
    if let Some(backend) = cli_backend {
        return Ok(backend.clone());
    }
    match env::var("FLOWCLOZE_LLM_BACKEND") {
        Ok(value) if !value.trim().is_empty() => parse_backend(&value),
        _ => Ok(LlmBackend::Gemini),
    }
}

/// `--backend` や `FLOWCLOZE_LLM_BACKEND` の文字列を内部表現へ変換する．
fn parse_backend(value: &str) -> Result<LlmBackend, String> {
    match value.trim() {
        "gemini" => Ok(LlmBackend::Gemini),
        "local" => Ok(LlmBackend::Local),
        other => Err(format!(
            "未知のLLM backendです: {other}。gemini または local を指定してください"
        )),
    }
}

fn backend_name(backend: &LlmBackend) -> String {
    match backend {
        LlmBackend::Gemini => "gemini",
        LlmBackend::Local => "local",
    }
    .to_string()
}

fn batch_name(batch: &BatchPolicyOverride) -> String {
    match batch {
        BatchPolicyOverride::Auto => "auto",
        BatchPolicyOverride::Small => "small",
        BatchPolicyOverride::OneTask => "one-task",
    }
    .to_string()
}

/// `--batch` や `FLOWCLOZE_BATCH_POLICY` の文字列を内部表現へ変換する．
fn parse_batch_policy_override(value: &str) -> Result<BatchPolicyOverride, String> {
    match value.trim() {
        "auto" => Ok(BatchPolicyOverride::Auto),
        "small" => Ok(BatchPolicyOverride::Small),
        "one-task" => Ok(BatchPolicyOverride::OneTask),
        other => Err(format!(
            "未知のbatch policyです: {other}。auto, small, one-task のいずれかを指定してください"
        )),
    }
}

/// 正の整数envが設定されていれば対象policy値へ反映する．
#[allow(dead_code)]
fn override_usize_env(name: &str, target: &mut usize) -> Result<(), String> {
    let Ok(value) = env::var(name) else {
        return Ok(());
    };
    if value.trim().is_empty() {
        return Ok(());
    }
    *target = value
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("{name} には正の整数を指定してください"))?;
    Ok(())
}

fn read_additional_constraints() -> Vec<String> {
    let mut constraints = Vec::new();
    let mut input = String::new();
    eprintln!("追加制約を入力してください．空行で終了します．");
    let _ = io::stderr().flush();

    loop {
        input.clear();
        match io::stdin().read_line(&mut input) {
            Ok(0) => break,
            Ok(_) => {
                let line = input.trim_end();
                if line.is_empty() {
                    break;
                }
                constraints.push(line.to_string());
            }
            Err(_) => break,
        }
    }

    constraints
}

/// 中間JSONと生成JSONを読み込み，検証結果をCLIへ表示する．
fn validate_files(intermediate_path: &str, generated_path: &str) {
    let intermediate_json = match fs::read_to_string(intermediate_path) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("{intermediate_path} を読めませんでした: {error}");
            process::exit(1);
        }
    };
    let generated_json = match fs::read_to_string(generated_path) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("{generated_path} を読めませんでした: {error}");
            process::exit(1);
        }
    };
    let report = validate_generated_json(&intermediate_json, &generated_json);
    if report.is_valid() {
        println!("validation ok");
        return;
    }

    for error in report.errors {
        eprintln!("validation error: {error}");
    }
    process::exit(1);
}

/// LLMを使わない通常parse時に，抽出されたqblock概要を表示する．
fn print_text_summary(qblocks: Vec<flowcloze::QBlock>) {
    for qblock in qblocks {
        println!("{}", qblock.id);

        for target in qblock.targets {
            println!("  - {} ({})", target.answer, target.target_type);
        }

        for warning in qblock.warnings {
            println!("  warning: {warning}");
        }
    }
}

#[cfg(test)]
mod stdout_tests {
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
