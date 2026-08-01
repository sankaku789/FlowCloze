//! 位置情報付き解析からcomposeまでを束ねる公開生成入口。

use std::collections::HashMap;
use std::ops::Range;

use crate::compose::{IdentityComposer, QuestionComposer};
use crate::config::{FallbackPolicy, RewritePolicy};
use crate::json::IntermediateDocument;
use crate::observability::{ComposeEvent, ComposeEventKind, EventSink, NoopEventSink, RunContext};
use crate::parser::{parse_markdown_located, MarkdownParseError, ParsedDocument};
use crate::planner::{ComposeExecutionPolicy, ComposePlanError, FailureReason};
use crate::scaffold::{ScaffoldDocument, ScaffoldTask};
use crate::validation::GeneratedDocument;

/// Markdown生成入口の設定。出力JSONにはこの情報を混ぜない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerateMarkdownOptions {
    pub source: String,
    pub policy: ComposeExecutionPolicy,
    pub rewrite: RewritePolicy,
    pub fallback: FallbackPolicy,
    /// provider taskがある時だけrequestへ渡す追加制約。
    pub extra_constraints: Vec<String>,
}

impl GenerateMarkdownOptions {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            policy: ComposeExecutionPolicy::default(),
            rewrite: RewritePolicy::Always,
            fallback: FallbackPolicy::Error,
            extra_constraints: Vec::new(),
        }
    }
}

/// Markdownから生成された文書。追跡情報はGeneratedDocumentのwire形式から分離する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerateMarkdownOutcome {
    pub document: GeneratedDocument,
    /// JSON wire形式へ入れない、fallbackしたtaskだけの運用情報。
    pub fallback_summary: Vec<FallbackSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackSummary {
    pub id: String,
    pub reason: FallbackReason,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    Transport,
    Content,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteReason {
    List,
    Multiline,
    NoTerminal,
    Short,
}

/// task本文だけから自然化の必要性を決める。理由の順序は表示・テストで安定させる。
pub fn auto_rewrite_reasons(source: &str) -> Vec<RewriteReason> {
    let trimmed = source.trim();
    let mut reasons = Vec::new();
    if trimmed.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with("+ ")
            || line
                .as_bytes()
                .iter()
                .position(|b| !b.is_ascii_digit())
                .is_some_and(|n| n > 0 && line[n..].starts_with(". "))
    }) {
        reasons.push(RewriteReason::List);
    }
    if trimmed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
        >= 2
    {
        reasons.push(RewriteReason::Multiline);
    }
    if !trimmed.is_empty() && !trimmed.ends_with(['。', '.', '!', '?', '！', '？']) {
        reasons.push(RewriteReason::NoTerminal);
    }
    if trimmed.chars().filter(|ch| !ch.is_whitespace()).count() < 40 {
        reasons.push(RewriteReason::Short);
    }
    reasons
}

/// located生成経路で起きる、provider呼び出し前後の失敗。
#[derive(Debug)]
pub enum GenerateMarkdownError {
    Markdown(MarkdownParseError),
    Compose(ComposePlanError),
}

impl std::fmt::Display for GenerateMarkdownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Markdown(error) => write!(f, "markdown parse error: {error}"),
            Self::Compose(error) => write!(f, "compose error: {error}"),
        }
    }
}

impl std::error::Error for GenerateMarkdownError {}

/// 位置情報を使った安全な標準生成経路。
pub fn generate_markdown_with_composer(
    markdown: &str,
    options: GenerateMarkdownOptions,
    composer: &dyn QuestionComposer,
) -> Result<GenerateMarkdownOutcome, GenerateMarkdownError> {
    let context = RunContext::new();
    let sink = NoopEventSink;
    generate_markdown_with_composer_observed(markdown, options, composer, &context, &sink)
}

