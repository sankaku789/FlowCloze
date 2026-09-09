use serde::Serialize;

use crate::orchestration::build_sentinel_scaffold;
use crate::parser::{MarkdownParseError, ParsedDocument};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GenerationTask {
    pub id: String,
    pub section: String,
    pub source_text: String,
    pub answers: Vec<String>,
    pub target_types: Vec<Option<String>>,
    pub draft_question: String,
    pub blank_tokens: Vec<String>,
    pub leakage_baseline: Vec<usize>,
}

pub type TaskBuildError = MarkdownParseError;

pub fn build_generation_tasks(
    markdown: &str,
    parsed: &ParsedDocument,
) -> Result<Vec<GenerationTask>, TaskBuildError> {
    let (scaffold, leakage_baselines) = build_sentinel_scaffold(markdown, parsed)?;
    Ok(scaffold
        .tasks
        .into_iter()
        .zip(&parsed.qblocks)
        .map(|(task, parsed)| GenerationTask {
            id: task.id.clone(),
            section: parsed.qblock.section.clone().unwrap_or_default(),
            source_text: task.source_text,
            answers: task.answers,
            target_types: parsed
                .qblock
                .targets
                .iter()
                .map(|target| Some(target.target_type.clone()))
                .collect(),
            blank_tokens: sentinel_tokens(&task.scaffold_question),
            draft_question: task.scaffold_question,
            leakage_baseline: leakage_baselines.get(&task.id).cloned().unwrap_or_default(),
        })
        .collect())
}

fn sentinel_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("⟦FC_") {
        let candidate = &remaining[start..];
        let Some(end) = candidate.find('⟧') else {
            break;
        };
        let token_end = end + '⟧'.len_utf8();
        tokens.push(candidate[..token_end].to_string());
        remaining = &candidate[token_end..];
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_markdown_located;

    #[test]
    fn generation_task_matches_legacy_scaffold() {
        let markdown = "# Memory\n\n#qblock{\n短期記憶は[ワーキングメモリ]{term-name}である。\n}";
        let parsed = parse_markdown_located(markdown).unwrap();
        let tasks = build_generation_tasks(markdown, &parsed).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].section, "Memory");
        assert_eq!(tasks[0].answers, ["ワーキングメモリ"]);
        assert_eq!(tasks[0].blank_tokens.len(), 1);
        assert!(tasks[0].draft_question.contains(&tasks[0].blank_tokens[0]));
    }
}
