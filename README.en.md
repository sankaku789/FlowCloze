# FlowCloze

[日本語](README.md) | English

FlowCloze is a CLI tool that generates context cloze questions from study notes written in Markdown.

You keep your notes readable as normal Markdown and wrap only the ranges you want to turn into questions with `#qblock{ ... }`. Terms to be used as answers are explicitly marked as `[answer]`. FlowCloze converts those annotations into an intermediate JSON, asks Gemini only to edit the question body, then fills fixed fields such as answers, targets, sections, source text, and paragraph boundaries on the Rust side before validating and producing PDFs.

```text
Markdown note
  -> qblock / target extraction
  -> intermediate JSON
  -> Gemini edits question text only
  -> generated JSON normalization
  -> validation
  -> Typst PDF
```

## Background

When studying for exams, people often use two main approaches:

1. Markdown notes
2. Handmade memorization sheets (Excel → PDF)

The first approach is convenient for summarizing materials in your own words while reading; it's easy to review later but can lead to remembering concepts only at a high level, which makes recalling exact terms or definitions harder.

The second approach is to create **context cloze questions** by turning key terms into blanks. Those questions were entered into Excel, formatted into memorization sheets, exported as PDFs, and imported into a note-taking app. Context cloze questions are more memorable than simple flashcards because the surrounding sentence helps recall meanings and definitions.

However, that workflow required not only creating question text but also copying into Excel and formatting the final PDF, which consumed significant effort before studying could even start.

FlowCloze was created to combine the ease of writing Markdown notes with the memorability of context cloze questions.

## System Overview

FlowCloze extracts qblock ranges and answer targets from Markdown notes and builds intermediate JSON. Gemini does not receive the full intermediate JSON; it receives only a slim input containing `id`, `cloze_template`, `blank_count`, and `answers`. Gemini uses the source text and cloze draft to generate only the `question` body. FlowCloze then normalizes the final generated JSON from the intermediate JSON and validates blank counts, answer order, and paragraph boundaries.

```mermaid
flowchart LR
    note[Markdown note<br/>#qblock / targets] --> parse[Parser<br/>qblock / target / section]
    parse --> intermediate[Intermediate JSON<br/>fixed structure]
    intermediate --> prompt[Prompt input<br/>id + cloze draft + blank count + answers]
    prompt --> gemini[Gemini API<br/>question text only]
    gemini --> normalize[Normalizer<br/>fill fixed fields]
    intermediate --> normalize
    normalize --> validate[Validator]
    validate --> json[Generated JSON]
    json --> pdf[PDF<br/>via Typst]
    json --> csv[Ankilot CSV]
    json --> tui[TUI viewer]
```

## Features

- Extract `#qblock{ ... }` ranges from Markdown
- Use only terms marked as `[answer]` or `[answer]{type}` as answer targets
- Treat `# Heading 1` as the section title for generated JSON and PDF output
- Auto-assign qblock IDs in `qblock-001` order
- Edit fragmented note text into context cloze question bodies with the Gemini API
- Deterministically build `section`, `targets`, `answers`, and `source_text` from intermediate JSON
- Compare intermediate JSON and generated JSON to detect blank-count, answer-order, target, and paragraph-boundary mismatches
- Review generated questions in a TUI before output
- Render A4 landscape PDFs with Typst in answer-page then question-page order
- Export CSV suitable for Ankilot import
- Bundled simple VS Code syntax highlighting extension

## Setup

### Requirements

- Rust / Cargo
- Typst CLI (required for PDF output)
- Gemini API key (required for the `generate` command)

### Build and Install

```bash
cargo build --release
mkdir -p ~/.local/bin
ln -sfn "$PWD/target/release/flowcloze" ~/.local/bin/flowcloze
```

If `~/.local/bin` is not in your `PATH`, add it to your shell configuration.

Verify the build and tests:

```bash
flowcloze --version
cargo test
```

For a debug build, you can run:

```bash
cargo build
```

Examples below assume you created the symbolic link after a release build and can run `flowcloze`. For a temporary local run, use `cargo run -- ...` instead of `flowcloze ...`.

```bash
flowcloze sample/sample.md
```

## Markdown Format

### qblock

Wrap the range you want to turn into questions with `#qblock{ ... }`.

```md
# Software Engineering Overview

#qblock{
- [QCD]{term-name} means [quality]{meaning}, [cost]{meaning}, and [delivery]{meaning}
}
```

Do not write qblock IDs manually. They are assigned automatically in appearance order (e.g. `qblock-001`).

```md
#qblock{
- An [information system]{term-name} is a system in which people, machines, and computers cooperate to achieve a purpose.
}
```

### Targets

Write answer targets as `[answer]`. When needed, you can also write `[answer]{type}` to specify the question perspective.

```md
[Requirements definition] consists of [elicitation], [analysis], [specification], and [validation].
```

