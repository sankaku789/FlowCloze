from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def replace(path: str, old: str, new: str) -> None:
    p = ROOT / path
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"pattern not found in {path}: {old[:120]!r}")
    p.write_text(text.replace(old, new))


replace("Cargo.toml", 'version = "2.1.1-beta"', 'version = "2.1.2-beta"')

replace(
    "src/config.rs",
    'const GEMINI_OPENAI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";\n',
    'const GEMINI_OPENAI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";\nconst BUNDLED_TYPST_TEMPLATE: &str = include_str!("../templates/cloze.typ");\n',
)

replace(
    "src/config.rs",
    '''/// PDF の既定 Typst テンプレートを標準 config.toml から解決する。
pub fn typst_template_path() -> Result<PathBuf, String> {
    let file = load_file()?;
    match file.typst_template {
        Some(value) if !value.trim().is_empty() => Ok(expand_home(&value)),
        _ => Ok(config_dir()?.join("templates").join("cloze.typ")),
    }
}
''',
    '''/// PDF のTypstテンプレートを解決する。
///
/// `typst_template` が未指定なら、バイナリへ埋め込んだ標準テンプレートを
/// ユーザー設定ディレクトリへ展開して利用する。
pub fn typst_template_path() -> Result<PathBuf, String> {
    let file = load_file()?;
    let managed = config_dir()?.join("templates").join("cloze.typ");
    match file.typst_template {
        Some(value) if !value.trim().is_empty() => {
            let selected = expand_home(&value);
            if selected == managed {
                ensure_bundled_typst_template(&managed)?;
            }
            Ok(selected)
        }
        _ => {
            ensure_bundled_typst_template(&managed)?;
            Ok(managed)
        }
    }
}

fn ensure_bundled_typst_template(path: &Path) -> Result<(), String> {
    let directory = config_dir()?;
    let template_directory = path
        .parent()
        .ok_or_else(|| "Typstテンプレートの保存先が不正です".to_string())?;
    fs::create_dir_all(template_directory).map_err(|error| {
        format!(
            "{} を作成できませんでした: {error}",
            template_directory.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "{} の権限を設定できませんでした: {error}",
                directory.display()
            )
        })?;
    }

    let current = fs::read_to_string(path).ok();
    if current.as_deref() == Some(BUNDLED_TYPST_TEMPLATE) {
        return Ok(());
    }
    fs::write(path, BUNDLED_TYPST_TEMPLATE)
        .map_err(|error| format!("{} を書き込めませんでした: {error}", path.display()))
}
''',
)

replace(
    "src/config.rs",
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
''',
    '''    #[test]
    fn default_typst_template_is_materialized_from_binary() {
        let _lock = environment_test_lock();
        let _xdg = EnvironmentVariable::new("XDG_CONFIG_HOME");
        let directory = temporary_home("default-template");
        env::set_var("XDG_CONFIG_HOME", &directory);
        let expected = directory
            .join("flowcloze")
            .join("templates")
            .join("cloze.typ");
        assert_eq!(typst_template_path().unwrap(), expected);
        assert_eq!(fs::read_to_string(&expected).unwrap(), BUNDLED_TYPST_TEMPLATE);

        fs::write(&expected, "stale template").unwrap();
        assert_eq!(typst_template_path().unwrap(), expected);
        assert_eq!(fs::read_to_string(&expected).unwrap(), BUNDLED_TYPST_TEMPLATE);
        fs::remove_dir_all(directory).unwrap();
    }
''',
)

replace(
    "config.toml.example",
    '''# install.sh places the bundled template here by default.
typst_template = "~/.config/flowcloze/templates/cloze.typ"''',
    '''# The bundled Typst template is materialized automatically when PDF output is used.
# Set this only to override it with your own template.
# typst_template = "/path/to/custom.typ"''',
)

