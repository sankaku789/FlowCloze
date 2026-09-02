//! LLM provider adapterの整理された公開namespace。

pub(crate) mod capability;

pub mod gemini {
    pub use crate::gemini::{
        GeminiAdapter, GeminiApi, GeminiClient, GeminiError, StructuredOutputMode,
    };
}

pub mod openai_compatible {
    pub use crate::local_openai::{
        local_openai_url_candidates, try_local_openai_candidates, LocalOpenAiClient,
        LocalOpenAiError, OpenAiCompatibleAdapter, OpenAiCompatiblePool,
    };
}
