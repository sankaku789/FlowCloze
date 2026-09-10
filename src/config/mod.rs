//! FlowClozeのYAML設定、model catalog、秘密情報を解決する。

pub mod app_config;
pub mod auth_store;
pub mod model_file;
pub mod quota;

pub use app_config::{
    AppConfig, BatchProfileSettings, BatchSettings, GenerationSettings, QuotaSettings,
};
pub use model_file::{load_catalogs, upsert_model_yaml, ModelFile};

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::planner::{BatchPolicy, ComposeExecutionPolicy};
use crate::providers::model_registry::ResolvedModel;
use crate::quota::QuotaProfile;

const BUNDLED_TYPST_TEMPLATE: &str = include_str!("../../templates/cloze.typ");
const BUNDLED_APP_CONFIG: &str = include_str!("../../config.yaml.example");
const BUNDLED_MODEL_FILE: &str = include_str!("../../model.yaml.example");
const LEGACY_APP_CONFIG: &str = "default_model: gemini-flash\ngeneration:\n  fallback: draft\nbatch:\n  mode: auto\n  max_retries: 2\nquotas:\n  gemini-flash:\n    rpm: 5\n    tpm: 250000\n# typst_template: /path/to/custom.typ\n";
const LEGACY_APP_CONFIG_V2: &str = "default_model: gemini-flash\ngeneration:\n  fallback: draft\nbatch:\n  mode: auto\n  max_retries: 2\n  max_tasks_per_batch: 5\n  max_input_tokens: 18000\n  max_output_tokens: 6000\n  max_blanks_per_batch: 52\n  max_concurrent_batches: 1\nquotas:\n  gemini-flash:\n    rpm: 4\n    tpm: 250000\n    rpd: 20\n    reserve_requests: 10\n    adaptive_max_tasks_per_batch: 6\n    adaptive_max_input_tokens: 18000\n    adaptive_max_output_tokens: 6000\n    adaptive_max_blanks_per_batch: 60\n# typst_template: /path/to/custom.typ\n";
const LEGACY_EMPTY_MODEL_FILE: &str = "# Built-in providers (google and ollama) and gemini-flash are always available.\n# Add or override provider and model profiles below.\nproviders: {}\nmodels: {}\n";

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
    pub batch_settings: BatchSettings,
    pub fallback: FallbackPolicy,
    pub offline: bool,
    resolved_batch_policy: BatchPolicy,
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

/// Missing user-editable YAML files are materialized without replacing existing settings.
pub fn ensure_default_files() -> Result<(), String> {
    let directory = config_dir()?;
    fs::create_dir_all(&directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("{}: {error}", directory.display()))?;
    }
    let config_path = directory.join("config.yaml");
    let existing_config = fs::read_to_string(&config_path).ok();
    if matches!(
        existing_config.as_deref(),
        Some(LEGACY_APP_CONFIG | LEGACY_APP_CONFIG_V2)
    ) {
        replace_managed_file(&config_path, BUNDLED_APP_CONFIG)?;
    } else {
        create_config_file(&config_path, BUNDLED_APP_CONFIG)?;
    }
    let model_path = directory.join("model.yaml");
    if fs::read_to_string(&model_path).ok().as_deref() == Some(LEGACY_EMPTY_MODEL_FILE) {
        replace_managed_file(&model_path, BUNDLED_MODEL_FILE)?;
    } else {
        create_config_file(&model_path, BUNDLED_MODEL_FILE)?;
    }
    Ok(())
}

