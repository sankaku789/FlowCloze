//! FlowCloze CLIの引数解析と各サブコマンドの実行入口．

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process;

use flowcloze::{
    build_generation_prompt, compile_pdf, default_pdf_output_path, parse_markdown, to_ankilot_csv,
    to_intermediate_json, validate_generated_document, validate_generated_json, GeminiClient,
    GeneratedDocument, IntermediateDocument, PdfOptions, ValidationError,
};

mod view;

const MAX_GENERATION_ATTEMPTS: u32 = 3;
const DEFAULT_MODEL: &str = "gemini-2.5-flash";

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
        Command::ApiSet { api_key, model } => {
            if let Err(error) = save_api_settings(api_key, model.as_deref()) {
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
        Command::Generate { model } => {
            let input_path = args
                .input_path
                .as_deref()
                .expect("generateには入力パスが必要です");
            generate_with_gemini(
                input_path,
                args.output_path.as_deref(),
                model.as_deref(),
                args.skip_constraints,
            );
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Help,
    Version,
    ApiSet {
        api_key: String,
        model: Option<String>,
    },
    View {
        generated_path: String,
    },
    Csv,
    Parse,
    Generate {
        model: Option<String>,
    },
    Pdf {
        template_path: String,
    },
    Validate {
        intermediate_path: String,
        generated_path: String,
    },
}

impl Args {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut input_path = None;
        let mut output_path = None;
        let mut json = false;
        let mut skip_constraints = false;
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
                    });
                }
                "generate" if input_path.is_none() && matches!(command, Command::Parse) => {
                    command = Command::Generate { model: None };
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
                    });
                }
                "--json" => json = true,
                "-s" | "--skip-constraints" => skip_constraints = true,
                "--model" => {
                    let Some(model) = args.next() else {
                        return Err("--model にはモデル名が必要です".to_string());
                    };
                    match &mut command {
                        Command::Generate {
                            model: command_model,
                        } => *command_model = Some(model),
                        _ => return Err("--model はgenerateコマンドでのみ使えます".to_string()),
                    }
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
                Command::Parse | Command::Generate { .. } => {
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
        })
    }
}

fn duplicate_input_error(command: &Command) -> String {
    match command {
        Command::Csv => "生成結果JSONファイルは1つだけ指定してください".to_string(),
        Command::Pdf { .. } => "生成結果JSONファイルは1つだけ指定してください".to_string(),
        _ => "入力Markdownファイルは1つだけ指定してください".to_string(),
    }
}

fn parse_api_command(args: &mut impl Iterator<Item = String>) -> Result<Command, String> {
    let Some(subcommand) = args.next() else {
        return Err("api にはサブコマンドが必要です (set)".to_string());
    };

    match subcommand.as_str() {
        "set" => {
            let mut api_key = None;
            let mut model = None;

            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--key" => {
                        let Some(value) = args.next() else {
                            return Err("--key にはAPIキーが必要です".to_string());
                        };
                        api_key = Some(value);
                    }
                    "--model" => {
                        let Some(value) = args.next() else {
                            return Err("--model にはモデル名が必要です".to_string());
                        };
                        model = Some(value);
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

            Ok(Command::ApiSet { api_key, model })
        }
        _ => Err("api のサブコマンドは set のみです".to_string()),
    }
}

fn print_usage() {
    eprintln!("使い方 / Usage:");
    eprintln!("  flowcloze [--json] [-o output.json] <markdown-file>");
    eprintln!("  flowcloze generate [-o output.json] [--model model] <markdown-file>");
    eprintln!("  flowcloze validate <intermediate.json> <generated.json>");
    eprintln!("  flowcloze view <generated.json>");
    eprintln!("  flowcloze csv [-o output.csv] <generated.json>");
    eprintln!("  flowcloze pdf [-o output.pdf] [--template template.typ] <generated.json>");
    eprintln!("  flowcloze api set --key <api_key> [--model model]");
}

