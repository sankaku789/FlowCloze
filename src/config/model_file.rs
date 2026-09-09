use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::providers::builtins::{builtin_models, builtin_providers};
use crate::providers::catalog::{ProviderCatalog, ProviderDefinition};
use crate::providers::model_registry::{ModelProfile, ModelRegistry};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile {
    #[serde(default)]
    providers: BTreeMap<String, ProviderDefinition>,
    #[serde(default)]
    models: BTreeMap<String, ModelProfile>,
}

pub fn load_catalogs(path: &Path) -> Result<(ProviderCatalog, ModelRegistry), String> {
    let mut providers = builtin_providers();
    let mut models = builtin_models();
    if !path.exists() {
        return Ok((providers, models));
    }
    let file: ModelFile =
        serde_yaml::from_str(&fs::read_to_string(path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("invalid model.yaml: {e}"))?;
    for (id, mut provider) in file.providers {
        provider.id = id;
        providers.upsert(provider).map_err(|e| e.to_string())?;
    }
    for (name, mut model) in file.models {
        model.name = name;
        models.upsert(model).map_err(|e| e.to_string())?;
    }
    Ok((providers, models))
}

pub fn upsert_model_yaml(
    path: &Path,
    name: &str,
    provider: &str,
    model: &str,
) -> Result<(), String> {
    let (name, provider, model) = (name.trim(), provider.trim(), model.trim());
    if name.is_empty() || provider.is_empty() || model.is_empty() {
        return Err("model name, provider, and provider model must not be empty".into());
    }
    let mut file = if path.exists() {
        serde_yaml::from_str::<ModelFile>(&fs::read_to_string(path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("invalid model.yaml: {e}"))?
    } else {
        ModelFile {
            providers: BTreeMap::new(),
            models: BTreeMap::new(),
        }
    };
    file.models.insert(
        name.to_string(),
        ModelProfile {
            name: String::new(),
            provider: provider.to_string(),
            model: model.to_string(),
        },
    );
    write_yaml_file(
        path,
        &serde_yaml::to_string(&file).map_err(|e| e.to_string())?,
    )
}

fn write_yaml_file(path: &Path, body: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "model.yaml path is invalid".to_string())?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = PathBuf::from(format!("{}.tmp", path.display()));
    let mut file = fs::File::create(&temporary).map_err(|e| e.to_string())?;
    file.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    fs::rename(temporary, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_yaml_extends_builtin_catalog() {
        let directory =
            std::env::temp_dir().join(format!("flowcloze-model-yaml-{}", std::process::id()));
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

    #[test]
    fn model_upsert_is_repeatable_and_contains_no_secrets_or_urls() {
        let directory =
            std::env::temp_dir().join(format!("flowcloze-model-upsert-{}", std::process::id()));
        let path = directory.join("model.yaml");
        upsert_model_yaml(&path, "qwen", "ollama", "qwen3:8b").unwrap();
        upsert_model_yaml(&path, "qwen", "ollama", "qwen3:14b").unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(!body.contains("base_url"));
        assert!(!body.contains("api_key"));
        let (providers, models) = load_catalogs(&path).unwrap();
        assert_eq!(
            models.resolve("qwen", &providers).unwrap().model,
            "qwen3:14b"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
