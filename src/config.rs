//! FlowCloze の標準設定と秘密情報を解決する。

pub mod auth_store;
pub mod yaml;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};

use crate::planner::BatchPolicy;
use crate::providers::capability::StructuredOutputMode;
use crate::quota::{QuotaProfile, QuotaProfileConfig};

const GEMINI_OPENAI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
const BUNDLED_TYPST_TEMPLATE: &str = include_str!("../templates/cloze.typ");
const BUNDLED_DEFAULT_CONFIG: &str = include_str!("../config.toml.example");

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
    quota_profile: Option<String>,
    #[serde(default)]
    quota_profiles: HashMap<String, QuotaProfileConfig>,
    batch: Option<String>,
    max_tasks_per_batch: Option<usize>,
    max_input_tokens: Option<usize>,
    max_output_tokens: Option<usize>,
    max_blanks_per_batch: Option<usize>,
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
    #[serde(skip_serializing_if = "Option::is_none", alias = "local_llm_api_key")]
    openai_compatible_api_key: Option<String>,
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
    pub quota: Option<QuotaProfile>,
    pub batch: BatchPolicyName,
    pub max_tasks_per_batch: Option<usize>,
    pub max_input_tokens: Option<usize>,
    pub max_output_tokens: Option<usize>,
    pub max_blanks_per_batch: Option<usize>,
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
                max_estimated_output_tokens: 1_500,
                max_blanks_per_batch: 8,
                max_retry_count: 2,
                max_concurrent_batches: 1,
            },
            (_, BatchPolicyName::OneTask) => BatchPolicy {
                max_tasks_per_batch: 1,
                max_estimated_input_tokens: 12_000,
                max_estimated_output_tokens: 6_000,
                max_blanks_per_batch: 24,
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
        if let Some(value) = self.max_output_tokens {
            policy.max_estimated_output_tokens = value;
        }
        if let Some(value) = self.max_blanks_per_batch {
            policy.max_blanks_per_batch = value;
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
            Provider::OpenAiCompatible => credentials.openai_compatible_api_key,
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
                    "Gemini APIキーが未設定です。`flowcloze api set` を実行してください ({location})"
                ),
                Provider::OpenAiCompatible => format!(
                    "OpenAI互換providerのAPIキーが未設定です。`flowcloze api set` を実行してください ({location})"
                ),
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
    let quota_name = file
        .quota_profile
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut quota = match quota_name {
        Some(name) => {
            let profile = file.quota_profiles.get(&name).ok_or_else(|| {
                format!("quota_profile '{name}' が quota_profiles に定義されていません")
            })?;
            Some(profile.resolve(name, &model)?)
        }
        None => None,
    };
    if batch != BatchPolicyName::Auto {
        if let Some(profile) = quota.as_mut() {
            profile.disable_adaptive_expansion();
        }
    }
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
        quota,
        batch,
        max_tasks_per_batch: positive("max_tasks_per_batch", file.max_tasks_per_batch)?,
        max_input_tokens: positive("max_input_tokens", file.max_input_tokens)?,
        max_output_tokens: positive("max_output_tokens", file.max_output_tokens)?,
        max_blanks_per_batch: positive("max_blanks_per_batch", file.max_blanks_per_batch)?,
        max_concurrent_batches: positive("max_concurrent_batches", file.max_concurrent_batches)?,
        rewrite,
        fallback,
        structured_output,
    })
}

/// PDF のTypstテンプレートを解決する。
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

/// provider に対応する API キーを標準 credentials.toml に保存する。
pub fn save_api_key(provider: Provider, api_key: &str) -> Result<(), String> {
    if api_key.trim().is_empty() {
        return Err("APIキーを空にはできません".into());
    }
    if api_key.contains(['\r', '\n', '\0']) {
        return Err("APIキーに改行またはNULを含めることはできません".into());
    }

    ensure_default_config()?;
    let directory = config_dir()?;
    fs::create_dir_all(&directory)
        .map_err(|error| format!("{} を作成できませんでした: {error}", directory.display()))?;
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

    let mut credentials = load_credentials()?;
    match provider {
        Provider::Gemini => credentials.gemini_api_key = Some(api_key.to_string()),
        Provider::OpenAiCompatible => {
            credentials.openai_compatible_api_key = Some(api_key.to_string())
        }
    }
    let body = toml::to_string_pretty(&credentials)
        .map_err(|_| "credentials.toml を作成できませんでした".to_string())?;
    write_private_file(&credentials_path()?, body.as_bytes())
}

fn ensure_default_config() -> Result<PathBuf, String> {
    use std::fs::OpenOptions;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let directory = config_dir()?;
    fs::create_dir_all(&directory)
        .map_err(|error| format!("{} を作成できませんでした: {error}", directory.display()))?;
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

    let path = directory.join("config.toml");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);

    match options.open(&path) {
        Ok(mut file) => {
            if let Err(error) = file
                .write_all(BUNDLED_DEFAULT_CONFIG.as_bytes())
                .and_then(|_| file.sync_all())
            {
                drop(file);
                let _ = fs::remove_file(&path);
                return Err(format!(
                    "{} を作成できませんでした: {error}",
                    path.display()
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|error| {
                    format!("{} の権限を設定できませんでした: {error}", path.display())
                })?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "{} を作成できませんでした: {error}",
                path.display()
            ));
        }
    }

    Ok(path)
}