fn print_help() {
    print_usage();
    eprintln!("\nコマンド / Commands:");
    eprintln!(
        "  (default)              Markdownを解析して概要を表示します / Parse markdown summary"
    );
    eprintln!("  generate               Geminiで問題文JSONを生成します / Generate questions JSON");
    eprintln!("  validate               中間JSONと生成JSONを検証します / Validate JSON pairs");
    eprintln!("  view                   生成JSONをTUIで表示します / View generated JSON in TUI");
    eprintln!("  csv                    生成JSONからAnkilot用CSVを作成します / Export Ankilot CSV");
    eprintln!("  pdf                    生成JSONからPDFを作成します / Build PDF from JSON");
    eprintln!("  api set                APIキーを.envに保存します / Save API key to .env");
    eprintln!("\nオプション / Options:");
    eprintln!("  --json                 中間JSONを出力します / Output intermediate JSON");
    eprintln!("  -s                     追加制約の入力をスキップします / Skip extra constraints");
    eprintln!("  -o, --output <path>     出力先を指定します / Set output path");
    eprintln!(
        "  --model <model>         generateで使うGeminiモデルを指定します / Model for generate"
    );
    eprintln!(
        "  --template <path>       pdfのTypstテンプレートを指定します / Typst template for pdf"
    );
    eprintln!("  -h, --help              ヘルプを表示します / Show help");
    eprintln!("  -V, --version           バージョンを表示します / Show version");
}

fn print_version() {
    println!("flowcloze {}", env!("CARGO_PKG_VERSION"));
}

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

fn save_api_settings(api_key: &str, model: Option<&str>) -> Result<(), String> {
    let path = ".env";
    let existing = fs::read_to_string(path).unwrap_or_default();
    let mut lines = Vec::new();
    let mut has_key = false;
    let mut has_model = false;

    for line in existing.lines() {
        if line.trim_start().starts_with("GEMINI_API_KEY=") {
            lines.push(format!("GEMINI_API_KEY={api_key}"));
            has_key = true;
            continue;
        }
        if line.trim_start().starts_with("GEMINI_MODEL=") {
            if let Some(model) = model {
                lines.push(format!("GEMINI_MODEL={model}"));
            } else {
                lines.push(line.to_string());
            }
            has_model = true;
            continue;
        }
        lines.push(line.to_string());
    }

    if !has_key {
        lines.push(format!("GEMINI_API_KEY={api_key}"));
    }
    if let Some(model) = model {
        if !has_model {
            lines.push(format!("GEMINI_MODEL={model}"));
        }
    }

    let mut contents = lines.join("\n");
    if !contents.ends_with('\n') {
        contents.push('\n');
    }

    fs::write(path, contents).map_err(|error| format!("{path} を書き込めませんでした: {error}"))
}

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

