# Changelog

## 1.0.0-beta.1 - Unreleased

### Added

- Add static documentation for overview and generation specification.
- Add GitHub Actions CI workflow.

### Changed

- Refactor Gemini API request handling into a reusable request layer.
- Simplify README files now that detailed documentation lives under `docs/`.
- Document local command installation with `cargo install --path .`.

## 0.1.0 - 2026-05-15

Initial release.

- Parse FlowCloze qblock notation from Markdown notes.
- Extract `[answer]{type}` targets and emit intermediate JSON.
- Generate context-cloze question JSON with Gemini.
- Validate generated JSON against the intermediate targets.
- View generated questions in a terminal UI.
- Export generated questions as Ankilot-compatible CSV.
- Build answer/question PDF sheets with Typst.
- Include a local VS Code syntax highlighting extension for FlowCloze notation.
