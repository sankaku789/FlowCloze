from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count == 0:
        if new in text:
            return text
        raise SystemExit(f"expected pattern not found: {label}")
    if count != 1:
        raise SystemExit(f"expected one pattern for {label}, found {count}")
    return text.replace(old, new, 1)


config_path = Path("src/config.rs")
config = config_path.read_text()
config = replace_once(
    config,
    "pub enum Provider {\n    Gemini,\n    Local,\n}",
    "pub enum Provider {\n    Gemini,\n    OpenAiCompatible,\n}",
    "Provider enum",
)
config = config.replace("Provider::Local", "Provider::OpenAiCompatible")
config = replace_once(
    config,
    '        "local" => Ok(Provider::OpenAiCompatible),\n        _ => Err("provider は gemini または local を指定してください".into()),',
    '        "openai-compatible" | "local" => Ok(Provider::OpenAiCompatible),\n        _ => Err("provider は gemini または openai-compatible (local) を指定してください".into()),',
    "provider parser alias",
)
config = replace_once(
    config,
    '        assert_eq!(parse_provider("local").unwrap(), Provider::OpenAiCompatible);\n        assert!(parse_provider("openai").is_err());',
    '        assert_eq!(parse_provider("local").unwrap(), Provider::OpenAiCompatible);\n        assert_eq!(\n            parse_provider("openai-compatible").unwrap(),\n            Provider::OpenAiCompatible\n        );\n        assert!(parse_provider("openai").is_err());',
    "provider parser test",
)
config_path.write_text(config)

main_path = Path("src/main.rs")
main = main_path.read_text()
main = main.replace("Provider::Local", "Provider::OpenAiCompatible")
main = replace_once(
    main,
    "    JsonLinesEventSink, PdfOptions, PlainProgressSink, ProgressEvent, ProgressSink, ProgressStage,\n    Provider, RewritePolicy, RunContext,",
    "    JsonLinesEventSink, OpenAiCompatiblePool, PdfOptions, PlainProgressSink, ProgressEvent,\n    ProgressSink, ProgressStage, Provider, RewritePolicy, RunContext,",
    "main import",
)
main = replace_once(
    main,
    '''            Provider::OpenAiCompatible => {\n                let adapter = LocalCandidateComposer {\n                    adapters: flowcloze::local_openai_url_candidates(config.base_url.as_deref())\n                        .into_iter()\n                        .map(|url| {\n                            flowcloze::OpenAiCompatibleAdapter::new(\n                                url,\n                                config.model.clone(),\n                                std::env::var(&config.api_key_env).ok(),\n                            )\n                            .with_structured_output(config.structured_output)\n                            .with_transport(retry_transport.clone())\n                        })\n                        .collect(),\n                };\n                flowcloze::generate_markdown_with_composer_observed_with_progress(\n                    &markdown, options, &adapter, &context, &*sink, progress,\n                )\n            }''',
    '''            Provider::OpenAiCompatible => {\n                let adapter = OpenAiCompatiblePool::from_candidates(\n                    config.base_url.as_deref(),\n                    config.model.clone(),\n                    env::var(&config.api_key_env).ok(),\n                )\n                .with_structured_output(config.structured_output)\n                .with_transport(retry_transport.clone());\n                flowcloze::generate_markdown_with_composer_observed_with_progress(\n                    &markdown, options, &adapter, &context, &*sink, progress,\n                )\n            }''',
    "OpenAI-compatible provider construction",
)
main = replace_once(
    main,
    '''/// Localの既定候補だけを接続系失敗時に順送りする。\nstruct LocalCandidateComposer {\n    adapters: Vec<flowcloze::OpenAiCompatibleAdapter>,\n}\n\nimpl flowcloze::QuestionComposer for LocalCandidateComposer {\n    fn compose(\n        &self,\n        request: &flowcloze::ComposeBatchRequest,\n    ) -> Result<flowcloze::ComposeBatchOutput, flowcloze::ComposeError> {\n        for (index, adapter) in self.adapters.iter().enumerate() {\n            match adapter.compose(request) {\n                Ok(output) => return Ok(output),\n                Err(\n                    error @ (flowcloze::ComposeError::Transport | flowcloze::ComposeError::Timeout),\n                ) if index + 1 < self.adapters.len() => {\n                    let _ = error;\n                }\n                Err(error) => return Err(error),\n            }\n        }\n        Err(flowcloze::ComposeError::Transport)\n    }\n}\n\n''',
    "",
    "LocalCandidateComposer",
)
main_path.write_text(main)
