use std::{fs, process};

pub(crate) fn run(intermediate_path: &str, generated_path: &str) {
    let intermediate_json = fs::read_to_string(intermediate_path).unwrap_or_else(|e| {
        eprintln!("{intermediate_path} を読めませんでした: {e}");
        process::exit(1)
    });
    let generated_json = fs::read_to_string(generated_path).unwrap_or_else(|e| {
        eprintln!("{generated_path} を読めませんでした: {e}");
        process::exit(1)
    });
    let report =
        flowcloze::application::validate::validate_json(&intermediate_json, &generated_json);
    if report.is_valid() {
        println!("validation ok");
        return;
    }
    for error in report.errors {
        eprintln!("validation error: {error}");
    }
    process::exit(1);
}
