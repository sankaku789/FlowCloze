# Compose Error Mapping
- Public `ComposePlanError` mapping preserves `ComposeError::Configuration` as `Configuration { id: "composer" }`.
- Every other `ComposeError` maps to `ComposePlanError::Llm(error.to_string())`; do not replace payload with an internal error class.
- `TerminalCause` remains private execution/progress classification, independent from public payload.