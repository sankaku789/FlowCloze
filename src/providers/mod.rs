//! LLM provider adapterの公開namespace。

pub mod capability;
pub mod openai_compatible;

#[cfg(feature = "gemini-native")]
pub mod gemini_native {
    pub use crate::gemini::GeminiAdapter;
}
