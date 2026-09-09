//! FlowCloze CLIのdispatch入口。

use std::{env, fs, process};

use flowcloze::{
    CliOverrides, FailureClass, GenerationConfig, LabeledProgressSink, ProgressEvent, ProgressSink,
    ProgressStage,
};

mod args;
mod commands;

use args::{batch_name, Args, Command};

pub fn run() {
    let args = Args::parse(env::args().skip(1)).unwrap_or_else(|message| {
        eprintln!("{message}");
        print_usage();
        process::exit(2)
    });
    match &args.command {
        Command::Help => print_help(),
        Command::Version => println!("flowcloze {}", env!("CARGO_PKG_VERSION")),
        Command::AuthAdd { provider } => result(commands::auth::run(provider)),
        Command::ModelList => result(commands::model::list()),
        Command::ModelAdd {
            name,
            provider,
            provider_model,
        } => result(commands::model::add(name, provider, provider_model)),
        Command::ProviderCheck { provider } => result(commands::provider::check(provider)),
        Command::View { generated_path } => commands::export::view(generated_path),
        Command::Csv => {
            commands::export::csv(required(&args.input_path), args.output_path.as_deref())
        }
        Command::Pdf { template_path } => commands::export::pdf(
            required(&args.input_path),
            args.output_path.as_deref(),
            template_path,
        ),
        Command::Validate {
            intermediate_path,
            generated_path,
        } => commands::validate::run(intermediate_path, generated_path),
        Command::Plan => {
            let config = load_config(&args).unwrap_or_else(|e| {
                eprintln!("{e}");
                process::exit(2)
            });
            commands::plan::run(required(&args.input_path), &config);
        }
        Command::Generate => {
            let progress = LabeledProgressSink::stderr("Generate");
            let config = load_config(&args).unwrap_or_else(|e| {
                progress.emit(ProgressEvent::Failed {
                    stage: ProgressStage::Config,
                    class: FailureClass::Configuration,
                });
                eprintln!("{e}");
                process::exit(2)
            });
            progress.set_label(if config.offline {
                "Identity"
            } else {
                "Provider"
            });
            commands::generate::run(
                required(&args.input_path),
                args.output_path.as_deref(),
                &config,
                args.skip_constraints,
                args.verbose,
                &progress,
            );
        }
        Command::InspectScaffold => commands::generate::inspect_scaffold(
            required(&args.input_path),
            args.output_path.as_deref(),
        ),
        Command::Parse => parse(
            required(&args.input_path),
            args.output_path.as_deref(),
            args.json,
        ),
    }
}

fn required(path: &Option<String>) -> &str {
    path.as_deref().expect("parser ensures an input path")
}

fn result(value: Result<(), String>) {
    if let Err(error) = value {
        eprintln!("{error}");
        process::exit(1);
    }
}

fn load_config(args: &Args) -> Result<GenerationConfig, String> {
    flowcloze::config::load(CliOverrides {
        model: args.model.clone(),
        fallback: args.fallback.clone(),
        batch: args.batch_policy.as_ref().map(batch_name),
        offline: args.offline,
    })
}

