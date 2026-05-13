//! FlowCloze用Markdown記法のParser。

use std::collections::HashSet;
use std::error::Error;
use std::fmt;

use crate::models::{QBlock, Target, ALLOWED_TARGET_TYPES};

/// Markdownのqblock記法が壊れている場合に送出するエラー。
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

/// Markdown文書からすべてのqblockを抽出する。
pub fn parse_markdown(markdown: &str) -> Result<Vec<QBlock>, MarkdownParseError> {
    let sections = iter_qblock_sections(markdown)?;
    let mut qblocks = sections
        .into_iter()
        .map(|section| parse_qblock(&section.attrs_text, &section.body, section.start_line))
        .collect::<Result<Vec<_>, _>>()?;

    warn_duplicate_ids(&mut qblocks);
    Ok(qblocks)
}

/// CLIやテストから意図が見えるように用意した `parse_markdown` の別名。
pub fn parse_qblocks(markdown: &str) -> Result<Vec<QBlock>, MarkdownParseError> {
    parse_markdown(markdown)
}

/// 1つのqblock本文と属性を解析する。
pub fn parse_qblock(
    attrs_text: &str,
    body: &str,
    start_line: usize,
) -> Result<QBlock, MarkdownParseError> {
    let attrs = parse_attrs(attrs_text, start_line)?;
    let id = attr_value(&attrs, "id")
        .ok_or_else(|| {
            MarkdownParseError::new(format!(
                "line {start_line} から始まるqblockに必須属性 id がありません"
            ))
        })?
        .to_string();
    let title = attr_value(&attrs, "title").map(str::to_string);
    let mode = attr_value(&attrs, "mode").map(str::to_string);

    let targets = extract_targets(body);
    let mut warnings = Vec::new();
    for target in &targets {
        if !ALLOWED_TARGET_TYPES.contains(&target.target_type.as_str()) {
            warnings.push(format!(
                "answer '{}' のtarget type '{}' は未定義です",
                target.answer, target.target_type
            ));
        }
    }

    Ok(QBlock {
        id,
        title,
        source_text: strip_target_markup(body).trim().to_string(),
        targets,
        mode,
        attrs,
        warnings,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QBlockSection {
    attrs_text: String,
    body: String,
    start_line: usize,
}

fn iter_qblock_sections(markdown: &str) -> Result<Vec<QBlockSection>, MarkdownParseError> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut sections = Vec::new();
    let mut index = 0;
    let mut in_fence = false;

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

        let Some(attrs_text) = parse_qblock_open(line) else {
            index += 1;
            continue;
        };

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
            attrs_text,
            body: body_lines.join("\n"),
            start_line,
        });
        index += 1;
    }

    Ok(sections)
}

fn parse_qblock_open(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix(":::")?.trim_start();
    let rest = rest.strip_prefix("qblock")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) && !rest.starts_with('{') {
        return None;
    }
    let rest = rest.trim();

    if rest.is_empty() {
        return Some(String::new());
    }

    if let Some(inner) = rest
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
    {
        return Some(inner.trim().to_string());
    }

    Some(rest.to_string())
}

fn attr_value<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find_map(|(attr_key, value)| (attr_key == key).then_some(value.as_str()))
}

fn is_qblock_close(line: &str) -> bool {
    line.trim() == ":::"
}

fn is_fence_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

fn parse_attrs(
    attrs_text: &str,
    start_line: usize,
) -> Result<Vec<(String, String)>, MarkdownParseError> {
    let mut attrs = Vec::new();
    for token in split_attr_tokens(attrs_text, start_line)? {
        let Some((key, value)) = token.split_once('=') else {
            return Err(MarkdownParseError::new(format!(
                "line {start_line} のqblock属性 '{token}' が key=value 形式ではありません"
            )));
        };
        if key.is_empty() {
            return Err(MarkdownParseError::new(format!(
                "line {start_line} のqblock属性名が空です"
            )));
        }
        attrs.push((key.to_string(), value.to_string()));
    }
    Ok(attrs)
}

fn split_attr_tokens(
    attrs_text: &str,
    start_line: usize,
) -> Result<Vec<String>, MarkdownParseError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = attrs_text.chars().peekable();

    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some(active_quote), value) if value == active_quote => {
                quote = None;
            }
            (Some(_), '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (Some(_), value) => current.push(value),
            (None, '"' | '\'') => quote = Some(ch),
            (None, value) if value.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(current);
                    current = String::new();
                }
            }
            (None, value) => current.push(value),
        }
    }

    if quote.is_some() {
        return Err(MarkdownParseError::new(format!(
            "line {start_line} のqblock属性で引用符が閉じられていません"
        )));
    }

    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn extract_targets(body: &str) -> Vec<Target> {
    let mut targets = Vec::new();
    let mut rest = body;

    while let Some(start) = rest.find('[') {
        rest = &rest[start + 1..];
        let Some(answer_end) = rest.find(']') else {
            break;
        };
        let answer = &rest[..answer_end];
        let after_answer = &rest[answer_end + 1..];
        let Some(after_open_brace) = after_answer.strip_prefix('{') else {
            rest = after_answer;
            continue;
        };
        let Some(type_end) = after_open_brace.find('}') else {
            break;
        };
        let target_type = &after_open_brace[..type_end];
        if !answer.contains('\n') && !target_type.chars().any(char::is_whitespace) {
            targets.push(Target {
                answer: answer.to_string(),
                target_type: target_type.to_string(),
            });
        }
        rest = &after_open_brace[type_end + 1..];
    }

    targets
}

fn strip_target_markup(body: &str) -> String {
    let mut output = String::new();
    let mut rest = body;

    while let Some(start) = rest.find('[') {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + 1..];
        let Some(answer_end) = after_open.find(']') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let answer = &after_open[..answer_end];
        let after_answer = &after_open[answer_end + 1..];
        let Some(after_open_brace) = after_answer.strip_prefix('{') else {
            output.push('[');
            output.push_str(answer);
            output.push(']');
            rest = after_answer;
            continue;
        };
        let Some(type_end) = after_open_brace.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };
        output.push_str(answer);
        rest = &after_open_brace[type_end + 1..];
    }

    output.push_str(rest);
    output
}

fn warn_duplicate_ids(qblocks: &mut [QBlock]) {
    let mut seen = HashSet::new();
    let mut duplicates = HashSet::new();
    for qblock in qblocks.iter() {
        if !seen.insert(qblock.id.clone()) {
            duplicates.insert(qblock.id.clone());
        }
    }

    for qblock in qblocks {
        if duplicates.contains(&qblock.id) {
            qblock
                .warnings
                .push(format!("qblock id '{}' が重複しています", qblock.id));
        }
    }
}
