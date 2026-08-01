use std::sync::Mutex;

use flowcloze::{
    generate_markdown_with_composer, ComposeBatchOutput, ComposeError, ComposeMetadata,
    ComposedItem, GenerateMarkdownError, GenerateMarkdownOptions, IdentityComposer,
    QuestionComposer,
};
use flowcloze::{BatchPolicy, ComposeExecutionPolicy, FallbackPolicy};

struct SwapThenIdentity {
    seen_tokens: Mutex<Vec<String>>,
}

impl QuestionComposer for SwapThenIdentity {
    fn compose(
        &self,
        request: &flowcloze::ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        self.seen_tokens.lock().unwrap().extend(
            request
                .tasks
                .iter()
                .map(|task| task.blank_tokens[0].clone()),
        );
        let mut tasks = request.tasks.iter().collect::<Vec<_>>();
        if tasks.len() > 1 {
            tasks.reverse();
        }
        Ok(ComposeBatchOutput {
            items: request
                .tasks
                .iter()
                .zip(tasks)
                .map(|(returned, source)| ComposedItem {
                    id: returned.id.clone(),
                    question: source.scaffold_question.clone(),
                })
                .collect(),
            metadata: ComposeMetadata::default(),
        })
    }
}

#[test]
fn swapped_task_bodies_are_retried_with_stable_tokens() {
    let composer = SwapThenIdentity {
        seen_tokens: Mutex::new(Vec::new()),
    };
    let markdown = "#qblock{\n[alpha]{term}\n}\n#qblock{\n[beta]{term}\n}\n";

    let outcome = generate_markdown_with_composer(
        markdown,
        GenerateMarkdownOptions::new("inline.md"),
        &composer,
    )
    .expect("single-task retry must retain its original token");

    assert_eq!(outcome.document.questions.len(), 2);
    let tokens = composer.seen_tokens.lock().unwrap();
    assert_eq!(tokens.len(), 4);
    assert_eq!(tokens[0], tokens[3]);
    assert_eq!(tokens[1], tokens[2]);
}

struct UnknownSentinelComposer;

impl QuestionComposer for UnknownSentinelComposer {
    fn compose(
        &self,
        request: &flowcloze::ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        Ok(ComposeBatchOutput {
            items: request
                .tasks
                .iter()
                .map(|task| ComposedItem {
                    id: task.id.clone(),
                    question: task.scaffold_question.replacen("000000", "999999", 1),
                })
                .collect(),
            metadata: ComposeMetadata::default(),
        })
    }
}

#[test]
fn active_namespace_unknown_index_is_rejected() {
    let error = generate_markdown_with_composer(
        "#qblock{\n[alpha]{term}\n}\n",
        GenerateMarkdownOptions::new("inline.md"),
        &UnknownSentinelComposer,
    )
    .unwrap_err();
    assert!(matches!(error, GenerateMarkdownError::Compose(_)));
}

struct OtherNamespaceComposer;

impl QuestionComposer for OtherNamespaceComposer {
    fn compose(
        &self,
        request: &flowcloze::ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        Ok(ComposeBatchOutput {
            items: request
                .tasks
                .iter()
                .map(|task| ComposedItem {
                    id: task.id.clone(),
                    question: format!("{} ⟦FC_0000000000000000_000000⟧", task.scaffold_question),
                })
                .collect(),
            metadata: ComposeMetadata::default(),
        })
    }
}

#[test]
fn source_existing_other_namespace_token_is_allowed() {
    let outcome = generate_markdown_with_composer(
        "#qblock{\n⟦FC_0000000000000000_000000⟧ [alpha]{term}\n}\n",
        GenerateMarkdownOptions::new("inline.md"),
        &IdentityComposer,
    )
    .unwrap();
    assert!(outcome.document.questions[0]
        .question
        .contains("⟦FC_0000000000000000_000000⟧"));
}

#[test]
fn newly_injected_other_namespace_token_is_rejected() {
    let error = generate_markdown_with_composer(
        "#qblock{\n[alpha]{term}\n}\n",
        GenerateMarkdownOptions::new("inline.md"),
        &OtherNamespaceComposer,
    )
    .unwrap_err();
    assert!(matches!(error, GenerateMarkdownError::Compose(_)));
}

