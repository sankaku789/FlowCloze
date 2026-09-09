use super::catalog::{AuthRequirement, ProviderCatalog, ProviderDefinition};
use super::model_registry::{ModelProfile, ModelRegistry};

pub const DEFAULT_MODEL: &str = "gemini-flash";

pub fn builtin_providers() -> ProviderCatalog {
    let mut catalog = ProviderCatalog::default();
    catalog
        .register(ProviderDefinition {
            id: "google".into(),
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai".into(),
            auth: AuthRequirement::ApiKey,
        })
        .expect("valid built-in google provider");
    catalog
        .register(ProviderDefinition {
            id: "ollama".into(),
            base_url: "http://localhost:11434/v1".into(),
            auth: AuthRequirement::None,
        })
        .expect("valid built-in ollama provider");
    catalog
}

pub fn builtin_models() -> ModelRegistry {
    let mut registry = ModelRegistry::default();
    registry
        .register(ModelProfile {
            name: DEFAULT_MODEL.into(),
            provider: "google".into(),
            model: "gemini-2.5-flash".into(),
        })
        .expect("valid built-in gemini model");
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_provider_google_is_resolvable() {
        assert!(builtin_providers().get("google").is_some());
    }

    #[test]
    fn builtin_provider_ollama_is_resolvable() {
        assert!(builtin_providers().get("ollama").is_some());
    }

    #[test]
    fn builtin_gemini_flash_is_default() {
        let providers = builtin_providers();
        let resolved = builtin_models().resolve(DEFAULT_MODEL, &providers).unwrap();
        assert_eq!(resolved.model, "gemini-2.5-flash");
        assert_eq!(resolved.provider, "google");
    }
}
