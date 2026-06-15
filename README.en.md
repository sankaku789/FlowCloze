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
- Gemini API key (required for the `generate` command)

Build:

```bash
cargo build --release
```

Install locally as a command:

```bash
mkdir -p ~/.local/bin
ln -sfn "$PWD/target/release/flowcloze" ~/.local/bin/flowcloze
```

For a temporary local run, use `cargo run -- ...`.

## Gemini API Settings

Using `.env`:

```bash
cp .env.example .env
```

Set values like:

```env
GEMINI_API_KEY=your_api_key_here
GEMINI_MODEL=gemini-2.5-flash
```

Or save them through the CLI:

```bash
flowcloze api set --key your_api_key_here --model gemini-2.5-flash
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