#[test]
fn empty_or_whitespace_answer_is_rejected_before_provider() {
    let error = generate_markdown_with_composer(
        "#qblock{\n[ ]{term}\n}\n",
        GenerateMarkdownOptions::new("inline.md"),
        &UnknownSentinelComposer,
    )
    .unwrap_err();
    assert!(matches!(error, GenerateMarkdownError::Markdown(_)));
}

struct InvalidComposer;

impl QuestionComposer for InvalidComposer {
    fn compose(
        &self,
        request: &flowcloze::ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        Ok(ComposeBatchOutput {
            items: request
                .tasks
                .iter()
                .map(|task| ComposedItem {
                    id: task.id.clone(),
                    question: "invalid".to_string(),
                })
                .collect(),
            metadata: ComposeMetadata::default(),
        })
    }
}

#[test]
fn draft_fallback_collects_every_exhausted_content_task() {
    let mut options = GenerateMarkdownOptions::new("inline.md");
    options.fallback = FallbackPolicy::Draft;
    options.policy.max_content_retries = 0;
    let outcome = generate_markdown_with_composer(
        "#qblock{\n[a]{term}\n}\n#qblock{\n[b]{term}\n}\n#qblock{\n[c]{term}\n}\n",
        options,
        &InvalidComposer,
    )
    .unwrap();
    assert_eq!(outcome.document.questions.len(), 3);
    assert_eq!(outcome.fallback_summary.len(), 3);
    assert!(outcome
        .fallback_summary
        .iter()
        .all(|summary| summary.reason == flowcloze::orchestration::FallbackReason::Content));
}

struct FirstBatchThenTransport(Mutex<u32>);

impl QuestionComposer for FirstBatchThenTransport {
    fn compose(
        &self,
        request: &flowcloze::ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        let mut calls = self.0.lock().unwrap();
        *calls += 1;
        if *calls == 2 {
            return Err(ComposeError::Transport);
        }
        Ok(ComposeBatchOutput {
            items: request
                .tasks
                .iter()
                .map(|task| ComposedItem {
                    id: task.id.clone(),
                    question: format!("provider {}", task.scaffold_question),
                })
                .collect(),
            metadata: ComposeMetadata::default(),
        })
    }
}

#[test]
fn transport_fallback_drafts_only_the_unresolved_batch() {
    let mut options = GenerateMarkdownOptions::new("inline.md");
    options.fallback = FallbackPolicy::Draft;
    options.policy = ComposeExecutionPolicy {
        batch_policy: BatchPolicy {
            max_tasks_per_batch: 2,
            max_estimated_input_tokens: 12_000,
            max_retry_count: 0,
            max_concurrent_batches: 1,
        },
        max_content_retries: 0,
    };
    let outcome = generate_markdown_with_composer(
        "#qblock{\n[a]{term}\n}\n#qblock{\n[b]{term}\n}\n#qblock{\n[c]{term}\n}\n#qblock{\n[d]{term}\n}\n",
        options,
        &FirstBatchThenTransport(Mutex::new(0)),
    )
    .unwrap();
    assert_eq!(outcome.document.questions.len(), 4);
    assert!(outcome.document.questions[0]
        .question
        .starts_with("provider"));
    assert!(outcome.document.questions[1]
        .question
        .starts_with("provider"));
    assert_eq!(outcome.fallback_summary.len(), 2);
    assert!(outcome
        .fallback_summary
        .iter()
        .all(|summary| summary.reason == flowcloze::orchestration::FallbackReason::Transport));
}

#[test]
fn transport_fallback_drafts_unattempted_later_batches() {
    let mut options = GenerateMarkdownOptions::new("inline.md");
    options.fallback = FallbackPolicy::Draft;
    options.policy = ComposeExecutionPolicy {
        batch_policy: BatchPolicy {
            max_tasks_per_batch: 2,
            max_estimated_input_tokens: 12_000,
            max_retry_count: 0,
            max_concurrent_batches: 1,
        },
        max_content_retries: 0,
    };
    let composer = FirstBatchThenTransport(Mutex::new(0));
    let outcome = generate_markdown_with_composer(
        "#qblock{\n[a]{term}\n}\n#qblock{\n[b]{term}\n}\n#qblock{\n[c]{term}\n}\n#qblock{\n[d]{term}\n}\n#qblock{\n[e]{term}\n}\n#qblock{\n[f]{term}\n}\n",
        options,
        &composer,
    )
    .unwrap();
    assert_eq!(*composer.0.lock().unwrap(), 2);
    assert_eq!(outcome.document.questions.len(), 6);
    assert!(outcome.document.questions[..2]
        .iter()
        .all(|question| question.question.starts_with("provider")));
    assert_eq!(outcome.fallback_summary.len(), 4);
    assert_eq!(
        outcome
            .fallback_summary
            .iter()
            .map(|summary| &summary.id)
            .collect::<Vec<_>>(),
        vec!["qblock-003", "qblock-004", "qblock-005", "qblock-006"]
    );
}

