use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;

use flowcloze::{
    build_generation_prompt, compile_pdf, default_pdf_output_path, parse_markdown,
    to_intermediate_json, validate_generated_document, validate_generated_json, GeminiClient,
    GeneratedDocument, IntermediateDocument, PdfOptions,
};

const MAX_GENERATION_ATTEMPTS: u32 = 3;

fn main() {
    let _ = dotenvy::dotenv();

    let args = match Args::parse(env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("使い方:");
            eprintln!("  flowcloze [--json] [-o output.json] <markdown-file>");
            eprintln!("  flowcloze generate [-o output.json] [--model model] <markdown-file>");
            eprintln!("  flowcloze validate <intermediate.json> <generated.json>");
            eprintln!("  flowcloze pdf [-o output.pdf] [--template template.typ] <generated.json>");
            process::exit(2);
        }
    };

    match &args.command {
        Command::Validate {
            intermediate_path,
            generated_path,
        } => {
            validate_files(intermediate_path, generated_path);
            return;
        }
        Command::Generate { model } => {
            generate_with_gemini(
                &args.input_path,
                args.output_path.as_deref(),
                model.as_deref(),
            );
            return;
        }
        Command::Pdf { template_path } => {
            compile_pdf_file(&args.input_path, args.output_path.as_deref(), template_path);
            return;
        }
        Command::Parse => {}
    }

    let markdown = match fs::read_to_string(&args.input_path) {
        Ok(markdown) => markdown,
        Err(error) => {
            eprintln!("{} を読めませんでした: {error}", args.input_path);
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
        let json = match to_intermediate_json(&args.input_path, &qblocks) {
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
    input_path: String,
    output_path: Option<String>,
    json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
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
        let mut command = Command::Parse;
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "generate" if input_path.is_none() && matches!(command, Command::Parse) => {
                    command = Command::Generate { model: None };
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
                        input_path: String::new(),
                        output_path: None,
                        json: false,
                    });
                }
                "--json" => json = true,
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
                _ if arg.starts_with('-') => return Err(format!("未知のオプションです: {arg}")),
                _ => {
                    if input_path.is_some() {
                        return Err("入力Markdownファイルは1つだけ指定してください".to_string());
                    }
                    input_path = Some(arg);
                }
            }
        }

        let Some(input_path) = input_path else {
            return Err("入力Markdownファイルを指定してください".to_string());
        };

        if output_path.is_some() && matches!(command, Command::Parse) {
            json = true;
        }

        Ok(Self {
            command,
            input_path,
            output_path,
            json,
        })
    }
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

fn generate_with_gemini(input_path: &str, output_path: Option<&str>, model: Option<&str>) {
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
    let prompt = match build_generation_prompt(&intermediate) {
        Ok(prompt) => prompt,
        Err(error) => {
            eprintln!("プロンプト生成に失敗しました: {error}");
            process::exit(1);
        }
    };
    let api_key = match env::var("GEMINI_API_KEY") {
        Ok(api_key) if !api_key.trim().is_empty() => api_key,
        _ => {
            eprintln!("GEMINI_API_KEY が未設定です。.env または環境変数に設定してください。");
            process::exit(1);
        }
    };
    let model = model
        .map(str::to_string)
        .or_else(|| env::var("GEMINI_MODEL").ok())
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| "gemini-2.5-flash".to_string());
    let client = GeminiClient::new(api_key, model);
    let mut generated_json = None;
    let mut last_validation_errors = Vec::new();
    for attempt in 1..=MAX_GENERATION_ATTEMPTS {
        let attempt_prompt = if attempt == 1 {
            prompt.clone()
        } else {
            format!(
                "{prompt}\n\n前回の出力は検証に失敗しました。次のエラーを修正し，JSONのみを再出力してください。\n- {}\n",
                last_validation_errors.join("\n- ")
            )
        };
        let candidate_json = match client.generate_text(&attempt_prompt) {
            Ok(json) => json,
            Err(error) => {
                eprintln!("{error}");
                process::exit(1);
            }
        };
        let generated_document = match serde_json::from_str::<GeneratedDocument>(&candidate_json) {
            Ok(document) => document,
            Err(error) => {
                last_validation_errors = vec![format!("生成結果JSONを読めません: {error}")];
                for error in &last_validation_errors {
                    eprintln!("validation error ({attempt}/{MAX_GENERATION_ATTEMPTS}): {error}");
                }
                continue;
            }
        };
        let report = validate_generated_document(&intermediate_json, &generated_document);
        if report.is_valid() {
            let json = match serde_json::to_string_pretty(&generated_document) {
                Ok(json) => json,
                Err(error) => {
                    eprintln!("生成結果JSONへの変換に失敗しました: {error}");
                    process::exit(1);
                }
            };
            generated_json = Some(json);
            break;
        }

        last_validation_errors = report
            .errors
            .into_iter()
            .map(|error| error.to_string())
            .collect();
        for error in &last_validation_errors {
            eprintln!("validation error ({attempt}/{MAX_GENERATION_ATTEMPTS}): {error}");
        }
    }
    let Some(generated_json) = generated_json else {
        eprintln!(
            "Geminiの生成結果が{MAX_GENERATION_ATTEMPTS}回連続で検証に失敗したため保存しませんでした。"
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
        match &qblock.title {
            Some(title) => println!("{}: {}", qblock.id, title),
            None => println!("{}", qblock.id),
        }

        for target in qblock.targets {
            println!("  - {} ({})", target.answer, target.target_type);
        }

        for warning in qblock.warnings {
            println!("  warning: {warning}");
        }
    }
}
