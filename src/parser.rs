//! FlowCloze記法を含むMarkdownからqblockと出題対象を抽出する．

use std::error::Error;
use std::fmt;
use std::ops::Range;

use crate::models::{QBlock, Target, ALLOWED_TARGET_TYPES, DEFAULT_TARGET_TYPE};

/// qblockの閉じ忘れなど，FlowCloze記法を解析できない場合のエラー．
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownParseError {
    message: String,
}

impl MarkdownParseError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// 新しい生成経路だけが使う、元Markdown上の位置を保持した解析結果。
#[derive(Debug, Clone)]
pub(crate) struct ParsedDocument {
    pub qblocks: Vec<ParsedQBlock>,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedQBlock {
    pub qblock: QBlock,
    pub raw_body: Range<usize>,
    pub target_locations: Vec<TargetLocation>,
}

/// targetのanswer部分の元Markdown上、およびtrim前source_text上のbyte範囲。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetLocation {
    pub raw: Range<usize>,
    pub source_text: Range<usize>,
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

/// 元Markdownのbyte位置を失わずにqblockを解析する内部用入口。
pub(crate) fn parse_markdown_located(markdown: &str) -> Result<ParsedDocument, MarkdownParseError> {
    let mut qblocks = Vec::new();
    let mut in_fence = false;
    let mut current_heading = None;
    let mut offset = 0;
    let mut lines = markdown.split_inclusive('\n').peekable();

    while let Some(line_with_end) = lines.next() {
        let line_start = offset;
        offset += line_with_end.len();
        let line = line_with_end.trim_end_matches(['\r', '\n']);
        if is_fence_line(line) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(heading) = parse_markdown_heading(line) {
            current_heading = Some(heading);
            continue;
        }
        if !is_qblock_open(line) {
            continue;
        }

        let body_start = offset;
        let mut close_start = None;
        for body_line_with_end in lines.by_ref() {
            let body_line_start = offset;
            offset += body_line_with_end.len();
            let body_line = body_line_with_end.trim_end_matches(['\r', '\n']);
            if is_qblock_close(body_line) {
                close_start = Some(body_line_start);
                break;
            }
        }
        let Some(body_end) = close_start else {
            return Err(MarkdownParseError::new(format!(
                "byte {line_start} から始まるqblockが閉じられていません"
            )));
        };
        let raw_body = body_start..body_end;
        let (source_text, targets, locations) =
            parse_located_body(&markdown[raw_body.clone()], body_start)?;
        let mut warnings = Vec::new();
        for target in &targets {
            if !ALLOWED_TARGET_TYPES.contains(&target.target_type.as_str()) {
                warnings.push(format!(
                    "answer '{}' のtarget type '{}' は未定義です",
                    target.answer, target.target_type
                ));
            }
        }
        qblocks.push(ParsedQBlock {
            qblock: QBlock {
                id: auto_qblock_id(qblocks.len()),
                section: current_heading.clone(),
                source_text,
                targets,
                warnings,
            },
            raw_body,
            target_locations: locations,
        });
    }
    Ok(ParsedDocument { qblocks })
}

/// target markupを一度だけ走査して、plain本文と両方のspanを作る。
fn parse_located_body(
    body: &str,
    body_offset: usize,
) -> Result<(String, Vec<Target>, Vec<TargetLocation>), MarkdownParseError> {
    let mut plain = String::new();
    let mut targets = Vec::new();
    let mut locations = Vec::new();
    let mut cursor = 0;
    while let Some(relative_open) = body[cursor..].find('[') {
        let open = cursor + relative_open;
        plain.push_str(&body[cursor..open]);
        let Some(relative_close) = body[open + 1..].find(']') else {
            plain.push_str(&body[open..]);
            cursor = body.len();
            break;
        };
        let close = open + 1 + relative_close;
        let answer = &body[open + 1..close];
        let after = close + 1;
        let mut end = after;
        let target_type = if body[after..].starts_with('{') {
            let Some(type_end_relative) = body[after + 1..].find('}') else {
                plain.push('[');
                cursor = open + 1;
                continue;
            };
            let type_end = after + 1 + type_end_relative;
            let value = &body[after + 1..type_end];
            let Some(value) = normalize_target_type(value) else {
                plain.push('[');
                cursor = open + 1;
                continue;
            };
            end = type_end + 1;
            Some(value)
        } else if !answer.contains('\n')
            && !body[after..].starts_with('(')
            && markdown_reference_label_len(&body[after..]).is_none()
        {
            Some(DEFAULT_TARGET_TYPE)
        } else {
            None
        };
        let Some(target_type) = target_type else {
            plain.push('[');
            cursor = open + 1;
            continue;
        };
        let source_start = plain.len();
        plain.push_str(answer);
        let source_end = plain.len();
        targets.push(Target {
            answer: answer.to_string(),
            target_type: target_type.to_string(),
        });
        locations.push(TargetLocation {
            raw: body_offset + open + 1..body_offset + close,
            source_text: source_start..source_end,
        });
        cursor = end;
    }
    plain.push_str(&body[cursor..]);
    let trim_start = plain.len() - plain.trim_start().len();
    let trim_end = plain.trim_end().len() + trim_start;
    for location in &mut locations {
        if location.source_text.start < trim_start || location.source_text.end > trim_end {
            return Err(MarkdownParseError::new(
                "targetがsource_textのtrim除去範囲と交差しています",
            ));
        }
        location.source_text =
            location.source_text.start - trim_start..location.source_text.end - trim_start;
    }
    Ok((plain.trim().to_string(), targets, locations))
}

/// 単体のqblock本文を既定ID付きで解析する．
pub fn parse_qblock(body: &str) -> Result<QBlock, MarkdownParseError> {
    parse_qblock_with_default_id(body, "qblock-001", None)
}

/// qblock本文からtargetと表示用本文を抽出し，不足するidは呼び出し側の既定値で補う．
fn parse_qblock_with_default_id(
    body: &str,
    default_id: &str,
    section: Option<String>,
) -> Result<QBlock, MarkdownParseError> {
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
        id: default_id.to_string(),
        section,
        source_text: strip_target_markup(body).trim().to_string(),
        targets,
        warnings,
    })
}

