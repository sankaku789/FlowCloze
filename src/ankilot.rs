//! Ankilot CSV export.

use crate::validation::{GeneratedDocument, GeneratedQuestion};

pub fn to_ankilot_csv(document: &GeneratedDocument) -> String {
    let mut csv = String::new();
    for question in &document.questions {
        write_csv_row(&mut csv, &question_to_row(question));
    }
    csv
}

fn question_to_row(question: &GeneratedQuestion) -> [String; 2] {
    [question.question.clone(), question.answers.join("\n")]
}

fn write_csv_row(csv: &mut String, row: &[String; 2]) {
    for (index, field) in row.iter().enumerate() {
        if index > 0 {
            csv.push(',');
        }
        write_csv_field(csv, field);
    }
    csv.push('\n');
}

fn write_csv_field(csv: &mut String, field: &str) {
    if !field
        .bytes()
        .any(|byte| matches!(byte, b',' | b'"' | b'\n' | b'\r'))
    {
        csv.push_str(field);
        return;
    }

    csv.push('"');
    for ch in field.chars() {
        if ch == '"' {
            csv.push('"');
        }
        csv.push(ch);
    }
    csv.push('"');
}