fn replace_managed_file(path: &Path, body: &str) -> Result<(), String> {
    fs::write(path, body).map_err(|error| format!("{}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(())
}

fn create_config_file(path: &Path, body: &str) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(body.as_bytes())
                .map_err(|error| format!("{}: {error}", path.display()))?;
            file.sync_all()
                .map_err(|error| format!("{}: {error}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

impl GenerationConfig {
    pub fn batch_policy(&self) -> BatchPolicy {
        self.resolved_batch_policy
    }

    pub fn execution_policy(&self) -> ComposeExecutionPolicy {
        ComposeExecutionPolicy {
            batch_policy: self.resolved_batch_policy,
            max_content_retries: self.max_retries,
        }
    }
}

fn resolve_batch_policy(
    batch: BatchPolicyName,
    settings: &BatchSettings,
    model: Option<&ResolvedModel>,
) -> Result<BatchPolicy, String> {
    let profile_name = match batch {
        BatchPolicyName::Small => "local",
        BatchPolicyName::Auto | BatchPolicyName::OneTask => model
            .and_then(|model| settings.provider_profiles.get(&model.provider))
            .map(String::as_str)
            .unwrap_or(&settings.default_profile),
    };
    let profile = settings.profiles.get(profile_name).ok_or_else(|| {
        format!("batch profile '{profile_name}' is not defined in batch.profiles")
    })?;
    let values = [
        profile.max_tasks_per_batch,
        profile.max_input_tokens,
        profile.max_output_tokens,
        profile.max_blanks_per_batch,
        profile.max_concurrent_batches,
    ];
    if values.contains(&0) {
        return Err(format!(
            "batch profile '{profile_name}' limits must be greater than zero"
        ));
    }

    let mut policy = BatchPolicy {
        max_tasks_per_batch: profile.max_tasks_per_batch,
        max_estimated_input_tokens: profile.max_input_tokens,
        max_estimated_output_tokens: profile.max_output_tokens,
        max_blanks_per_batch: profile.max_blanks_per_batch,
        max_concurrent_batches: profile.max_concurrent_batches,
    };
    apply_batch_overrides(&mut policy, settings)?;
    if batch == BatchPolicyName::OneTask {
        policy.max_tasks_per_batch = 1;
        policy.max_concurrent_batches = 1;
    }
    Ok(policy)
}

fn apply_batch_overrides(policy: &mut BatchPolicy, settings: &BatchSettings) -> Result<(), String> {
    let overrides = [
        ("max_tasks_per_batch", settings.max_tasks_per_batch),
        ("max_input_tokens", settings.max_input_tokens),
        ("max_output_tokens", settings.max_output_tokens),
        ("max_blanks_per_batch", settings.max_blanks_per_batch),
        ("max_concurrent_batches", settings.max_concurrent_batches),
    ];
    if let Some((name, _)) = overrides.iter().find(|(_, value)| *value == Some(0)) {
        return Err(format!("batch.{name} must be greater than zero"));
    }
    if let Some(value) = settings.max_tasks_per_batch {
        policy.max_tasks_per_batch = value;
    }
    if let Some(value) = settings.max_input_tokens {
        policy.max_estimated_input_tokens = value;
    }
    if let Some(value) = settings.max_output_tokens {
        policy.max_estimated_output_tokens = value;
    }
    if let Some(value) = settings.max_blanks_per_batch {
        policy.max_blanks_per_batch = value;
    }
    if let Some(value) = settings.max_concurrent_batches {
        policy.max_concurrent_batches = value;
    }
    Ok(())
}

/// CLI > config.yaml > bundled defaults の順に生成設定を解決する。
pub fn load(cli: CliOverrides) -> Result<GenerationConfig, String> {
    ensure_default_files()?;
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
    let resolved_batch_policy = resolve_batch_policy(batch, &app.batch, model.as_ref())?;
    Ok(GenerationConfig {
        model,
        quota,
        batch,
        max_retries: app.batch.max_retries,
        batch_settings: app.batch,
        fallback,
        offline: cli.offline,
        resolved_batch_policy,
    })
}

pub fn typst_template_path() -> Result<PathBuf, String> {
    ensure_default_files()?;
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
        assert_eq!(config.batch_policy().max_tasks_per_batch, 1);
        fs::remove_dir_all(root).unwrap();
        match old {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn missing_yaml_files_are_materialized_without_overwriting_existing_config() {
        let _lock = environment_test_lock();
        let old = env::var_os("XDG_CONFIG_HOME");
        let root = env::temp_dir().join(format!("flowcloze-materialize-{}", std::process::id()));
        let directory = root.join("flowcloze");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("config.yaml"), "default_model: custom\n").unwrap();
        env::set_var("XDG_CONFIG_HOME", &root);

        ensure_default_files().unwrap();

        assert_eq!(
            fs::read_to_string(directory.join("config.yaml")).unwrap(),
            "default_model: custom\n"
        );
        assert_eq!(
            fs::read_to_string(directory.join("model.yaml")).unwrap(),
            BUNDLED_MODEL_FILE
        );
        let (providers, models) = model_file::load_catalogs(&directory.join("model.yaml")).unwrap();
        assert_eq!(
            providers.get("google").unwrap().auth,
            crate::AuthRequirement::ApiKey
        );
        assert_eq!(
            providers.get("ollama").unwrap().auth,
            crate::AuthRequirement::None
        );
        assert_eq!(
            models.resolve("gemini-flash", &providers).unwrap().model,
            "gemini-2.5-flash"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(directory.join("model.yaml"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(root).unwrap();
        match old {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn legacy_empty_model_file_is_replaced_with_builtin_definitions() {
        let _lock = environment_test_lock();
        let old = env::var_os("XDG_CONFIG_HOME");
        let root =
            env::temp_dir().join(format!("flowcloze-model-migration-{}", std::process::id()));
        let directory = root.join("flowcloze");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("model.yaml"), LEGACY_EMPTY_MODEL_FILE).unwrap();
        env::set_var("XDG_CONFIG_HOME", &root);

        ensure_default_files().unwrap();

        assert_eq!(
            fs::read_to_string(directory.join("model.yaml")).unwrap(),
            BUNDLED_MODEL_FILE
        );
        fs::remove_dir_all(root).unwrap();
        match old {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn bundled_config_applies_remote_batch_and_quota_limits() {
        let _lock = environment_test_lock();
        let old = env::var_os("XDG_CONFIG_HOME");
        let root = env::temp_dir().join(format!("flowcloze-bundled-limits-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        env::set_var("XDG_CONFIG_HOME", &root);

        let config = load(CliOverrides::default()).unwrap();
        let policy = config.batch_policy();
        assert_eq!(policy.max_tasks_per_batch, 5);
        assert_eq!(policy.max_estimated_input_tokens, 18_000);
        assert_eq!(policy.max_estimated_output_tokens, 6_000);
        assert_eq!(policy.max_blanks_per_batch, 52);
        assert_eq!(policy.max_concurrent_batches, 1);
        let quota = config.quota.unwrap();
        assert_eq!(quota.rpm, Some(4));
        assert_eq!(quota.tpm, Some(250_000));
        assert_eq!(quota.rpd, Some(20));
        assert_eq!(quota.request_budget(), Some(10));
        assert_eq!(quota.adaptive_max_tasks_per_batch, Some(6));
        assert_eq!(quota.adaptive_max_input_tokens, Some(18_000));
        assert_eq!(quota.adaptive_max_output_tokens, Some(6_000));
        assert_eq!(quota.adaptive_max_blanks_per_batch, Some(60));

        fs::remove_dir_all(root).unwrap();
        match old {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn ollama_auto_uses_local_profile_from_config() {
        let _lock = environment_test_lock();
        let old = env::var_os("XDG_CONFIG_HOME");
        let root = env::temp_dir().join(format!("flowcloze-local-profile-{}", std::process::id()));
        let directory = root.join("flowcloze");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&directory).unwrap();
        env::set_var("XDG_CONFIG_HOME", &root);
        fs::write(
            directory.join("model.yaml"),
            "models:\n  local-test:\n    provider: ollama\n    model: local-model\n",
        )
        .unwrap();

        let config = load(CliOverrides {
            model: Some("local-test".into()),
            ..CliOverrides::default()
        })
        .unwrap();
        let policy = config.batch_policy();
        assert_eq!(policy.max_tasks_per_batch, 2);
        assert_eq!(policy.max_estimated_input_tokens, 4_000);
        assert_eq!(policy.max_estimated_output_tokens, 1_500);
        assert_eq!(policy.max_blanks_per_batch, 8);
        assert_eq!(policy.max_concurrent_batches, 1);

        fs::remove_dir_all(root).unwrap();
        match old {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}
