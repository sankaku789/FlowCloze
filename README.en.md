# FlowCloze

[日本語](README.md) | English

FlowCloze is a Rust CLI that generates context-cloze questions from study notes written in Markdown.
Wrap a question range with `#qblock{ ... }`, and mark answer targets with `[answer]` or `[answer]{type}`.

It supports Gemini, OpenAI-compatible local LLMs such as Ollama / LM Studio, and offline Identity generation without calling an LLM. The same CLI also validates generated output and exports it to TUI, PDF, and CSV formats.

![FlowCloze TUI](fig/image.png)

## Features

- Extract `qblock` ranges and targets from Markdown
- Emit intermediate JSON
- Generate questions with Gemini or an OpenAI-compatible local LLM
- Run offline Identity generation with `--rewrite never`
- Validate targets, answers, blanks, IDs, and ordering
- Inspect generated questions in a TUI
- Export PDF through Typst
- Export Ankilot-compatible CSV

```text
Markdown
  -> parse
  -> intermediate JSON
  -> compose / rewrite
  -> validate
  -> JSON
  -> TUI / PDF / CSV
```

## Requirements

Core:

- Rust / Cargo

Depending on the features you use:

- Gemini API key: for Gemini rewrite generation
- Ollama or LM Studio: for local LLM generation
- Typst CLI: for PDF output
- Japanese fonts: for Japanese text in PDF output

On Ubuntu / WSL, Noto CJK fonts are recommended for PDF output:

```bash
sudo apt update
sudo apt install -y fonts-noto-cjk
fc-cache -fv
```

Check whether Typst can see the font:

```bash
typst fonts | grep "Noto Sans CJK"
```

## Build / Install

```bash
git clone https://github.com/sankaku789/FlowCloze.git
cd FlowCloze
cargo build --release
```

Install `flowcloze` as a command:

```bash
cargo install --path .
flowcloze --version
```

The binary is usually installed to `~/.cargo/bin/flowcloze`.

For a temporary run without installing, use `cargo run -- ...`.

## Markdown Syntax

```md
# Software Engineering Overview

#qblock{
[QCD]{term-name} means [quality]{meaning}, [cost]{meaning}, and [delivery]{meaning}.
}
```

- `#qblock{ ... }`: range to turn into questions
- `[answer]`: answer target
- `[answer]{type}`: answer target with an optional question perspective

Common target types:

- `term-name`: term name
- `meaning`: meaning, definition, or property
- `process`: process, procedure, or action
- `relation`: structure, comparison, classification, or relation

## Basic Usage

### Parse Markdown

```bash
flowcloze sample/sample.md
```

Write intermediate JSON:

```bash
flowcloze --json -o sample/sample.json sample/sample.md
```

### Generate Questions

With Gemini:

```bash
flowcloze generate --provider gemini \
  -o sample/generated.json sample/sample.md
```

Without calling an LLM:

```bash
flowcloze generate --rewrite never \
  -o sample/generated.json sample/sample.md
```

With a local LLM:

```bash
flowcloze local check
flowcloze generate --provider local \
  -o sample/generated.json sample/sample.md
```

### Validate Output

```bash
flowcloze validate sample/sample.json sample/generated.json
```

### View in the TUI

```bash
flowcloze view sample/generated.json
```

### Build a PDF

```bash
flowcloze pdf -o sample/sample.pdf sample/generated.json
```

Set the default template with `typst_template` in `~/.config/flowcloze/config.toml`.
For a one-off override, use:

```bash
flowcloze pdf --template path/to/template.typ \
  -o sample/sample.pdf sample/generated.json
```

### Export CSV

```bash
flowcloze csv -o sample/sample.csv sample/generated.json
```

## Generation Settings

FlowCloze 2.1 keeps user-level settings in the standard config directory:

```text
~/.config/flowcloze/config.toml
~/.config/flowcloze/credentials.toml
```

When `XDG_CONFIG_HOME` is set, FlowCloze uses `$XDG_CONFIG_HOME/flowcloze/`. For development, you can isolate settings like this:

```bash
export XDG_CONFIG_HOME="$PWD/.dev-config"
```

Create the normal config:

```bash
mkdir -p ~/.config/flowcloze
cp config.toml.example ~/.config/flowcloze/config.toml
```

