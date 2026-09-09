use crate::core::validation::{self, GeneratedDocument, ValidationReport};

pub fn validate_document(
    intermediate_json: &str,
    document: &GeneratedDocument,
) -> ValidationReport {
    validation::validate_generated_document(intermediate_json, document)
}

pub fn validate_json(intermediate_json: &str, generated_json: &str) -> ValidationReport {
    validation::validate_generated_json(intermediate_json, generated_json)
}