/// 位置情報を使った安全な標準生成経路へ、本文なしの観測hookを加えた版。
pub fn generate_markdown_with_composer_observed(
    markdown: &str,
    options: GenerateMarkdownOptions,
    composer: &dyn QuestionComposer,
    context: &RunContext,
    sink: &dyn EventSink,
) -> Result<GenerateMarkdownOutcome, GenerateMarkdownError> {
    let parsed = parse_markdown_located(markdown).map_err(GenerateMarkdownError::Markdown)?;
    let qblocks = parsed
        .qblocks
        .iter()
        .map(|qblock| qblock.qblock.clone())
        .collect::<Vec<_>>();
    let intermediate = IntermediateDocument::from_qblocks(options.source, &qblocks);
    let (scaffold, leakage_baselines) =
        build_sentinel_scaffold(markdown, &parsed).map_err(GenerateMarkdownError::Markdown)?;
    let rewrite_indexes = scaffold
        .tasks
        .iter()
        .enumerate()
        .filter_map(|(index, task)| match options.rewrite {
            RewritePolicy::Always => Some(index),
            RewritePolicy::Never => None,
            RewritePolicy::Auto => {
                (!auto_rewrite_reasons(&task.source_text).is_empty()).then_some(index)
            }
        })
        .collect::<Vec<_>>();
    if options.rewrite == RewritePolicy::Auto {
        for (index, task) in scaffold.tasks.iter().enumerate() {
            let reasons = auto_rewrite_reasons(&task.source_text);
            let mut event = ComposeEvent::new(ComposeEventKind::RewriteDecision, context);
            event.task_id = Some(task.id.clone());
            event.validation_result = Some(
                if rewrite_indexes.contains(&index) {
                    "rewrite"
                } else {
                    "identity"
                }
                .to_string(),
            );
            event.error_class = (!reasons.is_empty()).then(|| {
                reasons
                    .iter()
                    .map(|reason| format!("{reason:?}").to_lowercase())
                    .collect::<Vec<_>>()
                    .join(",")
            });
            sink.emit(event);
        }
    }
    let identity_indexes = (0..scaffold.tasks.len())
        .filter(|index| !rewrite_indexes.contains(index))
        .collect::<Vec<_>>();
    let mut questions = Vec::new();
    let mut fallback_summary = Vec::new();
    if !identity_indexes.is_empty() {
        questions.extend(
            compose_indexes(
                &intermediate,
                &scaffold,
                &identity_indexes,
                options.policy,
                &IdentityComposer,
                context,
                sink,
                &[],
                &leakage_baselines,
            )
            .map_err(GenerateMarkdownError::Compose)?
            .questions,
        );
    }
    if !rewrite_indexes.is_empty() {
        // fallback方針は初回batchの形を変えない。plannerが失敗taskだけを単独retryする。
        match compose_indexes(
            &intermediate,
            &scaffold,
            &rewrite_indexes,
            options.policy,
            composer,
            context,
            sink,
            &options.extra_constraints,
            &leakage_baselines,
        ) {
            Ok(document) => questions.extend(document.questions),
            Err(ComposePlanError::Partial {
                document,
                failed_ids,
                failed_reasons,
            }) if options.fallback == FallbackPolicy::Draft => {
                questions.extend(document.questions);
                for (id, failure_reason) in failed_ids.into_iter().zip(failed_reasons) {
                    let index = rewrite_indexes
                        .iter()
                        .copied()
                        .find(|index| scaffold.tasks[*index].id == id)
                        .expect("planner failure must refer to a selected task");
                    let draft = compose_indexes(
                        &intermediate,
                        &scaffold,
                        &[index],
                        options.policy,
                        &IdentityComposer,
                        context,
                        sink,
                        &[],
                        &leakage_baselines,
                    )
                    .map_err(GenerateMarkdownError::Compose)?;
                    fallback_summary.push(FallbackSummary {
                        id: scaffold.tasks[index].id.clone(),
                        reason: match failure_reason {
                            FailureReason::Content => FallbackReason::Content,
                            FailureReason::Transport => FallbackReason::Transport,
                        },
                    });
                    let mut event = ComposeEvent::new(ComposeEventKind::Fallback, context);
                    event.task_id = Some(scaffold.tasks[index].id.clone());
                    event.fallback_reason = Some(
                        match failure_reason {
                            FailureReason::Content => "content",
                            FailureReason::Transport => "transport",
                        }
                        .to_string(),
                    );
                    sink.emit(event);
                    questions.extend(draft.questions);
                }
            }
            Err(error) => return Err(GenerateMarkdownError::Compose(error)),
        }
    }
    // 分割実行しても中間表現の順番を唯一の出力順として保つ。
    questions.sort_by_key(|question| {
        intermediate
            .qblocks
            .iter()
            .position(|qblock| qblock.id == question.id)
            .unwrap_or(usize::MAX)
    });
    let document = GeneratedDocument { questions };
    // fallbackを含めても、部分文書を成功として返さない。
    let report = crate::validation::validate_generated_documents(&intermediate, &document);
    if let Some(error) = report.errors.first() {
        return Err(GenerateMarkdownError::Compose(
            ComposePlanError::Validation {
                id: "document".to_string(),
                errors: vec![error.to_string()],
            },
        ));
    }
    Ok(GenerateMarkdownOutcome {
        document,
        fallback_summary,
    })
}

