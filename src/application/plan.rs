use crate::parser::parse_markdown_located;
use crate::planner::ComposeExecutionPolicy;
use crate::quota::QuotaProfile;
use crate::scaffold::ScaffoldDocument;

use super::generate::{build_blank_scaffold, prepare_selected_plan, GenerateMarkdownError};

#[derive(Debug, Clone, Default)]
pub struct PlanMarkdownOptions {
    pub policy: ComposeExecutionPolicy,
    pub offline: bool,
    pub quota: Option<QuotaProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanQBlockSummary {
    pub position: usize,
    pub id: String,
    pub input_tokens: usize,
    pub expected_output_tokens: usize,
    pub blanks: usize,
    pub isolated_heavy: bool,
    pub oversized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanBatchSummary {
    pub number: usize,
    pub qblocks: Vec<PlanQBlockSummary>,
    pub input_tokens: usize,
    pub expected_output_tokens: usize,
    pub blanks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanIdentitySummary {
    pub position: usize,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanMarkdownOutcome {
    pub total_qblocks: usize,
    pub provider_qblocks: usize,
    pub provider_batches: Vec<PlanBatchSummary>,
    pub identity_qblocks: Vec<PlanIdentitySummary>,
    pub identity_batches: usize,
    pub effective_policy: crate::planner::BatchPolicy,
}

pub fn plan_markdown(
    markdown: &str,
    options: PlanMarkdownOptions,
) -> Result<PlanMarkdownOutcome, GenerateMarkdownError> {
    let parsed = parse_markdown_located(markdown).map_err(GenerateMarkdownError::Markdown)?;
    let scaffold =
        build_blank_scaffold(markdown, &parsed).map_err(GenerateMarkdownError::Markdown)?;
    let all_indexes = (0..scaffold.tasks.len()).collect::<Vec<_>>();
    let (provider_indexes, identity_indexes) = if options.offline {
        (Vec::new(), all_indexes)
    } else {
        (all_indexes, Vec::new())
    };
    let identity_plan = prepare_selected_plan(&scaffold, &identity_indexes, options.policy, None)
        .map_err(GenerateMarkdownError::Compose)?;
    let provider_plan = prepare_selected_plan(
        &scaffold,
        &provider_indexes,
        options.policy,
        options.quota.as_ref(),
    )
    .map_err(GenerateMarkdownError::Compose)?;
    let selected = ScaffoldDocument {
        tasks: provider_indexes
            .iter()
            .map(|i| scaffold.tasks[*i].clone())
            .collect(),
    };
    let summary = provider_plan.summary(&selected);
    let effective_policy = summary.effective_policy;
    let provider_batches = summary
        .batches
        .into_iter()
        .enumerate()
        .map(|(batch_index, batch)| PlanBatchSummary {
            number: batch_index + 1,
            qblocks: batch
                .qblocks
                .into_iter()
                .map(|qblock| {
                    let index = provider_indexes[qblock.index];
                    PlanQBlockSummary {
                        position: index + 1,
                        id: qblock.id,
                        input_tokens: qblock.input_tokens,
                        expected_output_tokens: qblock.expected_output_tokens,
                        blanks: qblock.blanks,
                        isolated_heavy: qblock.isolated_heavy,
                        oversized: qblock.oversized,
                    }
                })
                .collect(),
            input_tokens: batch.input_tokens,
            expected_output_tokens: batch.expected_output_tokens,
            blanks: batch.blanks,
        })
        .collect();
    let identity_qblocks = identity_indexes
        .iter()
        .map(|i| PlanIdentitySummary {
            position: i + 1,
            id: scaffold.tasks[*i].id.clone(),
        })
        .collect();
    Ok(PlanMarkdownOutcome {
        total_qblocks: scaffold.tasks.len(),
        provider_qblocks: provider_indexes.len(),
        provider_batches,
        identity_qblocks,
        identity_batches: identity_plan.batch_count(),
        effective_policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plan_reports_exact_provider_batches_without_a_composer() {
        let markdown = "#qblock{\n[alpha]{term}。\n}\n#qblock{\n[beta]{term}。\n}\n#qblock{\n[gamma]{term}。\n}\n";
        let mut options = PlanMarkdownOptions::default();
        options.policy.batch_policy.max_tasks_per_batch = 2;
        options.policy.batch_policy.max_estimated_input_tokens = 100_000;
        options.policy.batch_policy.max_estimated_output_tokens = 100_000;
        options.policy.batch_policy.max_blanks_per_batch = 100;
        let plan = plan_markdown(markdown, options).unwrap();
        assert_eq!(plan.provider_qblocks, 3);
        assert_eq!(plan.provider_batches.len(), 2);
        let mut positions = plan
            .provider_batches
            .iter()
            .flat_map(|batch| batch.qblocks.iter().map(|qblock| qblock.position))
            .collect::<Vec<_>>();
        positions.sort_unstable();
        assert_eq!(positions, vec![1, 2, 3]);
        assert!(plan.identity_qblocks.is_empty());
    }
    #[test]
    fn offline_plan_lists_all_qblocks_as_no_api() {
        let options = PlanMarkdownOptions {
            offline: true,
            ..Default::default()
        };
        let plan = plan_markdown("#qblock{\n[answer]{term}。\n}\n", options).unwrap();
        assert_eq!(plan.provider_qblocks, 0);
        assert_eq!(plan.identity_qblocks.len(), 1);
        assert_eq!(plan.identity_qblocks[0].position, 1);
    }
}
