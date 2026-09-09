pub mod generate;
pub mod plan;
pub mod validate;

use crate::compose::QuestionComposer;
use generate::{
    generate_markdown_with_composer, GenerateMarkdownError, GenerateMarkdownOptions,
    GenerateMarkdownOutcome,
};
use plan::{plan_markdown, PlanMarkdownOptions, PlanMarkdownOutcome};

pub struct GenerateUseCase<'a> {
    composer: &'a dyn QuestionComposer,
}

impl<'a> GenerateUseCase<'a> {
    pub fn new(composer: &'a dyn QuestionComposer) -> Self {
        Self { composer }
    }

    pub fn execute(
        &self,
        markdown: &str,
        options: GenerateMarkdownOptions,
    ) -> Result<GenerateMarkdownOutcome, GenerateMarkdownError> {
        generate_markdown_with_composer(markdown, options, self.composer)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PlanUseCase;

impl PlanUseCase {
    pub fn execute(
        &self,
        markdown: &str,
        options: PlanMarkdownOptions,
    ) -> Result<PlanMarkdownOutcome, GenerateMarkdownError> {
        plan_markdown(markdown, options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IdentityComposer;

    const MARKDOWN: &str = "#qblock{\n[答え]{term-name}を説明する。\n}";

    #[test]
    fn generate_use_case_offline() {
        let options = GenerateMarkdownOptions::new("inline.md");
        let outcome = GenerateUseCase::new(&IdentityComposer)
            .execute(MARKDOWN, options)
            .unwrap();
        assert_eq!(outcome.document.questions.len(), 1);
    }

    #[test]
    fn plan_use_case_does_not_call_provider() {
        let outcome = PlanUseCase
            .execute(MARKDOWN, PlanMarkdownOptions::default())
            .unwrap();
        assert_eq!(outcome.total_qblocks, 1);
    }
}