#[allow(clippy::too_many_arguments)]
fn compose_indexes(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    indexes: &[usize],
    policy: ComposeExecutionPolicy,
    composer: &dyn QuestionComposer,
    context: &RunContext,
    sink: &dyn EventSink,
    extra_constraints: &[String],
    leakage_baselines: &HashMap<String, Vec<usize>>,
) -> Result<GeneratedDocument, ComposePlanError> {
    let selected_intermediate = IntermediateDocument {
        meta: intermediate.meta.clone(),
        qblocks: indexes
            .iter()
            .map(|index| intermediate.qblocks[*index].clone())
            .collect(),
    };
    let selected_scaffold = ScaffoldDocument {
        tasks: indexes
            .iter()
            .map(|index| scaffold.tasks[*index].clone())
            .collect(),
    };
    crate::planner::compose_with_question_composer_observed_with_constraints_and_leakage_baselines(
        &selected_intermediate,
        &selected_scaffold,
        policy,
        composer,
        extra_constraints,
        context,
        sink,
        Some(leakage_baselines),
    )
}

fn build_sentinel_scaffold(
    markdown: &str,
    parsed: &ParsedDocument,
) -> Result<(ScaffoldDocument, HashMap<String, Vec<usize>>), MarkdownParseError> {
    let namespace = SentinelNamespace::for_document(markdown, parsed);
    let mut global_index = 0usize;
    let mut tasks = Vec::with_capacity(parsed.qblocks.len());
    let mut leakage_baselines = HashMap::new();
    for qblock in &parsed.qblocks {
        if qblock.qblock.targets.len() != qblock.target_locations.len() {
            return Err(MarkdownParseError::new("target位置を確定できません"));
        }
        for locations in qblock.target_locations.windows(2) {
            if locations[0].raw.end > locations[1].raw.start
                || locations[0].source_text.end > locations[1].source_text.start
            {
                return Err(MarkdownParseError::new("target spanが重複しています"));
            }
        }
        let mut replacements = Vec::new();
        for (target, location) in qblock.qblock.targets.iter().zip(&qblock.target_locations) {
            validate_target_location(
                markdown,
                qblock.raw_body.clone(),
                target.answer.as_str(),
                location.raw.clone(),
                location.source_text.clone(),
                qblock.qblock.source_text.as_str(),
            )?;
            replacements.push((location.source_text.clone(), namespace.token(global_index)));
            global_index += 1;
        }
        let mut scaffold_question = qblock.qblock.source_text.clone();
        // 後ろから置換してsource_text上のbyte位置をずらさない。
        for (span, token) in replacements.iter().rev() {
            scaffold_question.replace_range(span.clone(), token);
        }
        let non_target_segments = non_target_segments(&qblock.qblock.source_text, &replacements);
        leakage_baselines.insert(
            qblock.qblock.id.clone(),
            qblock
                .qblock
                .targets
                .iter()
                .map(|target| {
                    // target間を連結すると、元の本文にないanswer一致を作ってしまう。
                    non_target_segments
                        .iter()
                        .map(|segment| segment.match_indices(&target.answer).count())
                        .sum()
                })
                .collect(),
        );
        tasks.push(ScaffoldTask {
            id: qblock.qblock.id.clone(),
            source_text: qblock.qblock.source_text.clone(),
            cloze_template: scaffold_question.clone(),
            scaffold_question,
            blank_count: qblock.qblock.targets.len(),
            answers: qblock
                .qblock
                .targets
                .iter()
                .map(|target| target.answer.clone())
                .collect(),
        });
    }
    Ok((ScaffoldDocument { tasks }, leakage_baselines))
}

/// target spanで分割した、連結しないtarget外の本文断片を返す。
fn non_target_segments<'a>(
    source_text: &'a str,
    replacements: &[(Range<usize>, String)],
) -> Vec<&'a str> {
    let mut start = 0;
    let mut segments = Vec::with_capacity(replacements.len() + 1);
    for (span, _) in replacements {
        segments.push(&source_text[start..span.start]);
        start = span.end;
    }
    segments.push(&source_text[start..]);
    segments
}

fn validate_target_location(
    markdown: &str,
    raw_body: Range<usize>,
    answer: &str,
    raw: Range<usize>,
    source: Range<usize>,
    source_text: &str,
) -> Result<(), MarkdownParseError> {
    if answer.trim().is_empty() || raw.is_empty() || source.is_empty() {
        return Err(MarkdownParseError::new(
            "空または空白だけのtarget answerは生成できません",
        ));
    }
    if raw.start < raw_body.start
        || raw.end > raw_body.end
        || !markdown.is_char_boundary(raw.start)
        || !markdown.is_char_boundary(raw.end)
        || !source_text.is_char_boundary(source.start)
        || !source_text.is_char_boundary(source.end)
    {
        return Err(MarkdownParseError::new(
            "target spanがUTF-8境界またはqblock範囲外です",
        ));
    }
    if markdown.get(raw) != Some(answer) || source_text.get(source) != Some(answer) {
        return Err(MarkdownParseError::new("target spanとanswerが一致しません"));
    }
    Ok(())
}

/// 文書全体で一意なnamespaceを決定する。
struct SentinelNamespace(u64);

