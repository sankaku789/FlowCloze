from pathlib import Path
import re

root = Path('.')

config_rs = r'''//! FlowCloze の標準設定と秘密情報を解決する。

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};

use crate::planner::BatchPolicy;
use crate::providers::capability::StructuredOutputMode;

const GEMINI_OPENAI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
const DEFAULT_TYPST_TEMPLATE: &str = "templates/cloze.typ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Gemini,
    OpenAiCompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewritePolicy {
    Always,
    Never,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackPolicy {
    Error,
    Draft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchPolicyName {
    Auto,
    Small,
    OneTask,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    provider: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    batch: Option<String>,
    max_tasks_per_batch: Option<usize>,
    max_input_tokens: Option<usize>,
    max_concurrent_batches: Option<usize>,
    rewrite: Option<String>,
    fallback: Option<String>,
    structured_output: Option<String>,
    typst_template: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_llm_api_key: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub rewrite: Option<String>,
    pub fallback: Option<String>,
    pub structured_output: Option<String>,
    pub batch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationConfig {
    pub provider: Provider,
    pub model: String,
    pub base_url: Option<String>,
    pub batch: BatchPolicyName,
    pub max_tasks_per_batch: Option<usize>,
    pub max_input_tokens: Option<usize>,
    pub max_concurrent_batches: Option<usize>,
    pub rewrite: RewritePolicy,
    pub fallback: FallbackPolicy,
    pub structured_output: StructuredOutputMode,
}

/// プロセス環境を変更するテストで共有するロック。
#[doc(hidden)]
pub fn environment_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

/// 標準の FlowCloze 設定ディレクトリを返す。
///
/// `XDG_CONFIG_HOME` があれば `$XDG_CONFIG_HOME/flowcloze`、なければ
/// `~/.config/flowcloze` を使う。
pub fn config_dir() -> Result<PathBuf, String> {
    if let Some(xdg) = nonempty_env_path("XDG_CONFIG_HOME") {
        return Ok(xdg.join("flowcloze"));
    }
    if let Some(home) = nonempty_env_path("HOME") {
        return Ok(home.join(".config").join("flowcloze"));
    }
    if let Some(appdata) = nonempty_env_path("APPDATA") {
        return Ok(appdata.join("flowcloze"));
    }
    Err("FlowCloze の設定ディレクトリを決定できません。HOME または XDG_CONFIG_HOME を設定してください".into())
}

pub fn config_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn credentials_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("credentials.toml"))
}

impl GenerationConfig {
    pub fn batch_policy(&self) -> BatchPolicy {
        let mut policy = match (self.provider, self.batch) {
            (_, BatchPolicyName::Small) => BatchPolicy {
                max_tasks_per_batch: 2,
                max_estimated_input_tokens: 4_000,
                max_retry_count: 2,
                max_concurrent_batches: 1,
            },
            (_, BatchPolicyName::OneTask) => BatchPolicy {
                max_tasks_per_batch: 1,
                max_estimated_input_tokens: 12_000,
                max_retry_count: 2,
                max_concurrent_batches: 1,
            },
            (Provider::Gemini, _) => BatchPolicy::gemini_default(),
            (Provider::OpenAiCompatible, _) => BatchPolicy::local_default(),
        };
        if let Some(value) = self.max_tasks_per_batch {
            policy.max_tasks_per_batch = value;
        }
        if let Some(value) = self.max_input_tokens {
            policy.max_estimated_input_tokens = value;
        }
        if let Some(value) = self.max_concurrent_batches {
            policy.max_concurrent_batches = value;
        }
        policy
    }

    /// provider に対応する API キーを秘密設定から読む。
    pub fn optional_api_key(&self) -> Result<Option<String>, String> {
        let credentials = load_credentials()?;
        let value = match self.provider {
            Provider::Gemini => credentials.gemini_api_key,
            Provider::OpenAiCompatible => credentials.local_llm_api_key,
        };
        Ok(value.filter(|value| !value.trim().is_empty()))
    }

    /// API キーが必須の provider 用。
    pub fn api_key(&self) -> Result<String, String> {
        self.optional_api_key()?.ok_or_else(|| {
            let location = credentials_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "credentials.toml".to_string());
            match self.provider {
                Provider::Gemini => format!(
                    "Gemini APIキーが未設定です。`flowcloze api set --key <API_KEY>` を実行してください ({location})"
                ),
                Provider::OpenAiCompatible => {
                    format!("ローカルLLM APIキーが未設定です ({location})")
                }
            }
        })
    }
}

/// CLI > 標準 config.toml > 組み込み既定値の順に生成設定を解決する。
pub fn load(cli: CliOverrides) -> Result<GenerationConfig, String> {
    let file = load_file()?;
    let provider = parse_provider(
        cli.provider
            .as_deref()
            .or(file.provider.as_deref())
            .unwrap_or("gemini"),
    )?;
    let model = cli
        .model
        .filter(|value| !value.trim().is_empty())
        .or(file.model)
        .unwrap_or_else(|| match provider {
            Provider::Gemini => "gemini-2.5-flash".into(),
            Provider::OpenAiCompatible => "gemma4:e2b-it-qat".into(),
        });
    let base_url = file
        .base_url
        .filter(|value| !value.trim().is_empty())
        .or_else(|| match provider {
            Provider::Gemini => Some(GEMINI_OPENAI_BASE_URL.into()),
            Provider::OpenAiCompatible => None,
        });
    let batch = parse_batch(
        cli.batch
            .as_deref()
            .or(file.batch.as_deref())
            .unwrap_or("auto"),
    )?;
    let rewrite = parse_rewrite(
        cli.rewrite
            .as_deref()
            .or(file.rewrite.as_deref())
            .unwrap_or("always"),
    )?;
    let fallback = parse_fallback(
        cli.fallback
            .as_deref()
            .or(file.fallback.as_deref())
            .unwrap_or("error"),
    )?;
    let structured_output = parse_structured(
        cli.structured_output
            .as_deref()
            .or(file.structured_output.as_deref())
            .unwrap_or("auto"),
    )?;

    Ok(GenerationConfig {
        provider,
        model,
        base_url,
        batch,
        max_tasks_per_batch: positive("max_tasks_per_batch", file.max_tasks_per_batch)?,
        max_input_tokens: positive("max_input_tokens", file.max_input_tokens)?,
        max_concurrent_batches: positive(
            "max_concurrent_batches",
            file.max_concurrent_batches,
        )?,
        rewrite,
        fallback,
        structured_output,
    })
}

/// PDF の既定 Typst テンプレートを標準 config.toml から解決する。
pub fn typst_template_path() -> Result<PathBuf, String> {
    let file = load_file()?;
    Ok(expand_home(
        file.typst_template
            .as_deref()
            .unwrap_or(DEFAULT_TYPST_TEMPLATE),
    ))
}

/// Gemini API キーを標準 credentials.toml に保存する。
pub fn save_gemini_api_key(api_key: &str) -> Result<(), String> {
    if api_key.trim().is_empty() {
        return Err("APIキーを空にはできません".into());
    }
    if api_key.contains(['\r', '\n', '\0']) {
        return Err("APIキーに改行またはNULを含めることはできません".into());
    }

    let directory = config_dir()?;
    fs::create_dir_all(&directory)
        .map_err(|error| format!("{} を作成できませんでした: {error}", directory.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!("{} の権限を設定できませんでした: {error}", directory.display())
        })?;
    }

    let mut credentials = load_credentials()?;
    credentials.gemini_api_key = Some(api_key.to_string());
    let body = toml::to_string_pretty(&credentials)
        .map_err(|_| "credentials.toml を作成できませんでした".to_string())?;
    write_private_file(&credentials_path()?, body.as_bytes())
}

fn load_file() -> Result<FileConfig, String> {
    let path = config_path()?;
    match fs::read_to_string(&path) {
        Ok(text) => {
            toml::from_str(&text).map_err(|error| format!("{} の設定が不正です: {error}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileConfig::default()),
        Err(error) => Err(format!("{} を読めませんでした: {error}", path.display())),
    }
}

fn load_credentials() -> Result<Credentials, String> {
    let path = credentials_path()?;
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text)
            .map_err(|error| format!("{} の設定が不正です: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Credentials::default()),
        Err(error) => Err(format!("{} を読めませんでした: {error}", path.display())),
    }
}

fn nonempty_env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
}

fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return nonempty_env_path("HOME").unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = nonempty_env_path("HOME") {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

fn write_private_file(path: &Path, body: &[u8]) -> Result<(), String> {
    use std::fs::OpenOptions;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let parent = path
        .parent()
        .ok_or_else(|| "credentials.toml の保存先が不正です".to_string())?;
    for _ in 0..16 {
        let mut random = [0u8; 16];
        getrandom::getrandom(&mut random)
            .map_err(|_| "credentials.toml を更新できませんでした".to_string())?;
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let temporary = parent.join(format!(".credentials.{suffix}.tmp"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&temporary) {
            Ok(mut file) => {
                let write_result = file.write_all(body).and_then(|_| file.sync_all());
                if write_result.is_err() {
                    drop(file);
                    let _ = fs::remove_file(&temporary);
                    return Err("credentials.toml を更新できませんでした".into());
                }
                drop(file);
                if fs::rename(&temporary, path).is_err() {
                    let _ = fs::remove_file(&temporary);
                    return Err("credentials.toml を更新できませんでした".into());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                        .map_err(|_| "credentials.toml の権限を設定できませんでした".to_string())?;
                }
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("credentials.toml を更新できませんでした".into()),
        }
    }
    Err("credentials.toml を更新できませんでした".into())
}

fn positive(name: &str, value: Option<usize>) -> Result<Option<usize>, String> {
    match value {
        Some(0) => Err(format!("{name} は1以上にしてください")),
        value => Ok(value),
    }
}

fn parse_provider(value: &str) -> Result<Provider, String> {
    match value.trim() {
        "gemini" => Ok(Provider::Gemini),
        "openai-compatible" | "local" => Ok(Provider::OpenAiCompatible),
        _ => Err("provider は gemini または openai-compatible (local) を指定してください".into()),
    }
}

fn parse_batch(value: &str) -> Result<BatchPolicyName, String> {
    match value.trim() {
        "auto" => Ok(BatchPolicyName::Auto),
        "small" => Ok(BatchPolicyName::Small),
        "one-task" => Ok(BatchPolicyName::OneTask),
        _ => Err("batch は auto, small, one-task のいずれかを指定してください".into()),
    }
}

fn parse_rewrite(value: &str) -> Result<RewritePolicy, String> {
    match value.trim() {
        "always" => Ok(RewritePolicy::Always),
        "never" => Ok(RewritePolicy::Never),
        "auto" => Ok(RewritePolicy::Auto),
        _ => Err("rewrite は always, never, auto のいずれかを指定してください".into()),
    }
}

fn parse_fallback(value: &str) -> Result<FallbackPolicy, String> {
    match value.trim() {
        "error" => Ok(FallbackPolicy::Error),
        "draft" => Ok(FallbackPolicy::Draft),
        _ => Err("fallback は error, draft のいずれかを指定してください".into()),
    }
}

fn parse_structured(value: &str) -> Result<StructuredOutputMode, String> {
    match value.trim() {
        "auto" => Ok(StructuredOutputMode::Auto),
        "on" => Ok(StructuredOutputMode::On),
        "off" => Ok(StructuredOutputMode::Off),
        _ => Err("structured_output は auto, on, off のいずれかを指定してください".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    struct EnvironmentVariable {
        name: &'static str,
        original: Option<OsString>,
    }

    impl EnvironmentVariable {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                original: env::var_os(name),
            }
        }
    }

    impl Drop for EnvironmentVariable {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => env::set_var(self.name, value),
                None => env::remove_var(self.name),
            }
        }
    }

    fn temporary_home(label: &str) -> PathBuf {
        let mut random = [0u8; 8];
        getrandom::getrandom(&mut random).unwrap();
        env::temp_dir().join(format!(
            "flowcloze-{label}-{}",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ))
    }

    #[test]
    fn accepts_only_documented_values_and_positive_numbers() {
        assert_eq!(parse_provider("local").unwrap(), Provider::OpenAiCompatible);
        assert_eq!(
            parse_provider("openai-compatible").unwrap(),
            Provider::OpenAiCompatible
        );
        assert!(parse_provider("openai").is_err());
        assert_eq!(parse_rewrite("auto").unwrap(), RewritePolicy::Auto);
        assert!(parse_fallback("best-effort").is_err());
        assert!(parse_structured("json").is_err());
        assert!(positive("value", Some(0)).is_err());
    }

    #[test]
    fn unknown_config_and_credential_keys_are_rejected() {
        assert!(toml::from_str::<FileConfig>("api_key = 'secret'").is_err());
        assert!(toml::from_str::<FileConfig>("provider = 'gemini'\nunknown = 1").is_err());
        assert!(toml::from_str::<Credentials>("unknown = 'secret'").is_err());
    }

    #[test]
    fn standard_config_uses_xdg_config_home() {
        let _lock = environment_test_lock();
        let _xdg = EnvironmentVariable::new("XDG_CONFIG_HOME");
        let directory = temporary_home("xdg");
        env::set_var("XDG_CONFIG_HOME", &directory);
        assert_eq!(
            config_path().unwrap(),
            directory.join("flowcloze").join("config.toml")
        );
        assert_eq!(
            credentials_path().unwrap(),
            directory.join("flowcloze").join("credentials.toml")
        );
    }

    #[test]
    fn loads_standard_config_and_typst_template() {
        let _lock = environment_test_lock();
        let _xdg = EnvironmentVariable::new("XDG_CONFIG_HOME");
        let directory = temporary_home("config");
        env::set_var("XDG_CONFIG_HOME", &directory);
        let flowcloze = directory.join("flowcloze");
        fs::create_dir_all(&flowcloze).unwrap();
        let template = directory.join("custom.typ");
        fs::write(
            flowcloze.join("config.toml"),
            format!(
                "provider = 'local'\nmodel = 'test-model'\ntypst_template = '{}'\n",
                template.display()
            ),
        )
        .unwrap();
        let config = load(CliOverrides::default()).unwrap();
        assert_eq!(config.provider, Provider::OpenAiCompatible);
        assert_eq!(config.model, "test-model");
        assert_eq!(typst_template_path().unwrap(), template);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn saves_gemini_credentials_privately() {
        let _lock = environment_test_lock();
        let _xdg = EnvironmentVariable::new("XDG_CONFIG_HOME");
        let directory = temporary_home("credentials");
        env::set_var("XDG_CONFIG_HOME", &directory);
        save_gemini_api_key("secret-marker").unwrap();
        let path = credentials_path().unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("gemini_api_key = \"secret-marker\""));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        fs::remove_dir_all(directory).unwrap();
    }
}
'''
(root / 'src/config.rs').write_text(config_rs)