fn load_file() -> Result<FileConfig, String> {
    let path = ensure_default_config()?;
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("{} を読めませんでした: {error}", path.display()))?;
    toml::from_str(&text).map_err(|error| format!("{} の設定が不正です: {error}", path.display()))
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
    fn legacy_local_credential_key_is_accepted() {
        let credentials: Credentials =
            toml::from_str("local_llm_api_key = 'legacy-secret'").unwrap();
        assert_eq!(
            credentials.openai_compatible_api_key.as_deref(),
            Some("legacy-secret")
        );
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
        assert_eq!(
            fs::read_to_string(&expected).unwrap(),
            BUNDLED_TYPST_TEMPLATE
        );

        fs::write(&expected, "stale template").unwrap();
        assert_eq!(typst_template_path().unwrap(), expected);
        assert_eq!(
            fs::read_to_string(&expected).unwrap(),
            BUNDLED_TYPST_TEMPLATE
        );
        fs::remove_dir_all(directory).unwrap();
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
    fn loads_quota_profile_and_model_override() {
        let _lock = environment_test_lock();
        let _xdg = EnvironmentVariable::new("XDG_CONFIG_HOME");
        let directory = temporary_home("quota");
        env::set_var("XDG_CONFIG_HOME", &directory);
        let flowcloze = directory.join("flowcloze");
        fs::create_dir_all(&flowcloze).unwrap();
        fs::write(
            flowcloze.join("config.toml"),
            "provider = 'gemini'
model = 'gemini-3.8-flash'
quota_profile = 'gemini'
[quota_profiles.gemini]
rpm = 5
tpm = 250000
rpd = 20
reserve_requests = 4
adaptive_max_tasks_per_batch = 24
adaptive_max_input_tokens = 36000
adaptive_max_output_tokens = 12000
adaptive_max_blanks_per_batch = 48
[quota_profiles.gemini.models.'gemini-3.8-flash']
rpd = 30
",
        )
        .unwrap();
        let config = load(CliOverrides::default()).unwrap();
        let quota = config.quota.unwrap();
        assert_eq!(quota.name, "gemini");
        assert_eq!(quota.rpm, Some(5));
        assert_eq!(quota.rpd, Some(30));
        assert_eq!(quota.request_budget(), Some(26));
        assert_eq!(quota.adaptive_max_tasks_per_batch, Some(24));
        assert_eq!(quota.adaptive_max_output_tokens, Some(12_000));
        assert_eq!(quota.adaptive_max_blanks_per_batch, Some(48));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn missing_standard_config_is_materialized_with_quota_defaults() {
        let _lock = environment_test_lock();
        let _xdg = EnvironmentVariable::new("XDG_CONFIG_HOME");
        let directory = temporary_home("bootstrap-config");
        env::set_var("XDG_CONFIG_HOME", &directory);
        let path = config_path().unwrap();
        assert!(!path.exists());

        let config = load(CliOverrides::default()).unwrap();
        assert_eq!(config.provider, Provider::Gemini);
        assert_eq!(config.model, "gemini-2.5-flash");
        assert_eq!(config.max_tasks_per_batch, Some(12));
        assert_eq!(config.max_input_tokens, Some(18_000));
        assert_eq!(config.max_output_tokens, Some(6_000));
        assert_eq!(config.max_blanks_per_batch, Some(24));
        let quota = config
            .quota
            .expect("default Gemini quota profile should be active");
        assert_eq!(quota.name, "gemini");
        assert_eq!(quota.rpm, Some(5));
        assert_eq!(quota.tpm, Some(250_000));
        assert_eq!(quota.rpd, Some(20));
        assert_eq!(quota.request_budget(), Some(16));
        assert_eq!(quota.adaptive_max_tasks_per_batch, Some(24));
        assert_eq!(quota.adaptive_max_output_tokens, Some(12_000));
        assert_eq!(quota.adaptive_max_blanks_per_batch, Some(48));
        assert_eq!(fs::read_to_string(&path).unwrap(), BUNDLED_DEFAULT_CONFIG);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn saving_api_key_also_materializes_standard_config() {
        let _lock = environment_test_lock();
        let _xdg = EnvironmentVariable::new("XDG_CONFIG_HOME");
        let directory = temporary_home("bootstrap-api-set");
        env::set_var("XDG_CONFIG_HOME", &directory);
        let path = config_path().unwrap();
        assert!(!path.exists());

        save_api_key(Provider::Gemini, "gemini-secret").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), BUNDLED_DEFAULT_CONFIG);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn saves_provider_credentials_privately() {
        let _lock = environment_test_lock();
        let _xdg = EnvironmentVariable::new("XDG_CONFIG_HOME");
        let directory = temporary_home("credentials");
        env::set_var("XDG_CONFIG_HOME", &directory);
        save_api_key(Provider::Gemini, "gemini-secret").unwrap();
        save_api_key(Provider::OpenAiCompatible, "openai-secret").unwrap();
        let path = credentials_path().unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("gemini_api_key = \"gemini-secret\""));
        assert!(body.contains("openai_compatible_api_key = \"openai-secret\""));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }
}