impl SentinelNamespace {
    fn for_document(markdown: &str, parsed: &ParsedDocument) -> Self {
        let mut input = markdown.as_bytes().to_vec();
        input.push(0);
        for qblock in &parsed.qblocks {
            input.extend_from_slice(qblock.qblock.id.as_bytes());
            input.push(0);
        }
        let source = parsed
            .qblocks
            .iter()
            .map(|qblock| qblock.qblock.source_text.as_str())
            .collect::<String>();
        for counter in 0u64.. {
            let mut candidate = input.clone();
            candidate.extend_from_slice(&counter.to_le_bytes());
            let value = fnv1a(&candidate);
            if !source.contains(&format!("⟦FC_{value:016x}_")) {
                return Self(value);
            }
        }
        unreachable!("u64 counter is exhaustive")
    }

    fn token(&self, index: usize) -> String {
        format!("⟦FC_{:016x}_{index:06}⟧", self.0)
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use crate::compose::IdentityComposer;

    use super::*;

    #[test]
    fn identity_generates_utf8_crlf_document_end_to_end() {
        let markdown = "#qblock{\r\n  [😀答]{term} は [同じ]{term}、[同じ]{term}。\r\n}\r\n";
        let outcome = generate_markdown_with_composer(
            markdown,
            GenerateMarkdownOptions::new("inline.md"),
            &IdentityComposer,
        )
        .expect("located identity path should validate");
        assert_eq!(
            outcome.document.questions[0].question,
            "＿＿＿ は ＿＿＿、＿＿＿。"
        );
        assert_eq!(
            outcome.document.questions[0].answers,
            ["😀答", "同じ", "同じ"]
        );
    }

    #[test]
    fn namespace_avoids_prefix_already_in_source_text() {
        let markdown = "#qblock{\n[alpha]{term}\n}\n";
        let parsed = parse_markdown_located(markdown).unwrap();
        let first = SentinelNamespace::for_document(markdown, &parsed);
        let collision = format!("⟦FC_{:016x}_", first.0);
        let markdown_with_collision = format!("#qblock{{\n{collision}[alpha]{{term}}\n}}\n");
        let parsed = parse_markdown_located(&markdown_with_collision).unwrap();
        let namespace = SentinelNamespace::for_document(&markdown_with_collision, &parsed);
        assert!(!parsed.qblocks[0]
            .qblock
            .source_text
            .contains(&format!("⟦FC_{:016x}_", namespace.0)));
    }

    #[test]
    fn auto_rewrite_reasons_are_complete_and_stably_ordered() {
        assert_eq!(
            auto_rewrite_reasons(" - item\nsecond"),
            vec![
                RewriteReason::List,
                RewriteReason::Multiline,
                RewriteReason::NoTerminal,
                RewriteReason::Short,
            ]
        );
        assert!(auto_rewrite_reasons(&format!("{}。", "あ".repeat(40))).is_empty());
    }

    #[test]
    fn never_uses_identity_without_calling_provider() {
        struct PanickingComposer;
        impl QuestionComposer for PanickingComposer {
            fn compose(
                &self,
                _: &crate::compose::ComposeBatchRequest,
            ) -> Result<crate::compose::ComposeBatchOutput, crate::compose::ComposeError>
            {
                panic!("provider must not be initialized")
            }
        }
        let mut options = GenerateMarkdownOptions::new("inline.md");
        options.rewrite = RewritePolicy::Never;
        assert!(generate_markdown_with_composer(
            "#qblock{\n[answer]{term}\n}\n",
            options,
            &PanickingComposer
        )
        .is_ok());
    }

    #[test]
    fn draft_fallback_keeps_successful_tasks_and_excludes_metadata_from_document() {
        struct OneFails;
        impl QuestionComposer for OneFails {
            fn compose(
                &self,
                request: &crate::compose::ComposeBatchRequest,
            ) -> Result<crate::compose::ComposeBatchOutput, crate::compose::ComposeError>
            {
                let mut output = IdentityComposer.compose(request).unwrap();
                for item in &mut output.items {
                    if item.id.ends_with("002") {
                        item.question = "invalid".to_string();
                    }
                }
                Ok(output)
            }
        }
        let mut options = GenerateMarkdownOptions::new("inline.md");
        options.fallback = FallbackPolicy::Draft;
        let outcome = generate_markdown_with_composer(
            "#qblock{\n[alpha]{term}\n}\n#qblock{\n[beta]{term}\n}\n",
            options,
            &OneFails,
        )
        .unwrap();
        assert_eq!(outcome.document.questions.len(), 2);
        assert_eq!(outcome.fallback_summary.len(), 1);
        assert_eq!(outcome.fallback_summary[0].reason, FallbackReason::Content);
        assert!(!serde_json::to_string(&outcome.document)
            .unwrap()
            .contains("fallback"));
    }
}