fn parse(input_path: &str, output_path: Option<&str>, json: bool) {
    let markdown = fs::read_to_string(input_path).unwrap_or_else(|e| {
        eprintln!("{input_path} を読めませんでした: {e}");
        process::exit(1)
    });
    let qblocks = flowcloze::parse_markdown(&markdown).unwrap_or_else(|e| {
        eprintln!("Markdownの解析に失敗しました: {e}");
        process::exit(1)
    });
    if json {
        let body = flowcloze::to_intermediate_json(input_path, &qblocks).unwrap_or_else(|e| {
            eprintln!("JSONへの変換に失敗しました: {e}");
            process::exit(1)
        });
        if let Some(path) = output_path {
            fs::write(path, body).unwrap_or_else(|e| {
                eprintln!("{path} へ書き込めませんでした: {e}");
                process::exit(1)
            });
        } else {
            print!("{body}");
        }
    } else {
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
}

fn print_usage() {
    eprintln!("使い方 / Usage:");
    eprintln!("  flowcloze [--json] [-o output.json] <markdown-file>");
    eprintln!("  flowcloze generate [-o output.json] [--verbose] [--model profile] [--fallback error|draft] [--batch auto|small|one-task] [--offline] <markdown-file>");
    eprintln!("  flowcloze plan [--model profile] [--batch auto|small|one-task] [--offline] <markdown-file>");
    eprintln!("  flowcloze auth add <provider>");
    eprintln!("  flowcloze model list");
    eprintln!("  flowcloze model add <name> --provider <id> --model <provider-model>");
    eprintln!("  flowcloze provider check <id>");
    eprintln!("  flowcloze inspect-scaffold [-o scaffold.json] <markdown-file>");
    eprintln!("  flowcloze validate <intermediate.json> <generated.json>");
    eprintln!("  flowcloze view <generated.json>");
    eprintln!("  flowcloze csv [-o output.csv] <generated.json>");
    eprintln!("  flowcloze pdf [-o output.pdf] [--template template.typ] <generated.json>");
}

fn print_help() {
    print_usage();
    eprintln!("\nコマンド / Commands:");
    eprintln!(
        "  (default)              Markdownを解析して概要を表示します / Parse markdown summary"
    );
    eprintln!(
        "  generate               providerで問題文JSONを生成します / Generate questions JSON"
    );
    eprintln!("  plan                   APIへ送信せず、qblockのまとめ方を表示します / Show batch plan without API calls");
    eprintln!("  auth add <provider>     APIキーをauth.yamlへ保存します / Save an API key");
    eprintln!("  model list             利用可能なmodel profileを表示します / List model profiles");
    eprintln!("  model add ...          model.yamlへprofileを追加または上書きします / Upsert a model profile");
    eprintln!(
        "  provider check <id>    providerの到達性を確認します / Check provider reachability"
    );
    eprintln!("  inspect-scaffold       LLM入力用scaffoldを表示します / Inspect scaffold JSON");
    eprintln!("  validate               中間JSONと生成JSONを検証します / Validate JSON pairs");
    eprintln!("  view                   生成JSONをTUIで表示します / View generated JSON in TUI");
    eprintln!("  csv                    生成JSONからAnkilot用CSVを作成します / Export Ankilot CSV");
    eprintln!("  pdf                    生成JSONからPDFを作成します / Build PDF from JSON");
    eprintln!("\nMarkdown記法 / Markdown Syntax:");
    eprintln!("  #qblock{{ ... }}        問題化範囲を囲みます / Mark a question range");
    eprintln!("  [答え]                 解答対象を指定します / Mark an answer target");
    eprintln!("  [答え]{{type}}          任意で出題観点を指定します / Optional target type");
    eprintln!("\nオプション / Options:");
    eprintln!("  --json                 中間JSONを出力します / Output intermediate JSON");
    eprintln!("  -s                     追加制約の入力をスキップします / Skip extra constraints");
    eprintln!("  -o, --output <path>     出力先を指定します / Set output path");
    eprintln!("  --batch <policy>        generateのbatch policyを指定します(auto/small/one-task) / Batch policy");
    eprintln!("  --verbose               通常の進捗表示に観測JSON Linesをstderrへ追加します (FLOWCLOZE_LOG=debugでも有効)");
    eprintln!("  --offline               generate/planをIdentityのみで実行します / Disable provider calls");
    eprintln!("                           max_concurrent_batchesは検証・観測のみで、現在は並列実行しません");
    eprintln!(
        "  --template <path>       pdfのTypstテンプレートを指定します / Typst template for pdf"
    );
    eprintln!("  -h, --help              ヘルプを表示します / Show help");
    eprintln!("  -V, --version           バージョンを表示します / Show version");
}