/// 入力順に基づいて安定した自動qblock idを作る．
fn auto_qblock_id(index: usize) -> String {
    format!("qblock-{:03}", index + 1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QBlockSection {
    body: String,
    section: Option<String>,
}

/// Markdown全体を走査し，#qblock{...} の範囲と現在sectionを取り出す．
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

/// Markdown見出し行からsection名として使う文字列を取り出す．
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

/// 行がqblock開始マーカーかどうかを判定する．
fn is_qblock_open(line: &str) -> bool {
    matches!(line.trim(), "#qblock{" | "#qblock {")
}

/// 行がqblock終了マーカーかどうかを判定する．
fn is_qblock_close(line: &str) -> bool {
    line.trim() == "}"
}

/// Markdownコードフェンス内のqblock風テキストを誤検出しないための判定．
fn is_fence_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// qblock本文から `[answer]` / `[answer]{type}` のtargetを順序付きで抽出する．
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
        if answer.contains('\n') {
            rest = after_answer;
            continue;
        }
        if let Some(after_open_brace) = after_answer.strip_prefix('{') {
            let Some(type_end) = after_open_brace.find('}') else {
                rest = after_answer;
                continue;
            };
            let target_type = &after_open_brace[..type_end];
            if let Some(target_type) = normalize_target_type(target_type) {
                targets.push(Target {
                    answer: answer.to_string(),
                    target_type: target_type.to_string(),
                });
            }
            rest = &after_open_brace[type_end + 1..];
        } else {
            if after_answer.starts_with('(') {
                rest = after_answer;
                continue;
            }
            if let Some(label_len) = markdown_reference_label_len(after_answer) {
                rest = &after_answer[label_len..];
                continue;
            }
            targets.push(Target {
                answer: answer.to_string(),
                target_type: DEFAULT_TARGET_TYPE.to_string(),
            });
            rest = after_answer;
        }
    }

    targets
}

/// 中間表現のsource_text用に，targetマークアップだけを取り除いた本文を作る．
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
        if let Some(after_open_brace) = after_answer.strip_prefix('{') {
            let Some(type_end) = after_open_brace.find('}') else {
                output.push('[');
                output.push_str(answer);
                output.push(']');
                rest = after_answer;
                continue;
            };
            let target_type = &after_open_brace[..type_end];
            if normalize_target_type(target_type).is_some() {
                output.push_str(answer);
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
                output.push_str(answer);
            }
            rest = after_answer;
        }
    }

    output.push_str(rest);
    output
}

/// 許可されたtarget typeだけを採用し，未知のtypeは警告側へ回せるようNoneにする．
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

/// Markdown参照リンク風の `[...]` をanswer targetと誤認しないための長さ判定．
fn markdown_reference_label_len(text: &str) -> Option<usize> {
    let label = text.strip_prefix('[')?;
    let label_end = label.find(']')?;
    Some(label_end + 2)
}