Store the Gemini API key in the dedicated `credentials.toml`, not in `config.toml`:

```bash
flowcloze api set --key "YOUR_GEMINI_API_KEY"
```

On Unix-like systems, FlowCloze creates `credentials.toml` with mode `0600` and the config directory with mode `0700`.

Example `config.toml`:

```toml
provider = "gemini"
model = "gemini-2.5-flash"
rewrite = "always"
fallback = "error"
structured_output = "auto"
batch = "auto"
typst_template = "/absolute/path/to/cloze.typ"
```

`typst_template` is the default template for PDF generation. `flowcloze pdf --template ...` overrides it for one invocation.

Main `generate` options:

```text
--provider gemini|local
--model <model>
--rewrite always|never|auto
--fallback error|draft
--structured-output auto|on|off
--batch auto|small|one-task
--verbose
-s, --skip-constraints
```

Example:

```bash
flowcloze generate \
  --provider gemini \
  --model gemini-2.5-flash \
  --rewrite auto \
  --fallback draft \
  --structured-output auto \
  --verbose \
  -o sample/generated.json \
  sample/sample.md
```

`rewrite`:

- `always`: rewrite through the selected provider
- `never`: use Identity generation without calling a provider
- `auto`: choose whether rewriting is needed from the input

`fallback`:

- `error`: return the failure as an error
- `draft`: fall back failed transport/content-validation tasks to Identity drafts

Settings resolve in this order: **CLI > `~/.config/flowcloze/config.toml` > built-in defaults**.
FlowCloze no longer automatically reads `.env`, a current-directory `config.toml`, or the legacy configuration environment variables.

## Local LLM

FlowCloze can use an OpenAI-compatible server from Ollama or LM Studio.

By default, it tries Ollama (`http://localhost:11434/v1`) first and then LM Studio (`http://localhost:1234/v1`) if Ollama is unavailable.
Set `FLOWCLOZE_BASE_URL` to choose the endpoint explicitly.

The default local model is `gemma4:e2b-it-qat`.

With Ollama:

```bash
ollama pull gemma4:e2b-it-qat
flowcloze local check
```

With LM Studio, load the same model, start Local Server, and then run `flowcloze local check`.

## Inspect the Scaffold

Inspect the scaffold JSON sent to the LLM:

```bash
flowcloze inspect-scaffold sample/sample.md
```

Save it to a file:

```bash
flowcloze inspect-scaffold \
  -o sample/scaffold.json sample/sample.md
```

## Logging / Observability

`generate` writes parse, batch, validation, and save progress to stderr.

With `--verbose` or `FLOWCLOZE_LOG=debug`, it also emits observability JSON Lines to stderr. Markdown bodies, prompts, provider responses, and credentials are not included in the logs.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Also check the Gemini native adapter:

```bash
cargo clippy --all-targets --features gemini-native -- -D warnings
cargo test --features gemini-native
```

## Editor Support

`editors/vscode-flowcloze-syntax` contains a small VS Code extension for highlighting `#qblock`, `[answer]`, and `[answer]{type}`.

VS Code on WSL:

```bash
mkdir -p ~/.vscode-server/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" \
  ~/.vscode-server/extensions/flowcloze.flowcloze-syntax-0.0.1
```

Non-WSL Linux:

```bash
mkdir -p ~/.vscode/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" \
  ~/.vscode/extensions/flowcloze.flowcloze-syntax-0.0.1
```

Then run `Developer: Reload Window` in VS Code.

## Repository Layout

```text
src/parser.rs          Markdown parser
src/json.rs            intermediate JSON
src/planner.rs         generation planning
src/compose.rs         question composition core
src/orchestration.rs   generation orchestration
src/config.rs          configuration resolution
src/gemini.rs          Gemini adapter
src/local_openai.rs    local OpenAI-compatible adapter
src/validation.rs      generated JSON validation
src/observability.rs   structured events / logging
src/csv.rs             Ankilot CSV export
src/pdf.rs             Typst PDF adapter
src/main.rs            CLI entry point
templates/             Typst templates
sample/                sample inputs / outputs
editors/               editor support
tests/                 integration tests
```

## License

Licensed under either Apache License, Version 2.0 or the MIT License, at your option.