The text inside `[]` is the answer string. When `{}` is present, it is used as the question perspective. When the type is omitted, FlowCloze treats it as `term-name`. Targets and answers are copied from the intermediate JSON into the generated JSON by Rust, so Gemini does not infer answer targets.

### Sections

PDF section titles use the nearest `# Heading 1` before a qblock, or the first `# Heading 1` inside the qblock. `##` and `###` headings are not used as sections.

```md
# Requirements Definition
```

Inside a qblock, `##` and `###` are treated as paragraph boundaries, not as sections. The heading text itself is not included in the generated question body.

### Target Types

Types are optional. When specifying a type, the following values are safe to use without warnings. A type indicates the perspective from which the term will be questioned.

| type | Description |
|---|---|
| `term-name` | Ask for the term itself |
| `meaning` | Ask for a meaning, definition, property, or purpose |
| `process` | Ask for a procedure, step, action, or state change |
| `relation` | Ask for a structure, comparison, classification, relation, or correspondence |

Undefined types are still extracted but will be listed in the intermediate JSON `warnings`.

## CLI Usage

### API Settings

To use the `generate` command you need to save a Gemini API key. You can set it in a `.env` file or use the CLI helper:

```bash
flowcloze api set --key your_api_key_here
```

To update the model setting:

```bash
flowcloze api set --key your_api_key_here --model gemini-2.5-flash
```

You can also create a `.env` from the example:

```bash
cp .env.example .env
```

And set values like:

```env
GEMINI_API_KEY=your_api_key_here
GEMINI_MODEL=gemini-2.5-flash
```

`GEMINI_MODEL` is optional; when omitted the default `gemini-2.5-flash` is used.

### Parse Markdown

Print extracted qblock IDs and targets as text.

```bash
flowcloze sample/sample.md
```

### Write Intermediate JSON

Generate intermediate JSON from Markdown.

```bash
flowcloze --json -o sample/sample.json sample/sample.md
```

Omit `-o` to write to standard output.

```bash
flowcloze --json sample/sample.md
```

### Generate Questions

Generate context cloze questions with Gemini. Gemini returns only each qblock's `id` and `question`. FlowCloze then fills `section`, `type`, `targets`, `answers`, and `source_text` from the intermediate JSON and normalizes the final generated JSON. If validation fails, FlowCloze sends the validation errors back to Gemini and regenerates the output up to 3 times.

```bash
flowcloze generate -o sample/sample.json sample/sample.md
```

Enter additional constraints during `generate` and finish input with an empty line. To skip additional constraints:

```bash
flowcloze generate -s -o sample/sample.json sample/sample.md
```

Specify a model explicitly:

```bash
flowcloze generate --model gemini-2.5-flash -o sample/sample.json sample/sample.md
```

### Validate Generated JSON

Validate intermediate JSON and generated JSON manually.

```bash
flowcloze validate sample/sample.json sample/sample.json
```

On success, FlowCloze prints `validation ok`. On failure, it prints validation errors and exits with status code `1`.

### View Generated JSON

Review generated JSON in the TUI.

```bash
flowcloze view sample/sample.json
```

### Export Ankilot CSV

Export generated JSON as CSV for Ankilot import. The CSV is UTF-8, has no header, and contains two columns:

1. Front: question
2. Back: answers

```bash
flowcloze csv -o sample/sample.csv sample/sample.json
```

Omit `-o` to write to standard output.

### Build PDF

Create a PDF from generated JSON. By default, FlowCloze uses `templates/cloze.typ` and writes a `.pdf` next to the input JSON.

```bash
flowcloze pdf sample/sample.json
```

You can specify an output path and template:

```bash
flowcloze pdf -o sample/sample.pdf --template templates/cloze.typ sample/sample.json
```

The PDF outputs each page in answer then question order. Answer pages show answers in red, and question pages replace the same positions with blanks.

### Help and Version

```bash
flowcloze --help
flowcloze --version
```

## JSON Format

The intermediate JSON stores facts extracted from Markdown as Rust-side generation tasks.
Blank positions, answer order, paragraph boundaries, and section titles are fixed by Rust. Gemini does not receive this full intermediate JSON; FlowCloze extracts only `id`, `cloze_template`, `blank_count`, and `answers` into a slim Gemini input. `answers` is provided only so Gemini can check that the sentence remains grammatical when answers are put back into the blanks.

