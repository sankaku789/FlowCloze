//! LLM provider adapterの公開namespace。

pub mod adapter_factory;
pub mod builtins;
pub mod capability;
pub mod catalog;
pub mod model_registry;
pub mod openai_compatible;

#[cfg(feature = "gemini-native")]
pub mod gemini_native {
    pub use crate::gemini::GeminiAdapter;
}
