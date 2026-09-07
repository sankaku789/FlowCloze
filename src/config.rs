//! CLI と設定ファイルの生成設定を一箇所で解決する。

use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::Deserialize;

use crate::planner::BatchPolicy;
use crate::providers::capability::StructuredOutputMode;

const GEMINI_OPENAI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";

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
    api_key_env: Option<String>,
    base_url: Option<String>,
    batch: Option<String>,
    max_tasks_per_batch: Option<usize>,
    max_input_tokens: Option<usize>,
    max_concurrent_batches: Option<usize>,
    rewrite: Option<String>,
    fallback: Option<String>,
    structured_output: Option<String>,
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
    pub api_key_env: String,
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
///
/// `main` など別crateのテストも同じ環境変数を触る場合にこの関数を使う。
#[doc(hidden)]
pub fn environment_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
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

    /// APIキーは必要になる直前まで読まない。
    pub fn api_key(&self) -> Result<String, String> {
        env::var(&self.api_key_env)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("{} が未設定です", self.api_key_env))
    }
}

pub fn load(cli: CliOverrides) -> Result<GenerationConfig, String> {
    let file = load_file()?;
    // canonical > legacy > file > default. 空の環境変数は未指定として扱う。
    let provider = parse_provider(
        value(
            &cli.provider,
            "FLOWCLOZE_PROVIDER",
            Some("FLOWCLOZE_LLM_BACKEND"),
            file.provider.as_deref(),
        )
        .as_deref()
        .unwrap_or("gemini"),
    )?;
    let model =
        value(&cli.model, "FLOWCLOZE_MODEL", None, file.model.as_deref()).unwrap_or_else(|| {
            match provider {
                Provider::Gemini => "gemini-2.5-flash".into(),
                Provider::OpenAiCompatible => "gemma4:e2b-it-qat".into(),
            }
        });
    let api_key_env = env_value("FLOWCLOZE_API_KEY_ENV")
        .or(file.api_key_env)
        .unwrap_or_else(|| match provider {
            Provider::Gemini => "GEMINI_API_KEY".into(),
            Provider::OpenAiCompatible => "LOCAL_LLM_API_KEY".into(),
        });
    let base_url = env_value("FLOWCLOZE_BASE_URL")
        .or_else(|| env_value("LOCAL_LLM_BASE_URL"))
        .or(file.base_url)
        .filter(|x| !x.trim().is_empty())
        .or_else(|| match provider {
            Provider::Gemini => Some(GEMINI_OPENAI_BASE_URL.into()),
            Provider::OpenAiCompatible => None,
        });
    let batch = parse_batch(
        value(
            &cli.batch,
            "FLOWCLOZE_BATCH_POLICY",
            None,
            file.batch.as_deref(),
        )
        .as_deref()
        .unwrap_or("auto"),
    )?;
    let rewrite = parse_rewrite(
        value(
            &cli.rewrite,
            "FLOWCLOZE_REWRITE",
            None,
            file.rewrite.as_deref(),
        )
        .as_deref()
        .unwrap_or("always"),
    )?;
    let fallback = parse_fallback(
        value(
            &cli.fallback,
            "FLOWCLOZE_FALLBACK",
            None,
            file.fallback.as_deref(),
        )
        .as_deref()
        .unwrap_or("error"),
    )?;
    let structured_output = parse_structured(
        value(
            &cli.structured_output,
            "FLOWCLOZE_STRUCTURED_OUTPUT",
            None,
            file.structured_output.as_deref(),
        )
        .as_deref()
        .unwrap_or("auto"),
    )?;
    Ok(GenerationConfig {
        provider,
        model,
        api_key_env,
        base_url,
        batch,
        max_tasks_per_batch: number("FLOWCLOZE_MAX_TASKS_PER_BATCH", file.max_tasks_per_batch)?,
        max_input_tokens: number("FLOWCLOZE_MAX_INPUT_TOKENS", file.max_input_tokens)?,
        max_concurrent_batches: number(
            "FLOWCLOZE_MAX_CONCURRENT_BATCHES",
            file.max_concurrent_batches,
        )?,
        rewrite,
        fallback,
        structured_output,
    })
}

