use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{LockResult, Mutex, MutexGuard};

use serde_json::{json, Map, Value};

use crate::compose::ComposeBatchRequest;
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

pub(super) fn response_format(
    strategy: StructuredStrategy,
    request: &ComposeBatchRequest,
    legacy_compose: bool,
) -> Option<Value> {
    match strategy {
        StructuredStrategy::JsonSchema if legacy_compose => Some(legacy_json_schema(request)),
        StructuredStrategy::JsonSchema => Some(segment_json_schema(request)),
        StructuredStrategy::JsonObject => Some(json!({"type": "json_object"})),
        StructuredStrategy::PromptOnly => None,
    }
}

fn legacy_json_schema(request: &ComposeBatchRequest) -> Value {
    let expected_ids = request
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let expected_items = request.tasks.len();
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "flowcloze_compose",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "minItems": expected_items,
                        "maxItems": expected_items,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string", "enum": expected_ids},
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
    })
}

fn segment_json_schema(request: &ComposeBatchRequest) -> Value {
    let mut item_properties = Map::new();
    let required_ids = request
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();

    for task in &request.tasks {
        let segment_count = task.blank_count + 1;
        item_properties.insert(
            task.id.clone(),
            json!({
                "type": "object",
                "properties": {
                    "segments": {
                        "type": "array",
                        "minItems": segment_count,
                        "maxItems": segment_count,
                        "items": {"type": "string"}
                    }
                },
                "required": ["segments"],
                "additionalProperties": false
            }),
        );
    }

    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "flowcloze_compose_segments",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "items": {
                        "type": "object",
                        "properties": item_properties,
                        "required": required_ids,
                        "additionalProperties": false
                    }
                },
                "required": ["items"],
                "additionalProperties": false
            }
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::ComposeTask;

    fn request() -> ComposeBatchRequest {
        ComposeBatchRequest {
            batch_id: "batch".into(),
            tasks: vec![
                ComposeTask {
                    id: "q1".into(),
                    scaffold_question: "<BLANK_0>".into(),
                    blank_count: 1,
                },
                ComposeTask {
                    id: "q2".into(),
                    scaffold_question: "<BLANK_0> / <BLANK_1>".into(),
                    blank_count: 2,
                },
            ],
            prompt_version: "test".into(),
            extra_constraints: Vec::new(),
            retry_feedback: Vec::new(),
        }
    }

    #[test]
    fn segment_json_schema_constrains_ids_and_segment_counts() {
        let format = response_format(StructuredStrategy::JsonSchema, &request(), false).unwrap();
        let items = &format["json_schema"]["schema"]["properties"]["items"];
        assert_eq!(items["required"], json!(["q1", "q2"]));
        assert_eq!(
            items["properties"]["q1"]["properties"]["segments"]["minItems"],
            2
        );
        assert_eq!(
            items["properties"]["q1"]["properties"]["segments"]["maxItems"],
            2
        );
        assert_eq!(
            items["properties"]["q2"]["properties"]["segments"]["minItems"],
            3
        );
        assert_eq!(items["additionalProperties"], false);
    }

    #[test]
    fn legacy_json_schema_keeps_previous_batch_shape() {
        let format = response_format(StructuredStrategy::JsonSchema, &request(), true).unwrap();
        let items = &format["json_schema"]["schema"]["properties"]["items"];
        assert_eq!(items["minItems"], 2);
        assert_eq!(items["maxItems"], 2);
        assert_eq!(
            items["items"]["properties"]["id"]["enum"],
            json!(["q1", "q2"])
        );
    }
}
