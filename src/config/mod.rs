//! FlowClozeのYAML設定、model catalog、秘密情報を解決する。

pub mod app_config;
pub mod auth_store;
pub mod model_file;
pub mod quota;

pub use app_config::{AppConfig, BatchSettings, GenerationSettings, QuotaSettings};
pub use model_file::{load_catalogs, upsert_model_yaml, ModelFile};

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::planner::{BatchPolicy, ComposeExecutionPolicy};
use crate::providers::model_registry::ResolvedModel;
use crate::quota::QuotaProfile;

const BUNDLED_TYPST_TEMPLATE: &str = include_str!("../../templates/cloze.typ");

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

#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub model: Option<String>,
    pub fallback: Option<String>,
    pub batch: Option<String>,
    pub offline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationConfig {
    pub model: Option<ResolvedModel>,
    pub quota: Option<QuotaProfile>,
    pub batch: BatchPolicyName,
    pub max_retries: u32,
    pub fallback: FallbackPolicy,
    pub offline: bool,
}

#[doc(hidden)]
pub fn environment_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

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
    Err(
        "FlowClozeの設定ディレクトリを決定できません。HOMEまたはXDG_CONFIG_HOMEを設定してください"
            .into(),
    )
}

pub fn config_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("config.yaml"))
}

pub fn model_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("model.yaml"))
}

impl GenerationConfig {
    pub fn batch_policy(&self) -> BatchPolicy {
        let local = self
            .model
            .as_ref()
            .is_some_and(|model| model.provider == "ollama");
        let mut policy = match self.batch {
            BatchPolicyName::Small => BatchPolicy::local_default(),
            BatchPolicyName::OneTask => BatchPolicy {
                max_tasks_per_batch: 1,
                max_estimated_input_tokens: 12_000,
                max_estimated_output_tokens: 6_000,
                max_blanks_per_batch: 24,
                max_retry_count: self.max_retries,
                max_concurrent_batches: 1,
            },
            BatchPolicyName::Auto if local => BatchPolicy::local_default(),
            BatchPolicyName::Auto => BatchPolicy::gemini_default(),
        };
        policy.max_retry_count = self.max_retries;
        policy
    }

    pub fn execution_policy(&self) -> ComposeExecutionPolicy {
        ComposeExecutionPolicy {
            batch_policy: self.batch_policy(),
            max_content_retries: self.max_retries,
        }
    }
}

/// CLI > config.yaml > built-in defaults の順に生成設定を解決する。
pub fn load(cli: CliOverrides) -> Result<GenerationConfig, String> {
    let app = app_config::load_app_config(&config_path()?)?;
    let (providers, models) = model_file::load_catalogs(&model_path()?)?;
    let profile = cli
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&app.default_model);
    let model = if cli.offline {
        None
    } else {
        Some(
            models
                .resolve(profile, &providers)
                .map_err(|error| error.to_string())?,
        )
    };
    let batch = parse_batch(cli.batch.as_deref().unwrap_or(&app.batch.mode))?;
    let fallback = parse_fallback(cli.fallback.as_deref().unwrap_or(&app.generation.fallback))?;
    let quota = if cli.offline {
        None
    } else {
        let resolved = model.as_ref().expect("online config has a model");
        app.quotas
            .get(profile)
            .or_else(|| app.quotas.get(&resolved.provider))
            .map(|settings| settings.resolve(profile))
            .transpose()?
    };
    Ok(GenerationConfig {
        model,
        quota,
        batch,
        max_retries: app.batch.max_retries,
        fallback,
        offline: cli.offline,
    })
}

pub fn typst_template_path() -> Result<PathBuf, String> {
    let app = app_config::load_app_config(&config_path()?)?;
    let managed = config_dir()?.join("templates").join("cloze.typ");
    match app.typst_template.as_deref() {
        Some(value) if !value.trim().is_empty() => {
            let selected = expand_home(value);
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
    let parent = path
        .parent()
        .ok_or_else(|| "Typst template path is invalid".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("{}: {error}", directory.display()))?;
    }
    if fs::read_to_string(path).ok().as_deref() != Some(BUNDLED_TYPST_TEMPLATE) {
        fs::write(path, BUNDLED_TYPST_TEMPLATE)
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(())
}

fn parse_batch(value: &str) -> Result<BatchPolicyName, String> {
    match value.trim() {
        "auto" => Ok(BatchPolicyName::Auto),
        "small" => Ok(BatchPolicyName::Small),
        "one-task" => Ok(BatchPolicyName::OneTask),
        _ => Err("batch.mode must be auto, small, or one-task".into()),
    }
}

fn parse_fallback(value: &str) -> Result<FallbackPolicy, String> {
    match value.trim() {
        "error" => Ok(FallbackPolicy::Error),
        "draft" => Ok(FallbackPolicy::Draft),
        _ => Err("generation.fallback must be error or draft".into()),
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
    value
        .strip_prefix("~/")
        .and_then(|rest| nonempty_env_path("HOME").map(|home| home.join(rest)))
        .unwrap_or_else(|| PathBuf::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_paths_are_yaml() {
        let _lock = environment_test_lock();
        let old = env::var_os("XDG_CONFIG_HOME");
        let root = env::temp_dir().join(format!("flowcloze-config-{}", std::process::id()));
        env::set_var("XDG_CONFIG_HOME", &root);
        assert!(config_path().unwrap().ends_with("flowcloze/config.yaml"));
        assert!(model_path().unwrap().ends_with("flowcloze/model.yaml"));
        match old {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn yaml_generation_settings_and_cli_overrides_are_connected() {
        let _lock = environment_test_lock();
        let old = env::var_os("XDG_CONFIG_HOME");
        let root = env::temp_dir().join(format!("flowcloze-resolve-{}", std::process::id()));
        let directory = root.join("flowcloze");
        fs::create_dir_all(&directory).unwrap();
        env::set_var("XDG_CONFIG_HOME", &root);
        fs::write(
            directory.join("config.yaml"),
            "default_model: gemini-flash\ngeneration:\n  fallback: error\nbatch:\n  mode: small\n  max_retries: 5\nquotas:\n  gemini-flash:\n    rpm: 7\n    tpm: 9000\n",
        )
        .unwrap();
        let config = load(CliOverrides {
            batch: Some("one-task".into()),
            fallback: Some("draft".into()),
            ..CliOverrides::default()
        })
        .unwrap();
        assert_eq!(config.batch, BatchPolicyName::OneTask);
        assert_eq!(config.fallback, FallbackPolicy::Draft);
        assert_eq!(config.quota.as_ref().unwrap().rpm, Some(7));
        assert_eq!(config.execution_policy().max_content_retries, 5);
        assert_eq!(config.batch_policy().max_retry_count, 5);
        fs::remove_dir_all(root).unwrap();
        match old {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}
