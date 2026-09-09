use crate::config::auth_store::AuthStore;

use super::catalog::{AuthRequirement, ProviderError};
use super::model_registry::ResolvedModel;
use super::openai_compatible::{OpenAiCompatibleAdapter, OpenAiEndpointConfig};

pub fn build_adapter(
    model: &ResolvedModel,
    auth: &AuthStore,
) -> Result<OpenAiCompatibleAdapter, ProviderError> {
    let mut endpoint = OpenAiEndpointConfig::new(&model.base_url, &model.model)
        .with_provider_label(&model.provider);
    if model.auth == AuthRequirement::ApiKey {
        let key =
            auth.require_api_key(&model.provider)
                .map_err(|_| ProviderError::Authentication {
                    provider: model.provider.clone(),
                })?;
        endpoint = endpoint.with_bearer(key);
    }
    Ok(OpenAiCompatibleAdapter::from_endpoint(endpoint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::builtins::{builtin_models, builtin_providers};

    #[test]
    fn ollama_requires_no_auth_entry() {
        let providers = builtin_providers();
        let mut models = builtin_models();
        models
            .upsert(crate::providers::model_registry::ModelProfile {
                name: "local".into(),
                provider: "ollama".into(),
                model: "qwen3".into(),
            })
            .unwrap();
        let model = models.resolve("local", &providers).unwrap();
        assert!(build_adapter(&model, &AuthStore::default()).is_ok());
    }
}
