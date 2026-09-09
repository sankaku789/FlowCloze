use flowcloze::{GenerationConfig, PlanMarkdownOptions, PlanMarkdownOutcome};
use std::{fs, process};

pub(crate) fn run(input_path: &str, config: &GenerationConfig) {
    let markdown = fs::read_to_string(input_path).unwrap_or_else(|e| {
        eprintln!("{input_path} を読めませんでした: {e}");
        process::exit(1)
    });
    let options = PlanMarkdownOptions {
        policy: config.execution_policy(),
        offline: config.offline,
        quota: config.quota.clone(),
    };
    match flowcloze::application::plan::plan_markdown(&markdown, options) {
        Ok(plan) => print_summary(&plan),
        Err(e) => {
            eprintln!("planの作成に失敗しました: {e}");
            process::exit(1);
        }
    }
}

fn print_summary(plan: &PlanMarkdownOutcome) {
    println!("qblocks: {}", plan.total_qblocks);
    println!(
        "API: {} qblocks / {} requests",
        plan.provider_qblocks,
        plan.provider_batches.len()
    );
    println!(
        "Identity: {} qblocks / {} internal batches (no API)",
        plan.identity_qblocks.len(),
        plan.identity_batches
    );
    println!();
    println!(
        "limits: qblocks={} input={} output={} blanks={}",
        plan.effective_policy.max_tasks_per_batch,
        count(plan.effective_policy.max_estimated_input_tokens),
        count(plan.effective_policy.max_estimated_output_tokens),
        plan.effective_policy.max_blanks_per_batch
    );
    for batch in &plan.provider_batches {
        println!();
        println!(
            "batch {}: {} qblocks | input {}/{} | output {}/{} | blanks {}/{}",
            batch.number,
            batch.qblocks.len(),
            count(batch.input_tokens),
            count(plan.effective_policy.max_estimated_input_tokens),
            count(batch.expected_output_tokens),
            count(plan.effective_policy.max_estimated_output_tokens),
            batch.blanks,
            plan.effective_policy.max_blanks_per_batch
        );
        for qblock in &batch.qblocks {
            let note = if qblock.oversized {
                " [oversized singleton]"
            } else if qblock.isolated_heavy {
                " [heavy singleton]"
            } else {
                ""
            };
            println!(
                "  #{} {}: input={} output={} blanks={}{}",
                qblock.position,
                qblock.id,
                count(qblock.input_tokens),
                count(qblock.expected_output_tokens),
                qblock.blanks,
                note
            );
        }
    }
    if !plan.identity_qblocks.is_empty() {
        println!();
        println!("identity (no API):");
        for qblock in &plan.identity_qblocks {
            println!("  #{} {}", qblock.position, qblock.id);
        }
    }
}

fn count(value: usize) -> String {
    let digits = value.to_string();
    let mut output = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(ch);
    }
    output
}
