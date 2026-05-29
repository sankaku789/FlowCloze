//! FlowCloze記法を含むMarkdownからqblockと出題対象を抽出する．

use std::error::Error;
use std::fmt;

use crate::models::{QBlock, Target, TargetOccurrence, ALLOWED_TARGET_TYPES, DEFAULT_TARGET_TYPE};

/// qblockの閉じ忘れなど，FlowCloze記法を解析できない場合のエラー．
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownParseError {
    message: String,
}

impl MarkdownParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for MarkdownParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Error for MarkdownParseError {}

/// Markdown文書からコードフェンス外のqblockをすべて抽出する．
pub fn parse_markdown(markdown: &str) -> Result<Vec<QBlock>, MarkdownParseError> {
    let sections = iter_qblock_sections(markdown)?;
    sections
        .into_iter()
        .enumerate()
        .map(|(index, section)| {
            parse_qblock_with_default_id(&section.body, &auto_qblock_id(index), section.section)
        })
        .collect::<Result<Vec<_>, _>>()
}

/// qblock抽出を明示したい呼び出し箇所向けの `parse_markdown` の別名．
pub fn parse_qblocks(markdown: &str) -> Result<Vec<QBlock>, MarkdownParseError> {
    parse_markdown(markdown)
}

/// 単体のqblock本文を既定ID付きで解析する．
pub fn parse_qblock(body: &str) -> Result<QBlock, MarkdownParseError> {
    parse_qblock_with_default_id(body, "qblock-001", None)
}

fn parse_qblock_with_default_id(
    body: &str,
    default_id: &str,
    section: Option<String>,
) -> Result<QBlock, MarkdownParseError> {
    let parsed = parse_targets_and_plain(body);
    let mut warnings = Vec::new();
    for target in &parsed.targets {
        if !ALLOWED_TARGET_TYPES.contains(&target.target_type.as_str()) {
            warnings.push(format!(
                "answer '{}' のtarget type '{}' は未定義です",
                target.answer, target.target_type
            ));
        }
    }

    Ok(QBlock {
        id: default_id.to_string(),
        section,
        raw_source_text: body.trim().to_string(),
        source_text: parsed.plain,
        targets: parsed.targets,
        target_occurrences: parsed.occurrences,
        warnings,
    })
}

