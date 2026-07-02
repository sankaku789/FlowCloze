//! FlowCloze CLIの引数解析と各サブコマンドの実行入口．

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process;

use flowcloze::{
    compile_pdf, default_pdf_output_path, parse_markdown, to_ankilot_csv, to_intermediate_json,
    validate_generated_document, validate_generated_json, GeminiClient, GeneratedDocument,
    IntermediateDocument, PdfOptions,
};

mod view;

const DEFAULT_MODEL: &str = "gemini-2.5-flash";
const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434/v1";
const DEFAULT_LM_STUDIO_BASE_URL: &str = "http://localhost:1234/v1";
const DEFAULT_LOCAL_MODEL: &str = "gemma4:e2b-it-qat";

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
            let backend = match resolve_backend(backend.as_ref()) {
                Ok(backend) => backend,
                Err(error) => {
                    eprintln!("{error}");
                    process::exit(2);
                }
            };
            let batch_policy = match resolve_batch_policy(&backend, args.batch_policy.as_ref()) {
                Ok(policy) => policy,
                Err(error) => {
                    eprintln!("{error}");
                    process::exit(2);
                }
            };
            generate_with_llm(
                input_path,
                args.output_path.as_deref(),
                &backend,
                args.skip_constraints,
                batch_policy,
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
                        skip_constraints,
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
                        batch_policy: None,
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
                        batch_policy: None,
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
                        batch_policy: None,
                    });
                }
                "--json" => json = true,
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
                "--backend" => {
                    let Some(value) = args.next() else {
                        return Err("--backend には gemini または local が必要です".to_string());
                    };
                    match &mut command {
                        Command::Generate {
                            backend: command_backend,
                            ..
                        } => *command_backend = Some(parse_backend(&value)?),
                        _ => return Err("--backend はgenerateコマンドでのみ使えます".to_string()),
                    }
                }
                "--model" => {
                    return Err(
                        "--model は廃止されました。local は gemma4:e2b-it-qat、gemini は gemini-2.5-flash を使用します"
                            .to_string(),
                    );
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
        "  flowcloze generate [-o output.json] [--backend gemini|local] [--batch auto|small|one-task] <markdown-file>"
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
    eprintln!("  generate               Geminiで問題文JSONを生成します / Generate questions JSON");
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
        "  --backend <backend>     generateで使うLLM backendを指定します(gemini/local) / LLM backend"
    );
    eprintln!(
        "  --batch <policy>        generateのbatch policyを指定します(auto/small/one-task) / Batch policy"
    );
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
    fs::write(env_path, body).map_err(|error| format!(".env を更新できませんでした: {error}"))?;
    Ok(())
}

/// local backendのセットアップ補助を実行する．
fn run_local_command(action: &LocalCommand) -> Result<(), String> {
    match action {
        LocalCommand::Check => check_local_server(),
    }
}

/// OpenAI互換local serverが応答するか確認する．
fn check_local_server() -> Result<(), String> {
    let client = reqwest::blocking::Client::new();
    let mut errors = Vec::new();

    for base_url in resolve_local_base_urls() {
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
    backend: &LlmBackend,
    skip_constraints: bool,
    batch_policy: flowcloze::planner::BatchPolicy,
) {
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
    let intermediate_json = match serde_json::to_string_pretty(&intermediate) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("中間JSONへの変換に失敗しました: {error}");
            process::exit(1);
        }
    };
    // CLIオプションに従い，外部ファイル由来の追加制約をpromptへ渡す．
    let extra_constraints = if skip_constraints {
        Vec::new()
    } else {
        read_additional_constraints()
    };
    // 中間表現から，LLMが自然化するための決定的な下書きを作る．
    let scaffold = flowcloze::scaffold::build_scaffold_document(&intermediate);

    eprintln!("問題文を生成中です．しばらくお待ち下さい....");
    let _ = io::stderr().flush();

    // Adaptive Compose Plannerでbatch生成し，失敗taskだけを単独retryする．
    let composed = match flowcloze::planner::compose_with_adaptive_planner(
        &intermediate,
        &scaffold,
        batch_policy,
        &extra_constraints,
        |prompt| generate_text_with_backend(backend, prompt),
    ) {
        Ok(composed) => composed,
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    };

    // LLMが返したquestionだけを採用し，固定フィールドは中間表現から再構築する．
    let generated_document = flowcloze::compose::merge_composed_questions(&intermediate, composed);
    // 成功済みtaskも含め，保存前にdocument全体を最終検証する．
    let report = validate_generated_document(&intermediate_json, &generated_document);
    if !report.is_valid() {
        for error in &report.errors {
            eprintln!("validation error: {error}");
        }
        eprintln!("Geminiの生成結果が最終検証に失敗したため保存しませんでした．");
        process::exit(1);
    }

    let generated_json = match serde_json::to_string_pretty(&generated_document) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("生成結果JSONへの変換に失敗しました: {error}");
            process::exit(1);
        }
    };

    if let Some(output_path) = output_path {
        if let Err(error) = fs::write(output_path, generated_json) {
            eprintln!("{output_path} へ書き込めませんでした: {error}");
            process::exit(1);
        }
    } else {
        print!("{generated_json}");
    }
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

/// backend種別に応じてGeminiまたはOpenAI互換local APIへpromptを送る．
fn generate_text_with_backend(backend: &LlmBackend, prompt: &str) -> Result<String, String> {
    match backend {
        LlmBackend::Gemini => {
            let api_key = match env::var("GEMINI_API_KEY") {
                Ok(api_key) if !api_key.trim().is_empty() => api_key,
                _ => {
                    return Err(
                        "GEMINI_API_KEY が未設定です．.env または環境変数に設定してください．"
                            .to_string(),
                    )
                }
            };
            GeminiClient::new(api_key, DEFAULT_MODEL.to_string())
                .generate_text(prompt)
                .map_err(|error| error.to_string())
        }
        LlmBackend::Local => {
            let api_key = env::var("LOCAL_LLM_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty());
            let mut errors = Vec::new();
            for base_url in resolve_local_base_urls() {
                let client = flowcloze::local_openai::LocalOpenAiClient::new(
                    base_url.clone(),
                    DEFAULT_LOCAL_MODEL.to_string(),
                    api_key.clone(),
                );
                match client.generate_text(prompt) {
                    Ok(text) => return Ok(text),
                    Err(error) => errors.push(format!("{base_url}: {error}")),
                }
            }
            Err(format!(
                "local LLM backendへの接続に失敗しました。OllamaまたはLM Studioのローカルサーバを起動してください。\n- {}",
                errors.join("\n- ")
            ))
        }
    }
}

fn resolve_local_base_urls() -> Vec<String> {
    match env::var("LOCAL_LLM_BASE_URL") {
        Ok(value) if !value.trim().is_empty() => vec![value],
        _ => vec![
            DEFAULT_OLLAMA_BASE_URL.to_string(),
            DEFAULT_LM_STUDIO_BASE_URL.to_string(),
        ],
    }
}

/// CLI/envの指定を選択backendで使うBatchPolicyへ解決する．
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
