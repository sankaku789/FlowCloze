use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{LockResult, Mutex, MutexGuard};

use serde_json::{json, Value};

use crate::http::HttpError;

const UNKNOWN: u8 = 0;
const JSON_SCHEMA: u8 = 1;
const JSON_OBJECT: u8 = 2;
const PROMPT_ONLY: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StructuredStrategy {
    JsonSchema,
    JsonObject,
    PromptOnly,
}

#[derive(Debug)]
pub(super) struct StructuredCapabilityProbe {
    state: AtomicU8,
    probe: Mutex<()>,
}

impl Default for StructuredCapabilityProbe {
    fn default() -> Self {
        Self {
            state: AtomicU8::new(UNKNOWN),
            probe: Mutex::new(()),
        }
    }
}

impl Clone for StructuredCapabilityProbe {
    fn clone(&self) -> Self {
        Self {
            state: AtomicU8::new(self.state.load(Ordering::Acquire)),
            probe: Mutex::new(()),
        }
    }
}

impl StructuredCapabilityProbe {
    pub(super) fn strategy(&self) -> Option<StructuredStrategy> {
        match self.state.load(Ordering::Acquire) {
            JSON_SCHEMA => Some(StructuredStrategy::JsonSchema),
            JSON_OBJECT => Some(StructuredStrategy::JsonObject),
            PROMPT_ONLY => Some(StructuredStrategy::PromptOnly),
            _ => None,
        }
    }

    pub(super) fn mark(&self, strategy: StructuredStrategy) {
        let value = match strategy {
            StructuredStrategy::JsonSchema => JSON_SCHEMA,
            StructuredStrategy::JsonObject => JSON_OBJECT,
            StructuredStrategy::PromptOnly => PROMPT_ONLY,
        };
        self.state.store(value, Ordering::Release);
    }

    pub(super) fn lock(&self) -> LockResult<MutexGuard<'_, ()>> {
        self.probe.lock()
    }
}

pub(super) fn response_format(strategy: StructuredStrategy) -> Option<Value> {
    match strategy {
        StructuredStrategy::JsonSchema => Some(json!({
            "type": "json_schema",
            "json_schema": {
                "name": "flowcloze_compose",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": {"type": "string"},
                                    "question": {"type": "string"}
                                },
                                "required": ["id", "question"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["items"],
                    "additionalProperties": false
                }
            }
        })),
        StructuredStrategy::JsonObject => Some(json!({"type": "json_object"})),
        StructuredStrategy::PromptOnly => None,
    }
}

pub(super) fn unsupported_response_format(error: &HttpError) -> bool {
    let HttpError::Api {
        status: 400 | 404 | 422,
        body,
        ..
    } = error
    else {
        return false;
    };

    let lower = body.to_ascii_lowercase();
    let mentions_format = lower.contains("response_format")
        || lower.contains("json_schema")
        || lower.contains("json_object")
        || lower.contains("structured output")
        || lower.contains("structured_output");
    let rejects_format = lower.contains("unsupported")
        || lower.contains("not support")
        || lower.contains("unknown")
        || lower.contains("unrecognized")
        || lower.contains("invalid")
        || lower.contains("not available");
    mentions_format && rejects_format
}
