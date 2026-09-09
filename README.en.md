# FlowCloze

[日本語](README.md) | English

FlowCloze is a Rust CLI that generates context-cloze questions from study notes written in Markdown. Wrap a question range in `#qblock{ ... }` and mark answer targets with `[answer]` or `[answer]{type}`.

It supports Google Gemini, OpenAI-compatible APIs such as Ollama, and offline Identity generation without calling an LLM. The same CLI validates generated output and exports it to TUI, PDF, and CSV formats.

![FlowCloze TUI](fig/image.png)

## Features

- Extract `qblock` ranges and targets from Markdown
- Emit intermediate JSON
- Generate questions through OpenAI-compatible APIs
- Run offline Identity generation with `--offline`
- Validate targets, answers, blanks, IDs, and ordering
- Inspect generated questions in the TUI
- Export PDF through Typst
- Export Ankilot-compatible CSV

```text
Markdown
  -> parse
  -> intermediate JSON
  -> compose
  -> validate
  -> JSON
  -> TUI / PDF / CSV
```

## Requirements

Core:

- Rust / Cargo

Depending on the features you use:

- Google API key: Gemini generation
- Ollama: local LLM generation
- Typst CLI: PDF output
- Japanese fonts: Japanese text in PDF output

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
cargo install --path . --force
flowcloze --version
```

The standard Typst template is embedded in the binary. When PDF output is used for the first time, FlowCloze materializes it at `~/.config/flowcloze/templates/cloze.typ` (or under `XDG_CONFIG_HOME`). Installation therefore does not depend on an OS- or shell-specific installer script.

The binary is usually installed at `~/.cargo/bin/flowcloze`.

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

Use the built-in `gemini-flash` model profile:

```bash
flowcloze auth add google
flowcloze generate --model gemini-flash \
  -o sample/generated.json sample/sample.md
```

`gemini-flash` connects to Google's `gemini-2.5-flash`. Every provider, including Google, is called through the OpenAI-compatible adapter.

Generate without calling an LLM:

```bash
flowcloze generate --offline \
  -o sample/generated.json sample/sample.md
```

Use Ollama:

```bash
flowcloze model add local-qwen --provider ollama --model qwen3:14b
flowcloze provider check ollama
flowcloze generate --model local-qwen \
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

The standard template is embedded in the binary and materialized at `~/.config/flowcloze/templates/cloze.typ` when PDF output is used. A configured `typst_template` overrides it.

For a one-off override, use:

```bash
flowcloze pdf --template path/to/template.typ \
  -o sample/sample.pdf sample/generated.json
```

### Export CSV

```bash
flowcloze csv -o sample/sample.csv sample/generated.json
```

## Inspect the Batch Plan Before API Calls

`plan` does not connect to a provider API. It shows qblock grouping produced by the same planner used by `generate`.

```bash
flowcloze plan --model gemini-flash sample/sample.md
```

The output includes qblock positions, estimated input/output sizes, blank counts, and heavy singleton qblocks.

To inspect a plan in which no qblocks are sent to an API, use:

```bash
flowcloze plan --offline sample/sample.md
```

In this mode, every qblock is listed as `identity (no API)` and no provider is initialized.

## Generation Settings

FlowCloze keeps user-level settings in the standard config directory:

```text
~/.config/flowcloze/
  config.yaml
  model.yaml
  auth.yaml
  templates/cloze.typ
```

When `XDG_CONFIG_HOME` is set, FlowCloze uses `$XDG_CONFIG_HOME/flowcloze/`. During development, settings can be isolated like this:

```bash
export XDG_CONFIG_HOME="$PWD/.dev-config"
```

File responsibilities:

- `config.yaml`: default model, generation, batch, quota, and Typst template settings
- `model.yaml`: provider catalog and model profiles
- `auth.yaml`: API keys only

Enter the Google API key without echoing it:

```bash
flowcloze auth add google
```

API keys are stored only in `auth.yaml`. On Unix-like systems, FlowCloze handles the config directory and `auth.yaml` with modes `0700` and `0600`, respectively. Ollama requires no authentication entry.

Example `config.yaml`:

```yaml
default_model: gemini-flash
generation:
  fallback: draft
batch:
  mode: auto
  max_retries: 2
quotas:
  gemini-flash:
    rpm: 5
    tpm: 250000
# typst_template: /path/to/custom.typ
```

Set `typst_template` only to replace the bundled default. When omitted, FlowCloze materializes the embedded template automatically. `flowcloze pdf --template ...` takes precedence for one invocation.

With `batch.mode: auto`, FlowCloze evaluates each qblock independently by estimated input tokens, estimated output tokens, and blank count rather than by qblock number. Light qblocks are repacked into the same request, while a qblock that consumes a large share of any budget is sent alone. Final output is restored to source qblock order. A malformed batch response causes the next retry batch to shrink, while qblock-specific validation failures retry only failed qblocks and preserve successful results.

Main `generate` options:

```text
--model <profile>
--fallback error|draft
--batch auto|small|one-task
--offline
--verbose
-s, --skip-constraints
```

Example:

```bash
flowcloze generate \
  --model gemini-flash \
  --fallback draft \
  --batch auto \
  --verbose \
  -o sample/generated.json \
  sample/sample.md
```

`fallback`:

- `error`: return the failure as an error
- `draft`: fall back failed transport or content-validation tasks to Identity drafts

`--offline` generates every task with Identity without calling a provider.

Settings resolve in this order: **CLI override > `~/.config/flowcloze/config.yaml` > built-in defaults**. Unknown fields are configuration errors.

FlowCloze does not automatically read `.env`, configuration files in the current directory, or legacy configuration environment variables.

## Providers and Models

Built-in definitions:

- provider `google`: `https://generativelanguage.googleapis.com/v1beta/openai`, API key required
- provider `ollama`: `http://localhost:11434/v1`, no authentication
- model profile `gemini-flash`: provider `google`, model `gemini-2.5-flash`

List available model profiles:

```bash
flowcloze model list
```

Add or replace an Ollama model profile:

```bash
flowcloze model add local-qwen --provider ollama --model qwen3:14b
```

`model add` modifies only the model profile in `model.yaml`. It does not store provider URLs or API keys in the model.

Check provider reachability:

```bash
flowcloze provider check google
flowcloze provider check ollama
```

With Ollama:

```bash
ollama pull qwen3:14b
flowcloze provider check ollama
```

## Inspect Scaffold

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

With `--verbose` or `FLOWCLOZE_LOG=debug`, it also emits observability JSON Lines to stderr. Markdown bodies, prompts, provider responses, and credentials are not included in logs.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Google and Ollama use the same OpenAI-compatible adapter, so no separate Gemini-native feature check is required.

## Editor Support

`editors/vscode-flowcloze-syntax` contains a small VS Code extension that highlights `#qblock`, `[answer]`, and `[answer]{type}`.

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
src/application/   use case orchestration
src/cli/           CLI parsing and commands
src/config/        YAML configuration and authentication
src/core/          parser, model, and validation core
src/generation/    planning and question generation
src/output/        JSON, TUI, CSV, and PDF output
src/providers/     provider catalog and OpenAI-compatible adapter
src/runtime/       progress and observability
templates/         Typst templates
sample/            sample inputs / outputs
editors/           editor support
tests/             integration tests
```

## License

Licensed under either Apache License, Version 2.0 or MIT License, at your option.