fn load_file() -> Result<FileConfig, String> {
    // 空値は他の設定環境変数と同様に未指定として扱う。
    let explicit_path = env::var_os("FLOWCLOZE_CONFIG")
        .filter(|value| !value.to_string_lossy().trim().is_empty())
        .map(PathBuf::from);
    let path = explicit_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("config.toml"));
    match fs::read_to_string(&path) {
        Ok(text) => {
            toml::from_str(&text).map_err(|e| format!("{} の設定が不正です: {e}", path.display()))
        }
        // cwd の既定設定だけは任意だが、明示指定の打ち間違いは隠さない。
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit_path.is_none() => {
            Ok(FileConfig::default())
        }
        Err(e) => Err(format!("{} を読めませんでした: {e}", path.display())),
    }
}
fn env_value(name: &str) -> Option<String> {
    env::var(name).ok().filter(|x| !x.trim().is_empty())
}
fn value(
    cli: &Option<String>,
    canonical: &str,
    legacy: Option<&str>,
    file: Option<&str>,
) -> Option<String> {
    cli.clone()
        .filter(|x| !x.trim().is_empty())
        .or_else(|| env_value(canonical))
        .or_else(|| legacy.and_then(env_value))
        .or_else(|| file.map(str::to_string))
}
fn number(name: &str, file: Option<usize>) -> Result<Option<usize>, String> {
    let value = env_value(name)
        .map(|x| {
            x.parse::<usize>()
                .map_err(|_| format!("{name} には1以上の整数を指定してください"))
        })
        .transpose()?
        .or(file);
    match value {
        Some(0) => Err(format!("{name} は1以上にしてください")),
        x => Ok(x),
    }
}
fn parse_provider(v: &str) -> Result<Provider, String> {
    match v.trim() {
        "gemini" => Ok(Provider::Gemini),
        "openai-compatible" | "local" => Ok(Provider::OpenAiCompatible),
        _ => Err("provider は gemini または openai-compatible (local) を指定してください".into()),
    }
}
fn parse_batch(v: &str) -> Result<BatchPolicyName, String> {
    match v.trim() {
        "auto" => Ok(BatchPolicyName::Auto),
        "small" => Ok(BatchPolicyName::Small),
        "one-task" => Ok(BatchPolicyName::OneTask),
        _ => Err("batch は auto, small, one-task のいずれかを指定してください".into()),
    }
}
fn parse_rewrite(v: &str) -> Result<RewritePolicy, String> {
    match v.trim() {
        "always" => Ok(RewritePolicy::Always),
        "never" => Ok(RewritePolicy::Never),
        "auto" => Ok(RewritePolicy::Auto),
        _ => Err("rewrite は always, never, auto のいずれかを指定してください".into()),
    }
}
fn parse_fallback(v: &str) -> Result<FallbackPolicy, String> {
    match v.trim() {
        "error" => Ok(FallbackPolicy::Error),
        "draft" => Ok(FallbackPolicy::Draft),
        _ => Err("fallback は error, draft のいずれかを指定してください".into()),
    }
}
fn parse_structured(v: &str) -> Result<StructuredOutputMode, String> {
    match v.trim() {
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
                original: std::env::var_os(name),
            }
        }
    }

    impl Drop for EnvironmentVariable {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
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
        assert!(number("FLOWCLOZE_TEST_ZERO", Some(0)).is_err());
    }

    #[test]
    fn unknown_config_key_is_rejected_including_secret_lookalikes() {
        assert!(toml::from_str::<FileConfig>("api_key = 'secret'").is_err());
        assert!(toml::from_str::<FileConfig>("provider = 'gemini'\nunknown = 1").is_err());
    }

    #[test]
    fn explicit_missing_config_is_an_error() {
        let _lock = environment_test_lock();
        let _config = EnvironmentVariable::new("FLOWCLOZE_CONFIG");
        let path =
            std::env::temp_dir().join(format!("flowcloze-missing-config-{}", std::process::id()));
        std::env::set_var("FLOWCLOZE_CONFIG", &path);
        let result = load_file();
        assert!(result.is_err());
    }

    #[test]
    fn empty_config_environment_uses_the_implicit_default() {
        let _lock = environment_test_lock();
        let _config = EnvironmentVariable::new("FLOWCLOZE_CONFIG");
        for value in [None, Some(""), Some(" \t ")] {
            match value {
                Some(value) => std::env::set_var("FLOWCLOZE_CONFIG", value),
                None => std::env::remove_var("FLOWCLOZE_CONFIG"),
            }
            assert!(load_file().is_ok());
        }
    }
}
