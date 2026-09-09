use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::providers::builtins::{builtin_models, builtin_providers, DEFAULT_MODEL};
use crate::providers::catalog::{ProviderCatalog, ProviderDefinition};
use crate::providers::model_registry::{ModelProfile, ModelRegistry};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelFile {
    #[serde(default)]
    providers: HashMap<String, ProviderDefinition>,
    #[serde(default)]
    models: HashMap<String, ModelProfile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default = "default_model")]
    pub default_model: String,
    #[serde(default)]
    pub quotas: HashMap<String, QuotaSettings>,
    #[serde(default)]
    pub generation: GenerationSettings,
    #[serde(default)]
    pub batch: BatchSettings,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationSettings {
    #[serde(default = "default_fallback")]
    pub fallback: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchSettings {
    #[serde(default = "default_batch_mode")]
    pub mode: String,
    #[serde(default = "default_retries")]
    pub max_retries: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaSettings {
    pub rpm: Option<u32>,
    pub tpm: Option<u32>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            default_model: default_model(),
            quotas: HashMap::new(),
            generation: GenerationSettings::default(),
            batch: BatchSettings::default(),
        }
    }
}

impl Default for GenerationSettings {
    fn default() -> Self {
        Self {
            fallback: default_fallback(),
        }
    }
}

impl Default for BatchSettings {
    fn default() -> Self {
        Self {
            mode: default_batch_mode(),
            max_retries: default_retries(),
        }
    }
}

pub fn load_catalogs(path: &Path) -> Result<(ProviderCatalog, ModelRegistry), String> {
    let mut providers = builtin_providers();
    let mut models = builtin_models();
    if !path.exists() {
        return Ok((providers, models));
    }
    let file: ModelFile =
        serde_yaml::from_str(&fs::read_to_string(path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("invalid model.yaml: {error}"))?;
    for (id, mut provider) in file.providers {
        provider.id = id;
        providers
            .upsert(provider)
            .map_err(|error| error.to_string())?;
    }
    for (name, mut model) in file.models {
        model.name = name;
        models.upsert(model).map_err(|error| error.to_string())?;
    }
    Ok((providers, models))
}

pub fn load_app_config(path: &Path) -> Result<AppConfig, String> {
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    serde_yaml::from_str(&fs::read_to_string(path).map_err(|error| error.to_string())?)
        .map_err(|error| format!("invalid config.yaml: {error}"))
}

fn default_model() -> String {
    DEFAULT_MODEL.into()
}
fn default_fallback() -> String {
    "draft".into()
}
fn default_batch_mode() -> String {
    "auto".into()
}
fn default_retries() -> u32 {
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_yaml_extends_builtin_catalog() {
        let directory = std::env::temp_dir().join(format!(
            "flowcloze-model-yaml-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("model.yaml");
        fs::write(
            &path,
            "models:\n  local-qwen:\n    provider: ollama\n    model: qwen3:14b\n",
        )
        .unwrap();
        let (providers, models) = load_catalogs(&path).unwrap();
        assert!(models.resolve("gemini-flash", &providers).is_ok());
        assert_eq!(
            models.resolve("local-qwen", &providers).unwrap().model,
            "qwen3:14b"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