replace(
    "README.md",
    '''`flowcloze` コマンドとしてインストールする場合は、付属インストーラを使います:

```bash
./install.sh
flowcloze --version
```

`install.sh` は `cargo install --path . --force` を実行したあと、Typstテンプレートを `~/.config/flowcloze/templates/cloze.typ`（`XDG_CONFIG_HOME` 設定時はその配下）へ配置し、`config.toml` の `typst_template` も自動設定します。

インストール先は通常 `~/.cargo/bin/flowcloze` です。`cargo install --path .` を直接使うとバイナリだけが入り、Typstテンプレートの注入は行われません。
''',
    '''`flowcloze` コマンドとしてインストールする場合:

```bash
cargo install --path . --force
flowcloze --version
```

標準Typstテンプレートはバイナリに内包されています。PDF出力を初めて使うと、FlowCloze自身が `~/.config/flowcloze/templates/cloze.typ`（`XDG_CONFIG_HOME` 設定時はその配下）へ自動展開します。そのため、インストール手順はシェルやOS固有のスクリプトに依存しません。

インストール先は通常 `~/.cargo/bin/flowcloze` です。
''',
)
replace(
    "README.md",
    '`./install.sh` で導入した場合、標準テンプレートは `~/.config/flowcloze/templates/cloze.typ` に配置され、`config.toml` へ自動設定されます。\n',
    '標準テンプレートはバイナリに内包され、PDF出力時に `~/.config/flowcloze/templates/cloze.typ` へ自動展開されます。`typst_template` を設定した場合は、そのカスタムテンプレートを優先します。\n',
)
replace(
    "README.md",
    '通常のインストールでは `./install.sh` が設定ディレクトリ、Typstテンプレート、`config.toml` を自動作成・更新します。\n',
    '標準TypstテンプレートはPDF出力時に自動展開されるため、テンプレート配置のための追加インストール操作は不要です。\n',
)
replace(
    "README.md",
    'typst_template = "~/.config/flowcloze/templates/cloze.typ"',
    '# typst_template = "/path/to/custom.typ"',
)
replace(
    "README.md",
    '`typst_template` がPDF生成時の標準テンプレートになります。`flowcloze pdf --template ...` を指定した場合はCLI指定を優先します。',
    '`typst_template` は標準テンプレートを差し替えたい場合だけ指定します。未指定時は内蔵テンプレートを自動展開します。`flowcloze pdf --template ...` を指定した場合はCLI指定を優先します。',
)

replace(
    "README.en.md",
    '''Install `flowcloze` with the bundled installer:

```bash
./install.sh
flowcloze --version
```

`install.sh` runs `cargo install --path . --force`, copies the bundled Typst template to `~/.config/flowcloze/templates/cloze.typ` (or under `XDG_CONFIG_HOME`), and updates `typst_template` in `config.toml` automatically.

The binary is usually installed to `~/.cargo/bin/flowcloze`. Running `cargo install --path .` directly installs only the binary and does not inject the Typst template.
''',
    '''Install `flowcloze` as a command:

```bash
cargo install --path . --force
flowcloze --version
```

The standard Typst template is embedded in the binary. When PDF output is used for the first time, FlowCloze materializes it at `~/.config/flowcloze/templates/cloze.typ` (or under `XDG_CONFIG_HOME`). Installation therefore does not depend on an OS- or shell-specific installer script.

The binary is usually installed to `~/.cargo/bin/flowcloze`.
''',
)
replace(
    "README.en.md",
    'When installed with `./install.sh`, the default template is placed at `~/.config/flowcloze/templates/cloze.typ` and configured automatically.\n',
    'The standard template is embedded in the binary and materialized at `~/.config/flowcloze/templates/cloze.typ` when PDF output is used. A configured `typst_template` overrides it.\n',
)
replace(
    "README.en.md",
    'A normal `./install.sh` installation creates or updates the config directory, bundled Typst template, and `config.toml` automatically.\n',
    'The bundled Typst template is materialized automatically when PDF output is used, so no extra template-install step is required.\n',
)
replace(
    "README.en.md",
    'typst_template = "~/.config/flowcloze/templates/cloze.typ"',
    '# typst_template = "/path/to/custom.typ"',
)
replace(
    "README.en.md",
    '`typst_template` is the default template for PDF generation. `flowcloze pdf --template ...` overrides it for one invocation.',
    '`typst_template` is only needed to replace the bundled default. If omitted, FlowCloze materializes the embedded template automatically. `flowcloze pdf --template ...` overrides it for one invocation.',
)

replace(
    "CHANGELOG.md",
    "# Changelog\n\n",
    '''# Changelog

## 2.1.2-beta - 2026-09-07

### Fixed

- Remove the shell-specific installation requirement introduced in 2.1.1-beta.
- Embed the standard Typst template in the FlowCloze binary and materialize it automatically for PDF output.

### Changed

- Restore `cargo install --path . --force` as the cross-platform installation path.
- Keep `typst_template` as an optional custom-template override instead of requiring installer-written configuration.

''',
)
