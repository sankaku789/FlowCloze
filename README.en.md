# FlowCloze

[日本語](README.md) | English

FlowCloze is a CLI tool that generates context cloze questions from study notes written in Markdown.
Wrap the range you want to turn into questions with `#qblock{ ... }`, and mark answer targets with `[answer]` or `[answer]{type}`. FlowCloze converts the Markdown into intermediate JSON, generates questions with Gemini, validates the result, and exports PDF/CSV output.

```text
Markdown note
  -> qblock / target extraction
  -> intermediate JSON
  -> Gemini question generation
  -> generated JSON validation
  -> PDF / CSV / TUI
```

## Documentation

Background, syntax, generation rules, and the OpenAPI definition are organized under `docs/` for GitHub Pages.

- `docs/index.html`: background and overview
- `docs/specification.html`: Markdown syntax and generation rules
- `docs/api.html`: OpenAPI documentation
- `docs/openapi.yaml`: HTTP API contract

Regenerate the OpenAPI documentation with:

```bash
npm install
npm run docs:api
```

Validate the definition only:

```bash
npm run docs:api:lint
```

## Setup

Requirements:

- Rust / Cargo
- Typst CLI (required for PDF output)
- Japanese fonts (required for Japanese PDF output)
- Gemini API key (required when Gemini performs rewrite generation)

On Ubuntu / WSL, install Noto CJK fonts for Japanese PDF output:

```bash
sudo apt update
sudo apt install -y fonts-noto-cjk
fc-cache -fv
```

To check whether Typst can see the font:

```bash
typst fonts | grep "Noto Sans CJK"
```

Build only:

```bash
cargo build --release
```

### Install as a Command

Run this from the cloned repository to install `flowcloze` as a command:

```bash
cargo install --path .
```

The binary is usually installed to `~/.cargo/bin/flowcloze`. If `~/.cargo/bin` is not in your `PATH`, add it to your shell configuration:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

Verify the install:

```bash
flowcloze --version
```

You can also symlink the release binary:

```bash
mkdir -p ~/.local/bin
ln -sfn "$PWD/target/release/flowcloze" ~/.local/bin/flowcloze
```

For a temporary local run without installing, use `cargo run -- ...`.

## Generation Settings

Using `.env`:

```bash
cp .env.example .env
```

You may store the actual API key in `.env`. Do not store secrets in `config.toml`; use `api_key_env` there to name the environment variable. If you want a config file, separately run `cp config.toml.example config.toml`. `api set` is deprecated. See [`.env.example`](.env.example) for every setting and its legacy alias.

```env
GEMINI_API_KEY=YOUR_GEMINI_API_KEY
FLOWCLOZE_PROVIDER=gemini
```

Use `generate --provider gemini|local` to select a provider; `--backend` is its compatibility alias. `--model`, `--rewrite always|never|auto`, `--fallback error|draft`, `--structured-output auto|on|off`, and `--verbose` are also available.

```bash
flowcloze generate --provider gemini --model gemini-2.5-flash \
  --rewrite auto --fallback draft --structured-output auto --verbose \
  -o sample/generated.json sample/sample.md
```

`--rewrite never` uses Identity generation, so it needs neither an API key nor a provider connection. `auto` rewrites only list, multiline, unterminated, or short source text, and uses Identity generation otherwise. `--fallback error` (the default) returns failures. With `--fallback draft`, only a task that fails due to transport or content validation falls back to an Identity draft; invalid ID, fixed-field, or ordering correspondence never does.

Settings resolve in this order: CLI, canonical environment variable, legacy environment variable where supported, `config.toml`, then the default. Empty environment variables are unspecified. Set `FLOWCLOZE_CONFIG` to choose another config file; see [`config.toml.example`](config.toml.example).

`--verbose` or `FLOWCLOZE_LOG=debug` writes observability JSON Lines to stderr. They never include Markdown bodies, prompts, provider responses, or credentials. `max_concurrent_batches` is validated and observed, but execution is currently sequential.

For compatibility, the CLI can save a key:

```bash
flowcloze api set --key your_api_key_here
```

## Minimal Example

Input Markdown:

```md
# Software Engineering Overview

#qblock{
[QCD]{term-name} means [quality]{meaning}, [cost]{meaning}, and [delivery]{meaning}.
}
```

Inspect extracted qblocks:

```bash
flowcloze sample/sample.md
```

Write intermediate JSON:

```bash
flowcloze --json -o sample/sample.json sample/sample.md
```

Generate questions with Gemini:

```bash
flowcloze generate -s -o sample/generated.json sample/sample.md
```

Inspect the scaffold sent to the LLM:

```bash
flowcloze inspect-scaffold sample/sample.md
```

Generate with a specific batch policy:

```bash
flowcloze generate --batch small -s -o sample/generated.json sample/sample.md
```

Generate with a local LLM through Ollama or LM Studio's OpenAI-compatible server:

Install the default local model, then start either the Ollama or LM Studio local server before running FlowCloze. The URL resolves through `FLOWCLOZE_BASE_URL`, the legacy `LOCAL_LLM_BASE_URL`, `config.toml`, then the defaults. When unset, FlowCloze tries Ollama (`http://localhost:11434/v1`) first, then falls back to LM Studio (`http://localhost:1234/v1`).

For Ollama:

```bash
ollama pull gemma4:e2b-it-qat
```

For LM Studio, download and load `gemma4:e2b-it-qat` in LM Studio, then start the Local Server.

```bash
flowcloze local check
```

```bash
flowcloze generate --provider local -s -o sample/generated.json sample/sample.md
```

Build a PDF:

```bash
flowcloze pdf -o sample/sample.pdf sample/generated.json
```

Export CSV for Ankilot:

```bash
flowcloze csv -o sample/sample.csv sample/generated.json
```

## Common Commands

```bash
flowcloze --help
flowcloze --version
cargo test
npm run docs:api:lint
npm run docs:api
```

## Editor Support

`editors/vscode-flowcloze-syntax` contains a small VS Code extension that highlights `#qblock`, `[answer]`, and `[answer]{type}`.

When using VS Code on WSL:

```sh
mkdir -p ~/.vscode-server/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" ~/.vscode-server/extensions/flowcloze.flowcloze-syntax-0.0.1
```

For non-WSL Linux environments:

```sh
mkdir -p ~/.vscode/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" ~/.vscode/extensions/flowcloze.flowcloze-syntax-0.0.1
```

Then run `Developer: Reload Window` in VS Code.

## Repository Layout

```text
src/parser.rs      Markdown qblock parser
src/json.rs        intermediate JSON conversion
src/prompt.rs      Gemini prompt builder
src/gemini.rs      Gemini API client
src/validation.rs  generated JSON validator
src/csv.rs         Ankilot CSV exporter
src/pdf.rs         Typst PDF adapter
docs/              Pages documentation and OpenAPI definition
templates/         Typst templates
sample/            sample note and outputs
tests/             parser / JSON / validation tests
```

## License

Licensed under either Apache License, Version 2.0 or the MIT license, at your option.