main = (root / 'src/main.rs').read_text()
main = main.replace('    let _ = dotenvy::dotenv();\n\n', '')
main = re.sub(
    r'''        Command::ApiSet \{ api_key \} => \{\n            eprintln!\(\n                "warning: api set は非推奨です。api_key_env が示す環境変数を設定してください。"\n            \);\n            if let Err\(error\) = save_api_settings\(api_key\) \{\n                eprintln!\("\{error\}"\);\n                process::exit\(1\);\n            \}\n            println!\("\.env を更新しました．"\);\n            return;\n        \}''',
    '''        Command::ApiSet { api_key } => {
            if let Err(error) = save_api_settings(api_key) {
                eprintln!("{error}");
                process::exit(1);
            }
            let path = flowcloze::config::credentials_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "credentials.toml".to_string());
            println!("{path} を更新しました．");
            return;
        }''',
    main,
    count=1,
)
main = main.replace(
    '    eprintln!("  api set                APIキーを.envに保存します / Save API key to .env");',
    '    eprintln!("  api set                APIキーを標準credentials.tomlに保存します / Save API key to the standard credentials.toml");',
)
main = re.sub(
    r'''/// Gemini API keyを\.envへ保存する．.*?(?=/// local backendのセットアップ補助を実行する．)''',
    '''/// Gemini API keyを標準 credentials.toml へ保存する．
fn save_api_settings(api_key: &str) -> Result<(), String> {
    flowcloze::config::save_gemini_api_key(api_key)
}

''',
    main,
    count=1,
    flags=re.S,
)
old_pdf = '''fn compile_pdf_file(generated_json_path: &str, output_path: Option<&str>, template_path: &str) {
    let output_pdf_path = output_path
        .map(PathBuf::from)
        .unwrap_or_else(|| default_pdf_output_path(generated_json_path));
    let options = PdfOptions {
        generated_json_path: PathBuf::from(generated_json_path),
        output_pdf_path: output_pdf_path.clone(),
        template_path: PathBuf::from(template_path),
    };'''