struct ContentThenTransport(Mutex<u32>);

impl QuestionComposer for ContentThenTransport {
    fn compose(
        &self,
        request: &flowcloze::ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        let mut calls = self.0.lock().unwrap();
        *calls += 1;
        if *calls == 2 {
            return Err(ComposeError::Transport);
        }
        Ok(ComposeBatchOutput {
            items: request
                .tasks
                .iter()
                .map(|task| ComposedItem {
                    id: task.id.clone(),
                    question: if task.id == "qblock-002" {
                        "invalid".to_string()
                    } else {
                        format!("provider {}", task.scaffold_question)
                    },
                })
                .collect(),
            metadata: ComposeMetadata::default(),
        })
    }
}

#[test]
fn terminal_transport_preserves_queued_content_failure_and_all_remaining_tasks() {
    let mut options = GenerateMarkdownOptions::new("inline.md");
    options.fallback = FallbackPolicy::Draft;
    options.policy = ComposeExecutionPolicy {
        batch_policy: BatchPolicy {
            max_tasks_per_batch: 2,
            max_estimated_input_tokens: 12_000,
            max_retry_count: 0,
            max_concurrent_batches: 1,
        },
        max_content_retries: 1,
    };
    let composer = ContentThenTransport(Mutex::new(0));
    let outcome = generate_markdown_with_composer(
        "#qblock{\n[a]{term}\n}\n#qblock{\n[b]{term}\n}\n#qblock{\n[c]{term}\n}\n#qblock{\n[d]{term}\n}\n#qblock{\n[e]{term}\n}\n",
        options,
        &composer,
    )
    .unwrap();

    assert_eq!(*composer.0.lock().unwrap(), 2);
    assert_eq!(outcome.document.questions.len(), 5);
    assert!(outcome.document.questions[0]
        .question
        .starts_with("provider"));
    assert_eq!(
        outcome
            .fallback_summary
            .iter()
            .map(|summary| (summary.id.as_str(), summary.reason))
            .collect::<Vec<_>>(),
        vec![
            (
                "qblock-002",
                flowcloze::orchestration::FallbackReason::Content
            ),
            (
                "qblock-003",
                flowcloze::orchestration::FallbackReason::Transport
            ),
            (
                "qblock-004",
                flowcloze::orchestration::FallbackReason::Transport
            ),
            (
                "qblock-005",
                flowcloze::orchestration::FallbackReason::Transport
            ),
        ]
    );
}

struct AddsAnswer;

impl QuestionComposer for AddsAnswer {
    fn compose(
        &self,
        request: &flowcloze::ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        Ok(ComposeBatchOutput {
            items: request
                .tasks
                .iter()
                .map(|task| ComposedItem {
                    id: task.id.clone(),
                    question: format!("{} A", task.scaffold_question),
                })
                .collect(),
            metadata: ComposeMetadata::default(),
        })
    }
}

struct AddsZero;

impl QuestionComposer for AddsZero {
    fn compose(
        &self,
        request: &flowcloze::ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        Ok(ComposeBatchOutput {
            items: request
                .tasks
                .iter()
                .map(|task| ComposedItem {
                    id: task.id.clone(),
                    question: format!("{} 0", task.scaffold_question),
                })
                .collect(),
            metadata: ComposeMetadata::default(),
        })
    }
}

struct AddsAb;

impl QuestionComposer for AddsAb {
    fn compose(
        &self,
        request: &flowcloze::ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        Ok(ComposeBatchOutput {
            items: request
                .tasks
                .iter()
                .map(|task| ComposedItem {
                    id: task.id.clone(),
                    question: format!("{} ab", task.scaffold_question),
                })
                .collect(),
            metadata: ComposeMetadata::default(),
        })
    }
}

struct SegmentBoundaryLeakageComposer(Mutex<u32>);

