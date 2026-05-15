//! Typstテンプレートを使って生成済み問題JSONからPDFを作成する。
//!
//! レイアウトはTypstテンプレートに寄せ，このモジュールは入力パスの解決と
//! `typst compile` の実行，出力ファイルの確認だけを担当する。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfOptions {
    pub generated_json_path: PathBuf,
    pub output_pdf_path: PathBuf,
    pub template_path: PathBuf,
}

#[derive(Debug)]
pub enum PdfError {
    MissingGeneratedJson {
        path: PathBuf,
        source: std::io::Error,
    },
    MissingTemplate {
        path: PathBuf,
        source: std::io::Error,
    },
    TypstNotFound(std::io::Error),
    InvalidOutputPath {
        path: PathBuf,
    },
    OutputMissing {
        path: PathBuf,
    },
    CopyOutput {
        from: PathBuf,
        to: PathBuf,
        source: std::io::Error,
    },
    TypstFailed {
        status: Option<i32>,
        stderr: String,
    },
}

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingGeneratedJson { path, source } => {
                write!(f, "{} を読めませんでした: {source}", path.display())
            }
            Self::MissingTemplate { path, source } => {
                write!(f, "{} を読めませんでした: {source}", path.display())
            }
            Self::TypstNotFound(error) => {
                write!(f, "typstを実行できませんでした: {error}")
            }
            Self::InvalidOutputPath { path } => {
                write!(f, "{} はPDF出力先として使えません", path.display())
            }
            Self::OutputMissing { path } => {
                write!(f, "{} が生成されませんでした", path.display())
            }
            Self::CopyOutput { from, to, source } => {
                write!(
                    f,
                    "{} から {} へPDFをコピーできませんでした: {source}",
                    from.display(),
                    to.display()
                )
            }
            Self::TypstFailed { status, stderr } => {
                write!(
                    f,
                    "typst compileが失敗しました(status: {}): {}",
                    status
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "signal".to_string()),
                    stderr.trim()
                )
            }
        }
    }
}

impl std::error::Error for PdfError {}

pub fn default_pdf_output_path(generated_json_path: impl AsRef<Path>) -> PathBuf {
    generated_json_path.as_ref().with_extension("pdf")
}

pub fn compile_pdf(options: &PdfOptions) -> Result<(), PdfError> {
    let generated_json_path = canonicalize(&options.generated_json_path).map_err(|source| {
        PdfError::MissingGeneratedJson {
            path: options.generated_json_path.clone(),
            source,
        }
    })?;
    let template_path =
        canonicalize(&options.template_path).map_err(|source| PdfError::MissingTemplate {
            path: options.template_path.clone(),
            source,
        })?;

    let (typst_output_arg, final_output_path, temporary_output_path) =
        typst_output_paths(&options.output_pdf_path)?;

    let mut command = Command::new("typst");
    command
        .arg("compile")
        .arg("--root")
        .arg("/")
        .arg(&template_path)
        .arg(&typst_output_arg);

    let output = command
        .arg("--input")
        .arg(format!("data={}", generated_json_path.display()))
        .output()
        .map_err(PdfError::TypstNotFound)?;

    if !output.status.success() {
        return Err(PdfError::TypstFailed {
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    if let Some(temporary_output_path) = temporary_output_path {
        if !temporary_output_path.exists() {
            return Err(PdfError::OutputMissing {
                path: temporary_output_path,
            });
        }
        fs::copy(&temporary_output_path, &final_output_path).map_err(|source| {
            PdfError::CopyOutput {
                from: temporary_output_path.clone(),
                to: final_output_path.clone(),
                source,
            }
        })?;
        let _ = fs::remove_file(temporary_output_path);
        return Ok(());
    }

    if !final_output_path.exists() {
        return Err(PdfError::OutputMissing {
            path: final_output_path,
        });
    }

    Ok(())
}

fn canonicalize(path: &Path) -> Result<PathBuf, std::io::Error> {
    path.canonicalize()
}

fn typst_output_paths(
    output_pdf_path: &Path,
) -> Result<(PathBuf, PathBuf, Option<PathBuf>), PdfError> {
    if output_pdf_path.as_os_str().is_empty() {
        return Err(PdfError::InvalidOutputPath {
            path: output_pdf_path.to_path_buf(),
        });
    }

    if output_pdf_path.is_absolute() {
        let temporary_name = format!(".flowcloze-pdf-{}.tmp.pdf", std::process::id());
        let temporary_output_path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(&temporary_name);
        return Ok((
            PathBuf::from(temporary_name),
            output_pdf_path.to_path_buf(),
            Some(temporary_output_path),
        ));
    }

    Ok((
        output_pdf_path.to_path_buf(),
        output_pdf_path.to_path_buf(),
        None,
    ))
}