new_pdf = '''fn compile_pdf_file(generated_json_path: &str, output_path: Option<&str>, template_path: &str) {
    let output_pdf_path = output_path
        .map(PathBuf::from)
        .unwrap_or_else(|| default_pdf_output_path(generated_json_path));
    let template_path = if template_path == "templates/cloze.typ" {
        flowcloze::config::typst_template_path().unwrap_or_else(|error| {
            eprintln!("{error}");
            process::exit(2)
        })
    } else {
        PathBuf::from(template_path)
    };
    let options = PdfOptions {
        generated_json_path: PathBuf::from(generated_json_path),
        output_pdf_path: output_pdf_path.clone(),
        template_path,
    };'''
if old_pdf not in main:
    raise SystemExit('PDF patch target not found')
main = main.replace(old_pdf, new_pdf, 1)
old_local = '''            Provider::OpenAiCompatible => {
                let adapter = OpenAiCompatiblePool::from_candidates(
                    config.base_url.as_deref(),
                    config.model.clone(),
                    env::var(&config.api_key_env).ok(),
                )
                .with_structured_output(config.structured_output)
                .with_transport(retry_transport.clone());'''
new_local = '''            Provider::OpenAiCompatible => {
                let api_key = config.optional_api_key().unwrap_or_else(|error| {
                    eprintln!("{error}");
                    process::exit(2)
                });
                let adapter = OpenAiCompatiblePool::from_candidates(
                    config.base_url.as_deref(),
                    config.model.clone(),
                    api_key,
                )
                .with_structured_output(config.structured_output)
                .with_transport(retry_transport.clone());'''
