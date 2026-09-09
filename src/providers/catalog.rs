use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthRequirement {
    ApiKey,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDefinition {
    #[serde(default)]
    pub id: String,
    pub base_url: String,
    pub auth: AuthRequirement,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderCatalog {
    providers: HashMap<String, ProviderDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    EmptyId,
    InvalidBaseUrl { provider: String },
    Duplicate { provider: String },
    Authentication { provider: String },
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId => write!(f, "provider id must not be empty"),
            Self::InvalidBaseUrl { provider } => {
                write!(f, "provider {provider} has an invalid base URL")
            }
            Self::Duplicate { provider } => write!(f, "provider {provider} is already registered"),
            Self::Authentication { provider } => {
                write!(f, "missing API key for provider: {provider}")
            }
        }
    }
}

impl std::error::Error for ProviderError {}

impl ProviderCatalog {
    pub fn get(&self, id: &str) -> Option<&ProviderDefinition> {
        self.providers.get(id)
    }

    pub fn register(&mut self, provider: ProviderDefinition) -> Result<(), ProviderError> {
        let id = validate_provider(&provider)?;
        if self.providers.insert(id.clone(), provider).is_some() {
            return Err(ProviderError::Duplicate { provider: id });
        }
        Ok(())
    }

    pub fn upsert(&mut self, provider: ProviderDefinition) -> Result<(), ProviderError> {
        let id = validate_provider(&provider)?;
        self.providers.insert(id, provider);
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ProviderDefinition)> {
        self.providers
            .iter()
            .map(|(id, provider)| (id.as_str(), provider))
    }
}

fn validate_provider(provider: &ProviderDefinition) -> Result<String, ProviderError> {
    let id = provider.id.trim().to_string();
    if id.is_empty() {
        return Err(ProviderError::EmptyId);
    }
    if !(provider.base_url.starts_with("http://") || provider.base_url.starts_with("https://")) {
        return Err(ProviderError::InvalidBaseUrl { provider: id });
    }
    Ok(id)
}
