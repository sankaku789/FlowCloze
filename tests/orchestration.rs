use std::sync::Mutex;

use flowcloze::{
    generate_markdown_with_composer, generate_markdown_with_composer_with_progress, BatchPolicy,
    ComposeBatchOutput, ComposeError, ComposeExecutionPolicy, ComposeMetadata, ComposedItem,
    FailureClass, FallbackPolicy, GenerateMarkdownError, GenerateMarkdownOptions, IdentityComposer,
    ProgressEvent, ProgressSink, ProgressStage, QuestionComposer, RetryCause, RetryResult,
};

#[derive(Default)]
struct RecordingProgressSink(Mutex<Vec<ProgressEvent>>);

impl ProgressSink for RecordingProgressSink {
    fn emit(&self, event: ProgressEvent) {
        self.0.lock().unwrap().push(event);
    }
}

struct RemovePlaceholderOnce(Mutex<u32>);

impl QuestionComposer for RemovePlaceholderOnce {
    fn compose(
        &self,
        request: &flowcloze::ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        let mut calls = self.0.lock().unwrap();
        *calls += 1;
        Ok(ComposeBatchOutput {
            items: request
                .tasks
                .iter()
                .map(|task| ComposedItem {
                    id: task.id.clone(),
                    question: if *calls == 1 {
                        task.scaffold_question.replace("<BLANK_0>", "")
                    } else {
                        task.scaffold_question.clone()
                    },
                })
                .collect(),
            metadata: ComposeMetadata::default(),
        })
    }
}

#[test]
fn missing_placeholder_is_retried_and_recovers() {
    let progress = RecordingProgressSink::default();
    let composer = RemovePlaceholderOnce(Mutex::new(0));

    let outcome = generate_markdown_with_composer_with_progress(
        "#qblock{\n[alpha]{term}\n}\n",
        GenerateMarkdownOptions::new("inline.md"),
        &composer,
        &progress,
    )
    .unwrap();

    assert_eq!(outcome.document.questions.len(), 1);
    assert_eq!(outcome.document.questions[0].question.matches("＿＿＿").count(), 1);
    assert!(progress.0.lock().unwrap().contains(&ProgressEvent::Retry {
        task_id: "qblock-001".to_string(),
        attempt: 1,
        cause: RetryCause::MissingPlaceholder,
        result: RetryResult::Success,
    }));
}

struct UnknownPlaceholderComposer;

impl QuestionComposer for UnknownPlaceholderComposer {
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
                    question: task
                        .scaffold_question
                        .replace("<BLANK_0>", "<BLANK_9>"),
                })
                .collect(),
            metadata: ComposeMetadata::default(),
        })
    }
}

#[test]
fn unknown_placeholder_index_is_rejected() {
    let mut options = GenerateMarkdownOptions::new("inline.md");
    options.policy.max_content_retries = 0;

    let error = generate_markdown_with_composer(
        "#qblock{\n[alpha]{term}\n}\n",
        options,
        &UnknownPlaceholderComposer,
    )
    .unwrap_err();

    assert!(matches!(error, GenerateMarkdownError::Compose(_)));
}

struct AlwaysError(ComposeError);

impl QuestionComposer for AlwaysError {
    fn compose(
        &self,
        _: &flowcloze::ComposeBatchRequest,
    ) -> Result<ComposeBatchOutput, ComposeError> {
        Err(self.0.clone())
    }
}

#[test]
fn transport_error_is_reported_as_transport() {
    let progress = RecordingProgressSink::default();
    let mut options = GenerateMarkdownOptions::new("inline.md");
    options.policy.max_content_retries = 0;

    assert!(generate_markdown_with_composer_with_progress(
        "#qblock{\n[answer]{term}\n}\n",
        options,
        &AlwaysError(ComposeError::Transport),
        &progress,
    )
    .is_err());

    assert!(progress.0.lock().unwrap().contains(&ProgressEvent::Failed {
        stage: ProgressStage::Generate,
        class: FailureClass::Transport,
    }));
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
fn draft_fallback_keeps_all_tasks_when_content_is_invalid() {
    let mut options = GenerateMarkdownOptions::new("inline.md");
    options.fallback = FallbackPolicy::Draft;
    options.policy.max_content_retries = 0;

    let outcome = generate_markdown_with_composer(
        "#qblock{\n[a]{term}\n}\n#qblock{\n[b]{term}\n}\n",
        options,
        &InvalidComposer,
    )
    .unwrap();

    assert_eq!(outcome.document.questions.len(), 2);
    assert_eq!(outcome.fallback_summary.len(), 2);
    assert!(outcome
        .document
        .questions
        .iter()
        .all(|question| question.question.contains("＿＿＿")));
}

#[test]
fn identity_composer_uses_the_same_placeholder_pipeline() {
    let outcome = generate_markdown_with_composer(
        "#qblock{\nA[alpha]{term}B[beta]{term}\n}\n",
        GenerateMarkdownOptions::new("inline.md"),
        &IdentityComposer,
    )
    .unwrap();

    assert_eq!(outcome.document.questions.len(), 1);
    assert_eq!(outcome.document.questions[0].question.matches("＿＿＿").count(), 2);
    assert!(!outcome.document.questions[0].question.contains("<BLANK_"));
}

#[test]
fn batching_still_uses_current_execution_policy() {
    let mut options = GenerateMarkdownOptions::new("inline.md");
    options.policy = ComposeExecutionPolicy {
        batch_policy: BatchPolicy {
            max_tasks_per_batch: 1,
            max_estimated_input_tokens: 12_000,
            max_estimated_output_tokens: 12_000,
            max_blanks_per_batch: 64,
            max_retry_count: 1,
            max_concurrent_batches: 1,
        },
        max_content_retries: 1,
    };

    let outcome = generate_markdown_with_composer(
        "#qblock{\n[a]{term}\n}\n#qblock{\n[b]{term}\n}\n",
        options,
        &IdentityComposer,
    )
    .unwrap();

    assert_eq!(outcome.document.questions.len(), 2);
}
