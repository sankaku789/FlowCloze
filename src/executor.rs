use std::collections::HashMap;

use crate::compose::QuestionComposer;
use crate::json::{IntermediateDocument, IntermediateMeta, IntermediateQBlock, IntermediateTarget};
use crate::observability::{EventSink, RunContext};
use crate::planner::{ComposeExecutionError, ComposeExecutionPolicy, PreparedComposePlan};
use crate::progress::ProgressSink;
use crate::scaffold::{ScaffoldDocument, ScaffoldTask};
use crate::task::GenerationTask;
use crate::validation::GeneratedDocument;

pub type ExecutionError = ComposeExecutionError;

pub struct ExecutionContext<'a> {
    pub run: &'a RunContext,
    pub events: &'a dyn EventSink,
    pub progress: &'a dyn ProgressSink,
}

#[derive(Debug, Clone, Copy)]
pub struct Executor {
    policy: ComposeExecutionPolicy,
}

impl Executor {
    pub fn new(policy: ComposeExecutionPolicy) -> Self {
        Self { policy }
    }

    pub fn execute(
        &self,
        tasks: &[GenerationTask],
        plan: &PreparedComposePlan,
        composer: &dyn QuestionComposer,
        context: &ExecutionContext<'_>,
    ) -> Result<GeneratedDocument, ExecutionError> {
        let intermediate = IntermediateDocument {
            meta: IntermediateMeta {
                source: String::new(),
            },
            qblocks: tasks
                .iter()
                .map(|task| IntermediateQBlock {
                    id: task.id.clone(),
                    section: (!task.section.is_empty()).then(|| task.section.clone()),
                    source_text: task.source_text.clone(),
                    targets: task
                        .answers
                        .iter()
                        .zip(&task.target_types)
                        .map(|(answer, target_type)| IntermediateTarget {
                            answer: answer.clone(),
                            target_type: target_type.clone().unwrap_or_else(|| "term-name".into()),
                        })
                        .collect(),
                    warnings: Vec::new(),
                })
                .collect(),
        };
        let scaffold = ScaffoldDocument {
            tasks: tasks
                .iter()
                .map(|task| ScaffoldTask {
                    id: task.id.clone(),
                    source_text: task.source_text.clone(),
                    cloze_template: task.draft_question.clone(),
                    scaffold_question: task.draft_question.clone(),
                    blank_count: task.answers.len(),
                    answers: task.answers.clone(),
                })
                .collect(),
        };
        let leakage = tasks
            .iter()
            .map(|task| (task.id.clone(), task.leakage_baseline.clone()))
            .collect::<HashMap<_, _>>();
        execute_legacy(
            &intermediate,
            &scaffold,
            self.policy,
            composer,
            &[],
            context.run,
            context.events,
            context.progress,
            Some(&leakage),
            Some(plan),
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_legacy(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
    policy: ComposeExecutionPolicy,
    composer: &dyn QuestionComposer,
    extra_constraints: &[String],
    context: &RunContext,
    sink: &dyn EventSink,
    progress: &dyn ProgressSink,
    leakage_baselines: Option<&HashMap<String, Vec<usize>>>,
    prepared: Option<&PreparedComposePlan>,
) -> Result<GeneratedDocument, ComposeExecutionError> {
    crate::planner::execute_prepared_with_terminal_cause(
        intermediate,
        scaffold,
        policy,
        composer,
        extra_constraints,
        context,
        sink,
        progress,
        leakage_baselines,
        prepared,
    )
}
