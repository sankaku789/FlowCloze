use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::providers::builtins::DEFAULT_MODEL;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default = "default_model")]
    pub default_model: String,
    #[serde(default)]
    pub quotas: BTreeMap<String, QuotaSettings>,
    #[serde(default)]
    pub generation: GenerationSettings,
    #[serde(default)]
    pub batch: BatchSettings,
    pub typst_template: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationSettings {
    #[serde(default = "default_fallback")]
    pub fallback: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchSettings {
    #[serde(default = "default_batch_mode")]
    pub mode: String,
    #[serde(default = "default_retries")]
    pub max_retries: u32,
    pub max_tasks_per_batch: Option<usize>,
    pub max_input_tokens: Option<usize>,
    pub max_output_tokens: Option<usize>,
    pub max_blanks_per_batch: Option<usize>,
    pub max_concurrent_batches: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaSettings {
    pub rpm: Option<u32>,
    pub tpm: Option<u32>,
    pub rpd: Option<u32>,
    #[serde(default)]
    pub reserve_requests: u32,
    pub adaptive_max_tasks_per_batch: Option<usize>,
    pub adaptive_max_input_tokens: Option<usize>,
    pub adaptive_max_output_tokens: Option<usize>,
    pub adaptive_max_blanks_per_batch: Option<usize>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            default_model: default_model(),
            quotas: BTreeMap::new(),
            generation: GenerationSettings::default(),
            batch: BatchSettings::default(),
            typst_template: None,
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
            max_tasks_per_batch: Some(5),
            max_input_tokens: Some(18_000),
            max_output_tokens: Some(6_000),
            max_blanks_per_batch: Some(52),
            max_concurrent_batches: Some(1),
        }
    }
}

impl QuotaSettings {
    pub fn resolve(&self, name: &str) -> Result<crate::quota::QuotaProfile, String> {
        if self.rpm == Some(0)
            || self.tpm == Some(0)
            || self.rpd == Some(0)
            || self.adaptive_max_tasks_per_batch == Some(0)
            || self.adaptive_max_input_tokens == Some(0)
            || self.adaptive_max_output_tokens == Some(0)
            || self.adaptive_max_blanks_per_batch == Some(0)
        {
            return Err(format!("quota '{name}' limits must be greater than zero"));
        }
        if self.rpd.is_some_and(|rpd| self.reserve_requests >= rpd) {
            return Err(format!(
                "quota '{name}' reserve_requests must be less than rpd"
            ));
        }
        Ok(crate::quota::QuotaProfile {
            name: name.to_string(),
            rpm: self.rpm,
            tpm: self.tpm.map(u64::from),
            rpd: self.rpd,
            reserve_requests: self.reserve_requests,
            adaptive_max_tasks_per_batch: self.adaptive_max_tasks_per_batch,
            adaptive_max_input_tokens: self.adaptive_max_input_tokens,
            adaptive_max_output_tokens: self.adaptive_max_output_tokens,
            adaptive_max_blanks_per_batch: self.adaptive_max_blanks_per_batch,
        })
    }
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
    fn config_rejects_unknown_fields_and_resolves_quota() {
        let directory =
            std::env::temp_dir().join(format!("flowcloze-config-yaml-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.yaml");
        fs::write(&path, "unknown: true\n").unwrap();
        assert!(load_app_config(&path).is_err());
        fs::write(&path, "default_model: gemini-flash\nquotas:\n  gemini-flash:\n    rpm: 5\n    tpm: 250000\nbatch:\n  max_retries: 4\n").unwrap();
        let config = load_app_config(&path).unwrap();
        let quota = config.quotas["gemini-flash"]
            .resolve("gemini-flash")
            .unwrap();
        assert_eq!(quota.rpm, Some(5));
        assert_eq!(quota.tpm, Some(250_000));
        assert_eq!(config.batch.max_retries, 4);
        fs::remove_dir_all(directory).unwrap();
    }
}