```json
{
  "schema_version": 3,
  "meta": {
    "source": "sample/sample.md",
    "format": {
      "blank": "＿＿＿",
      "block_separator": "\n\n",
      "paragraph_indent": "　"
    }
  },
  "tasks": [
    {
      "id": "qblock-001",
      "type": "context-cloze",
      "section": "Requirements Definition",
      "source": {
        "raw": "[Requirements definition]{term-name} is the process of creating a [requirements specification]{relation} from what the customer wants.",
        "plain": "Requirements definition is the process of creating a requirements specification from what the customer wants."
      },
      "blocks": [
        {
          "id": "qblock-001-b001",
          "kind": "paragraph",
          "starts_new_paragraph": false,
          "text": "Requirements definition is the process of creating a requirements specification from what the customer wants.",
          "cloze_text": "　＿＿＿ is the process of creating a ＿＿＿ from what the customer wants.",
          "target_refs": [0, 1]
        }
      ],
      "cloze_template": "Original text:\nRequirements definition is the process of creating a requirements specification from what the customer wants.\n\nCloze draft:\n　＿＿＿ is the process of creating a ＿＿＿ from what the customer wants.",
      "targets": [
        { "index": 0, "answer": "Requirements definition", "type": "term-name", "block_id": "qblock-001-b001" },
        { "index": 1, "answer": "requirements specification", "type": "relation", "block_id": "qblock-001-b001" }
      ],
      "answers": ["Requirements definition", "requirements specification"]
    }
  ]
}
```

The actual Gemini input is conceptually the following minimal shape. It does not include `meta`, `source`, `blocks`, or `targets`.

```json
{
  "tasks": [
    {
      "id": "qblock-001",
      "cloze_template": "Original text:\nRequirements definition is the process of creating a requirements specification from what the customer wants.\n\nCloze draft:\n　＿＿＿ is the process of creating a ＿＿＿ from what the customer wants.",
      "blank_count": 2,
      "answers": ["Requirements definition", "requirements specification"]
    }
  ]
}
```

Gemini's raw output is expected to be the following minimal shape.

```json
{
  "questions": [
    {
      "id": "qblock-001",
      "question": "　＿＿＿ is the process of creating a ＿＿＿ from what the customer wants."
    }
  ]
}
```

FlowCloze normalizes this output against the intermediate JSON. Generated JSON is the format read by the Typst template and validator.

```json
{
  "questions": [
    {
      "id": "qblock-001",
      "section": "Requirements Definition",
      "type": "context-cloze",
      "targets": [
        { "answer": "Requirements definition", "type": "term-name" },
        { "answer": "requirements specification", "type": "relation" }
      ],
      "question": "_____ is the process of creating a _____ from what the customer wants.",
      "answers": ["Requirements definition", "requirements specification"],
      "source_text": "Requirements definition is the process of creating a requirements specification from what the customer wants.",
      "explanation": "",
      "tags": [],
      "warnings": []
    }
  ]
}
```

## Responsibility Split

The current generation flow does not ask Gemini to maintain the whole JSON structure.

- `parser.rs`: extracts `#qblock`, targets, sections, and target positions from Markdown
- `json.rs`: builds intermediate JSON and fixes `blocks`, `cloze_text`, `cloze_template`, `targets`, and `answers`
- `prompt.rs`: extracts only `id` / `cloze_template` / `blank_count` / `answers` and builds a short prompt for editing `question`
- `gemini.rs`: calls the Gemini API and receives minimal JSON containing `id` and `question`
- `main.rs`: normalizes Gemini output with intermediate JSON and fills `section` / `type` / `targets` / `answers` / `source_text` / paragraph boundaries
- `validation.rs`: validates blank counts, answer order, target correspondence, and paragraph boundaries
- `templates/cloze.typ`: typesets generated JSON into PDF

## Editor Support

`editors/vscode-flowcloze-syntax` contains a small VS Code extension that highlights `#qblock`, `[answer]`, and `[answer]{type}` syntax.

### Local Install

When using VS Code on WSL, create a symbolic link in the VS Code Server extension directory:

```sh
mkdir -p ~/.vscode-server/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" ~/.vscode-server/extensions/flowcloze.flowcloze-syntax-0.0.1
```

Then run `Developer: Reload Window` in VS Code and open a Markdown file such as `sample/sample.md`.

For non-WSL Linux environments, use `~/.vscode/extensions` instead:

```sh
mkdir -p ~/.vscode/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" ~/.vscode/extensions/flowcloze.flowcloze-syntax-0.0.1
```

## Repository Layout

```text
src/parser.rs      Markdown qblock parser
src/json.rs        intermediate JSON conversion
src/prompt.rs      Gemini prompt builder
src/gemini.rs      Gemini API client
src/validation.rs  generated JSON validator
src/csv.rs         Ankilot CSV exporter
src/pdf.rs         Typst PDF adapter
templates/         Typst templates
sample/            sample note and outputs
tests/             parser / JSON / validation tests
```

## Development

This project uses "vibe coding" during development. If you find a bug or a serious issue, please open an Issue. If you can fix it, create a branch, make the change, and send a Pull Request. Contributions are welcome.

## License

Licensed under either Apache License, Version 2.0 or the MIT license, at your option.