impl QuestionComposer for SegmentBoundaryLeakageComposer {
    fn compose(
        &self,
        request: &flowcloze::ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        *self.0.lock().unwrap() += 1;
        Ok(ComposeBatchOutput {
            items: request
                .tasks
                .iter()
                .map(|task| ComposedItem {
                    id: task.id.clone(),
                    question: if task.id == "qblock-001" {
                        format!("provider {} ab", task.scaffold_question)
                    } else {
                        format!("provider {}", task.scaffold_question)
                    },
                })
                .collect(),
            metadata: ComposeMetadata::default(),
        })
    }
}

#[test]
fn located_leakage_retries_before_drafting_only_the_failed_task() {
    let mut options = GenerateMarkdownOptions::new("inline.md");
    options.fallback = FallbackPolicy::Draft;
    options.policy.max_content_retries = 1;
    let composer = SegmentBoundaryLeakageComposer(Mutex::new(0));
    let outcome = generate_markdown_with_composer(
        "#qblock{\na[X]{term}b [ab]{term}\n}\n#qblock{\n[c]{term}\n}\n",
        options,
        &composer,
    )
    .unwrap();

    assert_eq!(*composer.0.lock().unwrap(), 2);
    assert_eq!(outcome.fallback_summary.len(), 1);
    assert_eq!(outcome.fallback_summary[0].id, "qblock-001");
    assert_eq!(
        outcome.fallback_summary[0].reason,
        flowcloze::orchestration::FallbackReason::Content
    );
    assert!(!outcome.document.questions[0]
        .question
        .starts_with("provider"));
    assert!(outcome.document.questions[1]
        .question
        .starts_with("provider"));
}

#[test]
fn located_leakage_is_fatal_after_configured_retries_without_draft_fallback() {
    let mut options = GenerateMarkdownOptions::new("inline.md");
    options.fallback = FallbackPolicy::Error;
    options.policy.max_content_retries = 1;
    let composer = SegmentBoundaryLeakageComposer(Mutex::new(0));

    let error = generate_markdown_with_composer(
        "#qblock{\na[X]{term}b [ab]{term}\n}\n#qblock{\n[c]{term}\n}\n",
        options,
        &composer,
    )
    .unwrap_err();

    assert_eq!(*composer.0.lock().unwrap(), 2);
    assert!(matches!(error, GenerateMarkdownError::Compose(_)));
}

#[test]
fn located_leakage_baseline_distinguishes_target_spans_from_substrings() {
    let markdown = "#qblock{\n[A]{term} [AB]{term} A 東京 [東京]{term}\n}\n";
    assert!(generate_markdown_with_composer(
        markdown,
        GenerateMarkdownOptions::new("inline.md"),
        &IdentityComposer,
    )
    .is_ok());
    assert!(generate_markdown_with_composer(
        markdown,
        GenerateMarkdownOptions::new("inline.md"),
        &AddsAnswer,
    )
    .is_err());
}

#[test]
fn sentinel_digits_do_not_contribute_to_leakage_baseline() {
    let markdown = "#qblock{\n[0]{term-name} [10]{term-name}\n}\n";
    assert!(generate_markdown_with_composer(
        markdown,
        GenerateMarkdownOptions::new("inline.md"),
        &IdentityComposer,
    )
    .is_ok());
    assert!(generate_markdown_with_composer(
        markdown,
        GenerateMarkdownOptions::new("inline.md"),
        &AddsZero,
    )
    .is_err());
}

#[test]
fn located_leakage_baseline_does_not_join_non_target_segments() {
    let markdown = "#qblock{\na[X]{term}b [ab]{term} [cab]{term}\n}\n";
    assert!(generate_markdown_with_composer(
        markdown,
        GenerateMarkdownOptions::new("inline.md"),
        &IdentityComposer,
    )
    .is_ok());
    assert!(generate_markdown_with_composer(
        markdown,
        GenerateMarkdownOptions::new("inline.md"),
        &AddsAb,
    )
    .is_err());
}

#[test]
fn located_leakage_baseline_allows_existing_single_segment_match() {
    let markdown = "#qblock{\nab [ab]{term}\n}\n";
    assert!(generate_markdown_with_composer(
        markdown,
        GenerateMarkdownOptions::new("inline.md"),
        &IdentityComposer,
    )
    .is_ok());
}
