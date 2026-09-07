from pathlib import Path
import os

ROOT = Path(__file__).resolve().parents[1]


def replace(path: str, old: str, new: str) -> None:
    p = ROOT / path
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"pattern not found in {path}: {old[:120]!r}")
    p.write_text(text.replace(old, new))


# Version bump. Cargo.lock is synchronized by cargo check in the release workflow.
replace("Cargo.toml", 'version = "2.1.0-beta"', 'version = "2.1.1-beta"')

# Make the built-in PDF template path follow the standard user config directory.
replace(
    "src/config.rs",
    'const DEFAULT_TYPST_TEMPLATE: &str = "templates/cloze.typ";\n',
    "",
)
replace(
    "src/config.rs",
    '''/// PDF の既定 Typst テンプレートを標準 config.toml から解決する。
pub fn typst_template_path() -> Result<PathBuf, String> {
    let file = load_file()?;
    Ok(expand_home(
        file.typst_template
            .as_deref()
            .unwrap_or(DEFAULT_TYPST_TEMPLATE),
    ))
}
''',
    '''/// PDF の既定 Typst テンプレートを標準 config.toml から解決する。
pub fn typst_template_path() -> Result<PathBuf, String> {
    let file = load_file()?;
    match file.typst_template {
        Some(value) if !value.trim().is_empty() => Ok(expand_home(&value)),
        _ => Ok(config_dir()?.join("templates").join("cloze.typ")),
    }
}
''',
)
replace(
    "src/config.rs",
    '''    #[test]
    fn loads_standard_config_and_typst_template() {
''',
    '''    #[test]
    fn default_typst_template_uses_config_directory() {
        let _lock = environment_test_lock();
        let _xdg = EnvironmentVariable::new("XDG_CONFIG_HOME");
        let directory = temporary_home("default-template");
        env::set_var("XDG_CONFIG_HOME", &directory);
        assert_eq!(
            typst_template_path().unwrap(),
            directory
                .join("flowcloze")
                .join("templates")
                .join("cloze.typ")
        );
    }

    #[test]
    fn loads_standard_config_and_typst_template() {
''',
)

# Official installer: installs the binary, injects the bundled Typst template,
# and writes the managed template path into the standard config.
installer = r'''#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

if [[ ! -f "$ROOT_DIR/templates/cloze.typ" ]]; then
  echo "templates/cloze.typ が見つかりません: $ROOT_DIR/templates/cloze.typ" >&2
  exit 1
fi

"${CARGO:-cargo}" install --path "$ROOT_DIR" --force

if [[ -n "${XDG_CONFIG_HOME:-}" ]]; then
  CONFIG_HOME="$XDG_CONFIG_HOME"
elif [[ -n "${HOME:-}" ]]; then
  CONFIG_HOME="$HOME/.config"
else
  echo "HOME または XDG_CONFIG_HOME が必要です" >&2
  exit 1
fi

FLOWCLOZE_CONFIG_DIR="$CONFIG_HOME/flowcloze"
TEMPLATE_DIR="$FLOWCLOZE_CONFIG_DIR/templates"
CONFIG_PATH="$FLOWCLOZE_CONFIG_DIR/config.toml"

umask 077
mkdir -p "$TEMPLATE_DIR"
chmod 700 "$FLOWCLOZE_CONFIG_DIR" "$TEMPLATE_DIR" 2>/dev/null || true
install -m 0644 "$ROOT_DIR/templates/cloze.typ" "$TEMPLATE_DIR/cloze.typ"
TEMPLATE_PATH="$(cd "$TEMPLATE_DIR" && pwd -P)/cloze.typ"

# Escape the absolute path for a TOML basic string.
TOML_TEMPLATE_PATH="${TEMPLATE_PATH//\\/\\\\}"
TOML_TEMPLATE_PATH="${TOML_TEMPLATE_PATH//\"/\\\"}"
SETTING="typst_template = \"$TOML_TEMPLATE_PATH\""

if [[ -f "$CONFIG_PATH" ]]; then
  TMP="$(mktemp "$FLOWCLOZE_CONFIG_DIR/.config.XXXXXX")"
  replaced=false
  while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ "$line" =~ ^[[:space:]]*typst_template[[:space:]]*= ]]; then
      printf '%s\n' "$SETTING" >> "$TMP"
      replaced=true
    else
      printf '%s\n' "$line" >> "$TMP"
    fi
  done < "$CONFIG_PATH"
  if [[ "$replaced" == false ]]; then
    printf '\n%s\n' "$SETTING" >> "$TMP"
  fi
  mv "$TMP" "$CONFIG_PATH"
else
  printf '%s\n' "$SETTING" > "$CONFIG_PATH"
fi
chmod 600 "$CONFIG_PATH" 2>/dev/null || true

printf 'FlowCloze installed.\n'
printf '  binary: %s\n' "${CARGO_HOME:-$HOME/.cargo}/bin/flowcloze"
printf '  template: %s\n' "$TEMPLATE_PATH"
printf '  config: %s\n' "$CONFIG_PATH"
'''
(ROOT / "install.sh").write_text(installer)
os.chmod(ROOT / "install.sh", 0o755)