if old_local not in main:
    raise SystemExit('local API-key patch target not found')
main = main.replace(old_local, new_local, 1)
(root / 'src/main.rs').write_text(main)

cargo = (root / 'Cargo.toml').read_text()
cargo = cargo.replace('version = "2.0.0-beta"', 'version = "2.1.0-beta"', 1)
cargo = cargo.replace('dotenvy = "0.15"\n', '')
(root / 'Cargo.toml').write_text(cargo)

(root / 'config.toml.example').write_text('''# Copy this file to ~/.config/flowcloze/config.toml
provider = "gemini"
model = "gemini-2.5-flash"
# base_url = "http://localhost:11434/v1"
batch = "auto"
max_tasks_per_batch = 8
max_input_tokens = 12000
max_concurrent_batches = 3
rewrite = "always"
fallback = "error"
structured_output = "auto"

# Use an absolute path when FlowCloze is installed globally.
typst_template = "/absolute/path/to/cloze.typ"
''')

env_example = root / '.env.example'
if env_example.exists():
    env_example.unlink()

readme = (root / 'README.md').read_text()
ja_section = '''## 生成設定

FlowCloze 2.1では、設定をユーザー単位の標準ディレクトリへ集約します。

```text
~/.config/flowcloze/config.toml
~/.config/flowcloze/credentials.toml
```

`XDG_CONFIG_HOME` が設定されている場合は、`$XDG_CONFIG_HOME/flowcloze/` を使います。開発時は例えば次のように分離できます。

```bash
export XDG_CONFIG_HOME="$PWD/.dev-config"
```

通常設定を作る例:

```bash
mkdir -p ~/.config/flowcloze
cp config.toml.example ~/.config/flowcloze/config.toml
```

Gemini APIキーは `config.toml` ではなく、専用の `credentials.toml` に保存します。

```bash
flowcloze api set --key "YOUR_GEMINI_API_KEY"
```

Unix系OSでは `credentials.toml` を `0600`、設定ディレクトリを `0700` で作成します。

`config.toml` の例:

```toml
provider = "gemini"
model = "gemini-2.5-flash"
rewrite = "always"
fallback = "error"
structured_output = "auto"
batch = "auto"
typst_template = "/absolute/path/to/cloze.typ"
```

`typst_template` がPDF生成時の標準テンプレートになります。`flowcloze pdf --template ...` を指定した場合はCLI指定を優先します。

主な `generate` オプション:

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

例:

```bash
flowcloze generate \\
  --provider gemini \\
  --model gemini-2.5-flash \\
  --rewrite auto \\
  --fallback draft \\
  --structured-output auto \\
  --verbose \\
  -o sample/generated.json \\
  sample/sample.md
```

`rewrite`:

- `always`: providerで書き換える
- `never`: providerを呼ばずIdentity生成する
- `auto`: 入力内容に応じて書き換えの要否を選ぶ

`fallback`:

- `error`: 失敗をそのままエラーにする
- `draft`: 通信または内容検証に失敗したtaskをIdentity下書きへ戻す

設定値は **CLI > `~/.config/flowcloze/config.toml` > 組み込み既定値** の順に解決されます。
`.env`、カレントディレクトリの `config.toml`、旧設定用環境変数は自動では読みません。

'''
readme = re.sub(r'## 生成設定\n.*?(?=## ローカルLLM)', ja_section, readme, count=1, flags=re.S)
readme = readme.replace(
    '別のTypstテンプレートを使う場合:\n',
    '標準テンプレートは `~/.config/flowcloze/config.toml` の `typst_template` で指定します。\n一時的に別のTypstテンプレートを使う場合:\n',
    1,
)
(root / 'README.md').write_text(readme)

