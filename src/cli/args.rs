#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Args {
    pub command: Command,
    pub input_path: Option<String>,
    pub output_path: Option<String>,
    pub json: bool,
    pub skip_constraints: bool,
    pub batch_policy: Option<BatchPolicyOverride>,
    pub model: Option<String>,
    pub fallback: Option<String>,
    pub verbose: bool,
    pub offline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Command {
    Help,
    Version,
    AuthAdd {
        provider: String,
    },
    ModelList,
    ModelAdd {
        name: String,
        provider: String,
        provider_model: String,
    },
    ProviderCheck {
        provider: String,
    },
    View {
        generated_path: String,
    },
    Csv,
    Parse,
    InspectScaffold,
    Generate,
    Plan,
    Pdf {
        template_path: String,
    },
    Validate {
        intermediate_path: String,
        generated_path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BatchPolicyOverride {
    Auto,
    Small,
    OneTask,
}

impl Args {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut input_path = None;
        let mut output_path = None;
        let mut json = false;
        let mut skip_constraints = false;
        let mut batch_policy = None;
        let mut model = None;
        let mut fallback = None;
        let mut verbose = false;
        let mut offline = false;
        let mut command = Command::Parse;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => command = Command::Help,
                "--version" | "-V" => command = Command::Version,
                "help" => return Err("help はオプションで指定してください (--help)".into()),
                "version" => {
                    return Err("version はオプションで指定してください (--version)".into())
                }
                "view" if input_path.is_none() && matches!(command, Command::Parse) => {
                    let generated_path = args
                        .next()
                        .ok_or_else(|| "viewには生成結果JSONパスが必要です".to_string())?;
                    if args.next().is_some() {
                        return Err("viewの引数が多すぎます".into());
                    }
                    return Ok(command_args(
                        Command::View { generated_path },
                        skip_constraints,
                        verbose,
                        offline,
                    ));
                }
                "auth" if input_path.is_none() && matches!(command, Command::Parse) => {
                    return Ok(command_args(
                        parse_auth_command(&mut args)?,
                        skip_constraints,
                        verbose,
                        offline,
                    ));
                }
                "model" if input_path.is_none() && matches!(command, Command::Parse) => {
                    return Ok(command_args(
                        parse_model_command(&mut args)?,
                        skip_constraints,
                        verbose,
                        offline,
                    ));
                }
                "provider" if input_path.is_none() && matches!(command, Command::Parse) => {
                    return Ok(command_args(
                        parse_provider_command(&mut args)?,
                        skip_constraints,
                        verbose,
                        offline,
                    ));
                }
                "generate" if input_path.is_none() && matches!(command, Command::Parse) => {
                    command = Command::Generate
                }
                "plan" if input_path.is_none() && matches!(command, Command::Parse) => {
                    command = Command::Plan
                }
                "inspect-scaffold" if input_path.is_none() && matches!(command, Command::Parse) => {
                    command = Command::InspectScaffold
                }
                "csv" if input_path.is_none() && matches!(command, Command::Parse) => {
                    command = Command::Csv
                }
                "pdf" if input_path.is_none() && matches!(command, Command::Parse) => {
                    command = Command::Pdf {
                        template_path: "templates/cloze.typ".into(),
                    }
                }
                "validate" if input_path.is_none() => {
                    let intermediate_path = args
                        .next()
                        .ok_or_else(|| "validateには中間JSONパスが必要です".to_string())?;
                    let generated_path = args
                        .next()
                        .ok_or_else(|| "validateには生成結果JSONパスが必要です".to_string())?;
                    if args.next().is_some() {
                        return Err("validateの引数が多すぎます".into());
                    }
                    return Ok(command_args(
                        Command::Validate {
                            intermediate_path,
                            generated_path,
                        },
                        skip_constraints,
                        verbose,
                        offline,
                    ));
                }
                "--json" => json = true,
                "--verbose" => verbose = true,
                "--offline" => {
                    if !matches!(command, Command::Generate | Command::Plan) {
                        return Err("--offline はgenerate/planコマンドでのみ使えます".into());
                    }
                    offline = true;
                }
                "-s" | "--skip-constraints" => skip_constraints = true,
                "--batch" => {
                    let value = args.next().ok_or_else(|| {
                        "--batch には auto, small, one-task のいずれかが必要です".to_string()
                    })?;
                    if !matches!(command, Command::Generate | Command::Plan) {
                        return Err("--batch はgenerate/planコマンドでのみ使えます".into());
                    }
                    batch_policy = Some(parse_batch_policy_override(&value)?);
                }
                "--model" => {
                    model = Some(
                        args.next()
                            .ok_or_else(|| "--model には値が必要です".to_string())?,
                    )
                }
                "--fallback" => {
                    fallback = Some(args.next().ok_or_else(|| {
                        "--fallback には error, draft のいずれかが必要です".to_string()
                    })?)
                }
                "--template" => {
                    let path = args.next().ok_or_else(|| {
                        "--template にはTypstテンプレートのパスが必要です".to_string()
                    })?;
                    match &mut command {
                        Command::Pdf { template_path } => *template_path = path,
                        _ => return Err("--template はpdfコマンドでのみ使えます".into()),
                    }
                }
                "-o" | "--output" => {
                    output_path = Some(
                        args.next()
                            .ok_or_else(|| format!("{arg} には出力先パスが必要です"))?,
                    )
                }
                _ if arg.starts_with("-s") => return Err("-s は単独で指定してください".into()),
                _ if arg.starts_with('-') => return Err(format!("未知のオプションです: {arg}")),
                _ => {
                    if matches!(command, Command::Help | Command::Version) {
                        return Err("help/version には追加引数を指定できません".into());
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
                Command::Parse | Command::Generate | Command::Plan | Command::InspectScaffold => {
                    return Err("入力Markdownファイルを指定してください".into())
                }
                Command::Csv => return Err("csvには生成結果JSONパスが必要です".into()),
                Command::Pdf { .. } => return Err("pdfには生成結果JSONパスが必要です".into()),
                _ => {}
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
            fallback,
            verbose,
            offline,
        })
    }
}

fn command_args(command: Command, skip_constraints: bool, verbose: bool, offline: bool) -> Args {
    Args {
        command,
        input_path: None,
        output_path: None,
        json: false,
        skip_constraints,
        batch_policy: None,
        model: None,
        fallback: None,
        verbose,
        offline,
    }
}

fn parse_auth_command(args: &mut impl Iterator<Item = String>) -> Result<Command, String> {
    if args.next().as_deref() != Some("add") {
        return Err("auth には add サブコマンドが必要です".into());
    }
    let provider = args
        .next()
        .ok_or_else(|| "auth add にはprovider IDが必要です".to_string())?;
    if args.next().is_some() {
        return Err("auth add の引数が多すぎます".into());
    }
    Ok(Command::AuthAdd { provider })
}

fn parse_model_command(args: &mut impl Iterator<Item = String>) -> Result<Command, String> {
    match args.next().as_deref() {
        Some("list") => {
            if args.next().is_some() {
                Err("model list は引数なしで実行してください".into())
            } else {
                Ok(Command::ModelList)
            }
        }
        Some("add") => {
            let name = args
                .next()
                .ok_or_else(|| "model add にはprofile名が必要です".to_string())?;
            let (mut provider, mut provider_model) = (None, None);
            while let Some(option) = args.next() {
                match option.as_str() {
                    "--provider" => provider = args.next(),
                    "--model" => provider_model = args.next(),
                    _ => return Err(format!("未知のmodel addオプションです: {option}")),
                }
            }
            Ok(Command::ModelAdd {
                name,
                provider: provider
                    .ok_or_else(|| "model add には--provider <id>が必要です".to_string())?,
                provider_model: provider_model.ok_or_else(|| {
                    "model add には--model <provider-model>が必要です".to_string()
                })?,
            })
        }
        _ => Err("model には list または add サブコマンドが必要です".into()),
    }
}

fn parse_provider_command(args: &mut impl Iterator<Item = String>) -> Result<Command, String> {
    if args.next().as_deref() != Some("check") {
        return Err("provider には check サブコマンドが必要です".into());
    }
    let provider = args
        .next()
        .ok_or_else(|| "provider check にはprovider IDが必要です".to_string())?;
    if args.next().is_some() {
        return Err("provider check の引数が多すぎます".into());
    }
    Ok(Command::ProviderCheck { provider })
}

fn duplicate_input_error(command: &Command) -> String {
    match command {
        Command::Csv | Command::Pdf { .. } => "生成結果JSONファイルは1つだけ指定してください",
        _ => "入力Markdownファイルは1つだけ指定してください",
    }
    .into()
}

pub(super) fn batch_name(batch: &BatchPolicyOverride) -> String {
    match batch {
        BatchPolicyOverride::Auto => "auto",
        BatchPolicyOverride::Small => "small",
        BatchPolicyOverride::OneTask => "one-task",
    }
    .into()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(values: &[&str]) -> Result<Args, String> {
        Args::parse(values.iter().map(|value| (*value).to_string()))
    }

    #[test]
    fn legacy_provider_option_is_rejected() {
        assert!(parse(&["generate", "--provider", "google", "notes.md"]).is_err());
    }
    #[test]
    fn plan_accepts_model_and_batch_without_output() {
        let parsed = parse(&[
            "plan",
            "--model",
            "gemini-flash",
            "--batch",
            "auto",
            "notes.md",
        ])
        .unwrap();
        assert_eq!(parsed.command, Command::Plan);
        assert_eq!(parsed.batch_policy, Some(BatchPolicyOverride::Auto));
        assert_eq!(parsed.input_path.as_deref(), Some("notes.md"));
    }
    #[test]
    fn legacy_api_set_is_rejected() {
        assert!(parse(&["api", "set"]).is_err());
    }
    #[test]
    fn catalog_commands_are_parsed() {
        assert_eq!(
            parse(&["auth", "add", "google"]).unwrap().command,
            Command::AuthAdd {
                provider: "google".into()
            }
        );
        assert_eq!(
            parse(&["model", "list"]).unwrap().command,
            Command::ModelList
        );
        assert_eq!(
            parse(&[
                "model",
                "add",
                "qwen",
                "--provider",
                "ollama",
                "--model",
                "qwen3",
            ])
            .unwrap()
            .command,
            Command::ModelAdd {
                name: "qwen".into(),
                provider: "ollama".into(),
                provider_model: "qwen3".into(),
            }
        );
        assert_eq!(
            parse(&["provider", "check", "ollama"]).unwrap().command,
            Command::ProviderCheck {
                provider: "ollama".into()
            }
        );
    }
    #[test]
    fn generate_accepts_model_profile_and_offline() {
        let parsed =
            parse(&["generate", "--model", "local-qwen", "--offline", "notes.md"]).unwrap();
        assert_eq!(parsed.model.as_deref(), Some("local-qwen"));
        assert!(parsed.offline);
    }
}
