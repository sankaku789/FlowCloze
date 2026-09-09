use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::catalog::{AuthRequirement, ProviderCatalog};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    #[serde(default)]
    pub name: String,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub profile: String,
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub auth: AuthRequirement,
}

#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    models: HashMap<String, ModelProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    Invalid,
    Duplicate { model: String },
    Unknown { model: String },
    UnknownProvider { model: String, provider: String },
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => write!(f, "model profile fields must not be empty"),
            Self::Duplicate { model } => write!(f, "model profile {model} is already registered"),
            Self::Unknown { model } => write!(f, "unknown model profile: {model}"),
            Self::UnknownProvider { model, provider } => {
                write!(f, "model {model} references unknown provider: {provider}")
            }
        }
    }
}

impl std::error::Error for ModelError {}

impl ModelRegistry {
    pub fn get(&self, name: &str) -> Option<&ModelProfile> {
        self.models.get(name)
    }

    pub fn register(&mut self, model: ModelProfile) -> Result<(), ModelError> {
        let name = validate_model(&model)?;
        if self.models.insert(name.clone(), model).is_some() {
            return Err(ModelError::Duplicate { model: name });
        }
        Ok(())
    }

    pub fn upsert(&mut self, model: ModelProfile) -> Result<(), ModelError> {
        let name = validate_model(&model)?;
        self.models.insert(name, model);
        Ok(())
    }

    pub fn resolve(
        &self,
        name: &str,
        providers: &ProviderCatalog,
    ) -> Result<ResolvedModel, ModelError> {
        let profile = self.get(name).ok_or_else(|| ModelError::Unknown {
            model: name.to_string(),
        })?;
        let provider =
            providers
                .get(&profile.provider)
                .ok_or_else(|| ModelError::UnknownProvider {
                    model: name.to_string(),
                    provider: profile.provider.clone(),
                })?;
        Ok(ResolvedModel {
            profile: name.to_string(),
            provider: profile.provider.clone(),
            base_url: provider.base_url.clone(),
            model: profile.model.clone(),
            auth: provider.auth,
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ModelProfile)> {
        self.models
            .iter()
            .map(|(name, model)| (name.as_str(), model))
    }
}

fn validate_model(model: &ModelProfile) -> Result<String, ModelError> {
    let name = model.name.trim().to_string();
    if name.is_empty() || model.provider.trim().is_empty() || model.model.trim().is_empty() {
        return Err(ModelError::Invalid);
    }
    Ok(name)
}