# Example config now points at the managed installer location.
replace(
    "config.toml.example",
    '# Use an absolute path when FlowCloze is installed globally.\ntypst_template = "/absolute/path/to/cloze.typ"',
    '# install.sh places the bundled template here by default.\ntypst_template = "~/.config/flowcloze/templates/cloze.typ"',
)

# Japanese README.
replace(
    "README.md",
    '''`flowcloze` コマンドとしてインストールする場合:

```bash
cargo install --path .
flowcloze --version
```

インストール先は通常 `~/.cargo/bin/flowcloze` です。
''',
    '''`flowcloze` コマンドとしてインストールする場合は、付属インストーラを使います:

```bash
./install.sh
flowcloze --version
```

`install.sh` は `cargo install --path . --force` を実行したあと、Typstテンプレートを `~/.config/flowcloze/templates/cloze.typ`（`XDG_CONFIG_HOME` 設定時はその配下）へ配置し、`config.toml` の `typst_template` も自動設定します。

インストール先は通常 `~/.cargo/bin/flowcloze` です。`cargo install --path .` を直接使うとバイナリだけが入り、Typstテンプレートの注入は行われません。
''',
)
replace(
    "README.md",
    '標準テンプレートは `~/.config/flowcloze/config.toml` の `typst_template` で指定します。\n',
    '`./install.sh` で導入した場合、標準テンプレートは `~/.config/flowcloze/templates/cloze.typ` に配置され、`config.toml` へ自動設定されます。\n',
)
replace(
    "README.md",
    '''通常設定を作る例:

```bash
mkdir -p ~/.config/flowcloze
cp config.toml.example ~/.config/flowcloze/config.toml
```
''',
    '''通常のインストールでは `./install.sh` が設定ディレクトリ、Typstテンプレート、`config.toml` を自動作成・更新します。
''',
)
replace(
    "README.md",
    'typst_template = "/absolute/path/to/cloze.typ"',
    'typst_template = "~/.config/flowcloze/templates/cloze.typ"',
)

# English README.
replace(
    "README.en.md",
    '''Install `flowcloze` as a command:

```bash
cargo install --path .
flowcloze --version
```

The binary is usually installed to `~/.cargo/bin/flowcloze`.
''',
    '''Install `flowcloze` with the bundled installer:

```bash
./install.sh
flowcloze --version
```

`install.sh` runs `cargo install --path . --force`, copies the bundled Typst template to `~/.config/flowcloze/templates/cloze.typ` (or under `XDG_CONFIG_HOME`), and updates `typst_template` in `config.toml` automatically.

The binary is usually installed to `~/.cargo/bin/flowcloze`. Running `cargo install --path .` directly installs only the binary and does not inject the Typst template.
''',
)
replace(
    "README.en.md",
    'Set the default template with `typst_template` in `~/.config/flowcloze/config.toml`.\n',
    'When installed with `./install.sh`, the default template is placed at `~/.config/flowcloze/templates/cloze.typ` and configured automatically.\n',
)
replace(
    "README.en.md",
    '''Create the normal config:

```bash
mkdir -p ~/.config/flowcloze
cp config.toml.example ~/.config/flowcloze/config.toml
```
''',
    '''A normal `./install.sh` installation creates or updates the config directory, bundled Typst template, and `config.toml` automatically.
''',
)
replace(
    "README.en.md",
    'typst_template = "/absolute/path/to/cloze.typ"',
    'typst_template = "~/.config/flowcloze/templates/cloze.typ"',
)

# Changelog.
replace(
    "CHANGELOG.md",
    "# Changelog\n\n",
    '''# Changelog

## 2.1.1-beta - 2026-09-07

### Fixed

- Install the bundled Typst template into the standard FlowCloze config directory through `install.sh`.
- Make the default PDF template path resolve from `XDG_CONFIG_HOME` / `~/.config/flowcloze` instead of the current working directory.

### Changed

- Use `./install.sh` as the documented installation path so binary installation and template/config injection happen together.

''',
)
