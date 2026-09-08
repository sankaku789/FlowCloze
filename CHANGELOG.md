# Changelog

## 2.1.5-beta - 2026-09-09

### Added

- Add config-defined quota profiles with model-specific overrides, with example profiles for Gemini, OpenAI, and Mistral.

### Changed

- Adapt `batch = "auto"` within configured quality limits so initial batch plans can fit daily request budgets while reserving requests for retries.
- Pace provider HTTP attempts against configured RPM and TPM limits, including transport retries.
- Re-batch failed content tasks on the first retry and use single-task retries only for the final retry attempt.

## 2.1.4-beta - 2026-09-08

### Fixed

- Surface provider failures explicitly in CLI output and preserve terminal HTTP status codes such as 429 and 503 for diagnostics.
- Replace the misleading `partial validation` terminal message with `generation incomplete` when generation stops before all tasks finish.

## 2.1.3-beta - 2026-09-08

### Added

- Add interactive provider selection to `flowcloze api set` and read API keys without echoing them.
- Add dedicated OpenAI-compatible API key storage in `credentials.toml`, while accepting the previous `local_llm_api_key` key when reading existing credentials.

### Fixed

- Support Gemini 3 models, including `gemini-3.8-flash`, by omitting deprecated sampling parameters from OpenAI-compatible requests.

## 2.1.2-beta - 2026-09-07

### Fixed

- Remove the shell-specific installation requirement introduced in 2.1.1-beta.
- Embed the standard Typst template in the FlowCloze binary and materialize it automatically for PDF output.

### Changed

- Restore `cargo install --path . --force` as the cross-platform installation path.
- Keep `typst_template` as an optional custom-template override instead of requiring installer-written configuration.

## 2.1.1-beta - 2026-09-07

### Fixed

- Install the bundled Typst template into the standard FlowCloze config directory through `install.sh`.
- Make the default PDF template path resolve from `XDG_CONFIG_HOME` / `~/.config/flowcloze` instead of the current working directory.

### Changed

- Use `./install.sh` as the documented installation path so binary installation and template/config injection happen together.

## 2.1.0-beta - 2026-09-07

### Added

- Add user-level FlowCloze configuration under `~/.config/flowcloze/` with `XDG_CONFIG_HOME` support.
- Add private `credentials.toml` storage for Gemini API keys through `flowcloze api set`.
- Add `typst_template` as the default PDF template setting.

### Changed

- Resolve generation settings from CLI, the standard user config, then built-in defaults.

### Removed

- Remove automatic `.env`, current-directory `config.toml`, and legacy configuration environment-variable loading.

## 2.0.0-beta - 2026-09-07

### Added

- Add GitHub Actions CI workflow.

### Changed

- Refactor Gemini API request handling into a reusable request layer.
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