fn generate_with_gemini(
    input_path: &str,
    output_path: Option<&str>,
    model: Option<&str>,
    skip_constraints: bool,
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
    let mut prompt = match build_generation_prompt(&intermediate) {
        Ok(prompt) => prompt,
        Err(error) => {
            eprintln!("プロンプト生成に失敗しました: {error}");
            process::exit(1);
        }
    };
    let extra_constraints = if skip_constraints {
        Vec::new()
    } else {
        read_additional_constraints()
    };
    if !extra_constraints.is_empty() {
        prompt.push_str("\n\n追加制約:\n");
        for constraint in extra_constraints {
            prompt.push_str("- ");
            prompt.push_str(&constraint);
            prompt.push('\n');
        }
    }
    let api_key = match env::var("GEMINI_API_KEY") {
        Ok(api_key) if !api_key.trim().is_empty() => api_key,
        _ => {
            eprintln!("GEMINI_API_KEY が未設定です．.env または環境変数に設定してください．");
            process::exit(1);
        }
    };
    let model = model
        .map(str::to_string)
        .or_else(|| env::var("GEMINI_MODEL").ok())
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let client = GeminiClient::new(api_key, model);
    eprintln!("問題文を生成中です．しばらくお待ち下さい....");
    let _ = io::stderr().flush();
    let mut generated_json = None;
    let mut last_validation_feedback = Vec::new();
    for attempt in 1..=MAX_GENERATION_ATTEMPTS {
        let attempt_prompt = if attempt == 1 {
            prompt.clone()
        } else {
            format!(
                "{prompt}\n\n前回の出力は検証に失敗しました．次のエラーを修正し，JSONのみを再出力してください．\n- {}\n",
                last_validation_feedback.join("\n- ")
            )
        };
        let candidate_json = match client.generate_text(&attempt_prompt) {
            Ok(json) => json,
            Err(error) => {
                eprintln!("{error}");
                process::exit(1);
            }
        };
        let parsed_document = match serde_json::from_str::<GeneratedDocument>(&candidate_json) {
            Ok(document) => document,
            Err(error) => {
                last_validation_feedback = vec![format!("生成結果JSONを読めません: {error}")];
                for error in &last_validation_feedback {
                    eprintln!("validation error ({attempt}/{MAX_GENERATION_ATTEMPTS}): {error}");
                }
                continue;
            }
        };
        let report = validate_generated_document(&intermediate_json, &parsed_document);
        if report.is_valid() {
            let json = match serde_json::to_string_pretty(&parsed_document) {
                Ok(json) => json,
                Err(error) => {
                    eprintln!("生成結果JSONへの変換に失敗しました: {error}");
                    process::exit(1);
                }
            };
            generated_json = Some(json);
            break;
        }

        let validation_errors = report.errors;
        for error in &validation_errors {
            eprintln!("validation error ({attempt}/{MAX_GENERATION_ATTEMPTS}): {error}");
        }
        last_validation_feedback =
            build_validation_retry_feedback(&intermediate, &validation_errors);
    }
    let Some(generated_json) = generated_json else {
        eprintln!(
            "Geminiの生成結果が{MAX_GENERATION_ATTEMPTS}回連続で検証に失敗したため保存しませんでした．"
        );
        process::exit(1);
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

fn build_validation_retry_feedback(
    intermediate: &IntermediateDocument,
    errors: &[ValidationError],
) -> Vec<String> {
    let mut feedback = errors.iter().map(ToString::to_string).collect::<Vec<_>>();
    let mut described_ids = Vec::new();

    for error in errors {
        let Some(id) = validation_error_id(error) else {
            continue;
        };
        if described_ids.iter().any(|described_id| described_id == id) {
            continue;
        }
        let Some(qblock) = intermediate.qblocks.iter().find(|qblock| qblock.id == id) else {
            continue;
        };
        let answers = qblock
            .targets
            .iter()
            .map(|target| format!("\"{}\"", target.answer))
            .collect::<Vec<_>>()
            .join(", ");
        feedback.push(format!(
            "{id}: source_textをそのまま抜き出すのではなく，文脈を保った文章補完問題として自然な本文に再構成してください．target以外の説明は省略せず通常文として残してください．question内の ＿＿＿ は{}個にし，answersはこの順序の配列 [{answers}] にしてください．各answerを文中に残さず，必ず独立した空欄にしてください．",
            qblock.targets.len()
        ));
        described_ids.push(id.to_string());
    }

    feedback
}

fn validation_error_id(error: &ValidationError) -> Option<&str> {
    match error {
        ValidationError::EmptyQuestion { id }
        | ValidationError::DuplicateQuestionId { id }
        | ValidationError::UnknownQuestionId { id }
        | ValidationError::BlankAnswerCountMismatch { id, .. }
        | ValidationError::AnswerNotInTargets { id, .. }
        | ValidationError::MissingTargetAnswer { id, .. } => Some(id),
        ValidationError::InvalidIntermediateJson(_) | ValidationError::InvalidGeneratedJson(_) => {
            None
        }
    }
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