fn auto_qblock_id(index: usize) -> String {
    format!("qblock-{:03}", index + 1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QBlockSection {
    body: String,
    section: Option<String>,
}

fn iter_qblock_sections(markdown: &str) -> Result<Vec<QBlockSection>, MarkdownParseError> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut sections = Vec::new();
    let mut index = 0;
    let mut in_fence = false;
    let mut current_heading = None;

    while index < lines.len() {
        let line = lines[index];
        if is_fence_line(line) {
            in_fence = !in_fence;
            index += 1;
            continue;
        }

        if in_fence {
            index += 1;
            continue;
        }

        if let Some(heading) = parse_markdown_heading(line) {
            current_heading = Some(heading);
            index += 1;
            continue;
        }

        if !is_qblock_open(line) {
            index += 1;
            continue;
        }

        let start_line = index + 1;
        let mut body_lines = Vec::new();
        index += 1;

        while index < lines.len() && !is_qblock_close(lines[index]) {
            body_lines.push(lines[index]);
            index += 1;
        }

        if index >= lines.len() {
            return Err(MarkdownParseError::new(format!(
                "line {start_line} から始まるqblockが閉じられていません"
            )));
        }

        sections.push(QBlockSection {
            body: body_lines.join("\n"),
            section: current_heading.clone(),
        });
        index += 1;
    }

    Ok(sections)
}

fn parse_markdown_heading(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if level != 1 {
        return None;
    }
    let rest = trimmed.get(level..)?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let heading = rest.trim().trim_matches('#').trim();
    (!heading.is_empty()).then(|| heading.to_string())
}

fn is_qblock_open(line: &str) -> bool {
    matches!(line.trim(), "#qblock{" | "#qblock {")
}

fn is_qblock_close(line: &str) -> bool {
    line.trim() == "}"
}

fn is_fence_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedTargets {
    plain: String,
    targets: Vec<Target>,
    occurrences: Vec<TargetOccurrence>,
}

fn parse_targets_and_plain(body: &str) -> ParsedTargets {
    let mut output = String::new();
    let mut targets = Vec::new();
    let mut occurrences = Vec::new();
    let mut rest = body;

    while let Some(start) = rest.find('[') {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + 1..];
        let Some(answer_end) = after_open.find(']') else {
            output.push_str(&rest[start..]);
            return trim_parsed_targets(output, targets, occurrences);
        };
        let answer = &after_open[..answer_end];
        let after_answer = &after_open[answer_end + 1..];
        if answer.contains('\n') {
            output.push('[');
            output.push_str(answer);
            output.push(']');
            rest = after_answer;
        } else if let Some(after_open_brace) = after_answer.strip_prefix('{') {
            let Some(type_end) = after_open_brace.find('}') else {
                output.push('[');
                output.push_str(answer);
                output.push(']');
                rest = after_answer;
                continue;
            };
            let target_type = &after_open_brace[..type_end];
            if normalize_target_type(target_type).is_some() {
                let target_type = normalize_target_type(target_type).unwrap();
                push_target(
                    answer,
                    target_type,
                    &mut output,
                    &mut targets,
                    &mut occurrences,
                );
            } else {
                output.push('[');
                output.push_str(answer);
                output.push(']');
                output.push('{');
                output.push_str(target_type);
                output.push('}');
            }
            rest = &after_open_brace[type_end + 1..];
        } else {
            output.push('[');
            output.push_str(answer);
            output.push(']');
            if let Some(label_len) = markdown_reference_label_len(after_answer) {
                output.push_str(&after_answer[..label_len]);
                rest = &after_answer[label_len..];
                continue;
            }
            if !after_answer.starts_with('(') && !answer.contains('\n') {
                output.truncate(output.len() - answer.len() - 2);
                push_target(
                    answer,
                    DEFAULT_TARGET_TYPE,
                    &mut output,
                    &mut targets,
                    &mut occurrences,
                );
            }
            rest = after_answer;
        }
    }

    output.push_str(rest);
    trim_parsed_targets(output, targets, occurrences)
}

fn push_target(
    answer: &str,
    target_type: &str,
    output: &mut String,
    targets: &mut Vec<Target>,
    occurrences: &mut Vec<TargetOccurrence>,
) {
    let start = output.len();
    output.push_str(answer);
    let end = output.len();
    let target_index = targets.len();
    targets.push(Target {
        answer: answer.to_string(),
        target_type: target_type.to_string(),
    });
    occurrences.push(TargetOccurrence {
        target_index,
        start,
        end,
    });
}

fn trim_parsed_targets(
    plain: String,
    targets: Vec<Target>,
    occurrences: Vec<TargetOccurrence>,
) -> ParsedTargets {
    let trimmed = plain.trim();
    let leading_trim = trimmed.as_ptr() as usize - plain.as_ptr() as usize;
    let trailing_end = leading_trim + trimmed.len();
    let occurrences = occurrences
        .into_iter()
        .filter_map(|occurrence| {
            if occurrence.start < leading_trim || occurrence.end > trailing_end {
                return None;
            }
            Some(TargetOccurrence {
                target_index: occurrence.target_index,
                start: occurrence.start - leading_trim,
                end: occurrence.end - leading_trim,
            })
        })
        .collect();

    ParsedTargets {
        plain: trimmed.to_string(),
        targets,
        occurrences,
    }
}

fn normalize_target_type(target_type: &str) -> Option<&str> {
    if target_type.chars().any(char::is_whitespace) {
        return None;
    }
    if target_type.is_empty() {
        Some(DEFAULT_TARGET_TYPE)
    } else {
        Some(target_type)
    }
}

fn markdown_reference_label_len(text: &str) -> Option<usize> {
    let label = text.strip_prefix('[')?;
    let label_end = label.find(']')?;
    Some(label_end + 2)
}