readme_en = (root / 'README.en.md').read_text()
en_section = '''## Generation Settings

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
flowcloze generate \\
  --provider gemini \\
  --model gemini-2.5-flash \\
  --rewrite auto \\
  --fallback draft \\
  --structured-output auto \\
  --verbose \\
  -o sample/generated.json \\
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

'''
readme_en = re.sub(r'## Generation Settings\n.*?(?=## Local LLM)', en_section, readme_en, count=1, flags=re.S)
readme_en = readme_en.replace(
    'Use another Typst template:\n',
    'Set the default template with `typst_template` in `~/.config/flowcloze/config.toml`.\nFor a one-off override, use:\n',
    1,
)
(root / 'README.en.md').write_text(readme_en)

changelog = (root / 'CHANGELOG.md').read_text()
release = '''# Changelog

## 2.1.0-beta - 2026-09-07

### Added

- Add user-level FlowCloze configuration under `~/.config/flowcloze/` with `XDG_CONFIG_HOME` support.
- Add private `credentials.toml` storage for Gemini API keys through `flowcloze api set`.
- Add `typst_template` as the default PDF template setting.

### Changed

- Resolve generation settings from CLI, the standard user config, then built-in defaults.

### Removed

- Remove automatic `.env`, current-directory `config.toml`, and legacy configuration environment-variable loading.

'''
if not changelog.startswith('# Changelog\n'):
    raise SystemExit('unexpected changelog header')
changelog = release + changelog[len('# Changelog\n\n'):]
(root / 'CHANGELOG.md').write_text(changelog)
