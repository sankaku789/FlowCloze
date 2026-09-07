//! 旧module pathの互換re-export。実装はproviders::openai_compatibleに集約する。

pub use crate::providers::openai_compatible::{
    local_openai_url_candidates, try_local_openai_candidates, OpenAiAuth, OpenAiCompatibleAdapter,
    OpenAiCompatiblePool, OpenAiEndpointConfig,
};
