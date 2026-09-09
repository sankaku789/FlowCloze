use std::fs;
use std::path::PathBuf;
use std::process;

use flowcloze::{
    compile_pdf, default_pdf_output_path, to_ankilot_csv, GeneratedDocument, PdfOptions,
};

fn read_document(path: &str) -> GeneratedDocument {
    let json = fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("{path} を読めませんでした: {e}");
        process::exit(1)
    });
    serde_json::from_str(&json).unwrap_or_else(|e| {
        eprintln!("生成結果JSONを読めません: {e}");
        process::exit(1)
    })
}

pub(crate) fn view(path: &str) {
    if let Err(e) = flowcloze::output::tui::run_viewer(read_document(path)) {
        eprintln!("TUIの表示に失敗しました: {e}");
        process::exit(1);
    }
}

pub(crate) fn csv(path: &str, output_path: Option<&str>) {
    let csv = to_ankilot_csv(&read_document(path));
    if let Some(output_path) = output_path {
        fs::write(output_path, csv).unwrap_or_else(|e| {
            eprintln!("{output_path} へ書き込めませんでした: {e}");
            process::exit(1)
        });
    } else {
        print!("{csv}");
    }
}

pub(crate) fn pdf(path: &str, output_path: Option<&str>, template_path: &str) {
    let output_pdf_path = output_path
        .map(PathBuf::from)
        .unwrap_or_else(|| default_pdf_output_path(path));
    let template_path = if template_path == "templates/cloze.typ" {
        flowcloze::config::typst_template_path().unwrap_or_else(|e| {
            eprintln!("{e}");
            process::exit(2)
        })
    } else {
        PathBuf::from(template_path)
    };
    let options = PdfOptions {
        generated_json_path: PathBuf::from(path),
        output_pdf_path: output_pdf_path.clone(),
        template_path,
    };
    if let Err(e) = compile_pdf(&options) {
        eprintln!("{e}");
        process::exit(1);
    }
    println!("{}", output_pdf_path.display());
}
