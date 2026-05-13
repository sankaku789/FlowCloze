use std::env;
use std::fs;
use std::process;

use flowcloze::{
    build_generation_prompt, parse_markdown, to_intermediate_yaml, validate_generated_yaml,
    GeminiClient, IntermediateDocument,
};

fn main() {
    let _ = dotenvy::dotenv();

    let args = match Args::parse(env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("使い方:");
            eprintln!("  flowcloze [--yaml] [-o output.yaml] <markdown-file>");
            eprintln!("  flowcloze generate [-o output.yaml] [--model model] <markdown-file>");
            eprintln!("  flowcloze validate <intermediate.yaml> <generated.yaml>");
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

    if args.yaml {
        let yaml = match to_intermediate_yaml(&args.input_path, &qblocks) {
            Ok(yaml) => yaml,
            Err(error) => {
                eprintln!("YAMLへの変換に失敗しました: {error}");
                process::exit(1);
            }
        };

        if let Some(output_path) = args.output_path {
            if let Err(error) = fs::write(&output_path, yaml) {
                eprintln!("{output_path} へ書き込めませんでした: {error}");
                process::exit(1);
            }
        } else {
            print!("{yaml}");
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
    yaml: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Parse,
    Generate {
        model: Option<String>,
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
        let mut yaml = false;
        let mut command = Command::Parse;
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "generate" if input_path.is_none() && matches!(command, Command::Parse) => {
                    command = Command::Generate { model: None };
                }
                "validate" if input_path.is_none() => {
                    let Some(intermediate_path) = args.next() else {
                        return Err("validateには中間YAMLパスが必要です".to_string());
                    };
                    let Some(generated_path) = args.next() else {
                        return Err("validateには生成結果YAMLパスが必要です".to_string());
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
                        yaml: false,
                    });
                }
                "--yaml" => yaml = true,
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
                "-o" | "--output" => {
                    let Some(path) = args.next() else {
                        return Err(format!("{arg} には出力先パスが必要です"));
                    };
                    output_path = Some(path);
                    yaml = true;
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

        Ok(Self {
            command,
            input_path,
            output_path,
            yaml,
        })
    }
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
    let intermediate_yaml = match serde_yaml::to_string(&intermediate) {
        Ok(yaml) => yaml,
        Err(error) => {
            eprintln!("中間YAMLへの変換に失敗しました: {error}");
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
    let generated_yaml = match client.generate_text(&prompt) {
        Ok(yaml) => yaml,
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    };
    let report = validate_generated_yaml(&intermediate_yaml, &generated_yaml);
    if !report.is_valid() {
        for error in report.errors {
            eprintln!("validation error: {error}");
        }
        eprintln!("Geminiの生成結果が検証に失敗したため保存しませんでした。");
        process::exit(1);
    }

    if let Some(output_path) = output_path {
        if let Err(error) = fs::write(output_path, generated_yaml) {
            eprintln!("{output_path} へ書き込めませんでした: {error}");
            process::exit(1);
        }
    } else {
        print!("{generated_yaml}");
    }
}

fn validate_files(intermediate_path: &str, generated_path: &str) {
    let intermediate_yaml = match fs::read_to_string(intermediate_path) {
        Ok(yaml) => yaml,
        Err(error) => {
            eprintln!("{intermediate_path} を読めませんでした: {error}");
            process::exit(1);
        }
    };
    let generated_yaml = match fs::read_to_string(generated_path) {
        Ok(yaml) => yaml,
        Err(error) => {
            eprintln!("{generated_path} を読めませんでした: {error}");
            process::exit(1);
        }
    };
    let report = validate_generated_yaml(&intermediate_yaml, &generated_yaml);
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
