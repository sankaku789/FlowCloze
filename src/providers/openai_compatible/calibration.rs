use crate::compose::{ComposeBatchOutput, ComposeBatchRequest, ComposeError, ComposeTask};

const CALIBRATION_ID: &str = "__flowcloze_segment_calibration__";
const CALIBRATION_SCAFFOLD: &str = "<BLANK_0>は利用者が本人であることを確かめる仕組みである。<BLANK_1>は許可する操作範囲を決める仕組みである。<BLANK_2>は異常時の処理を扱う仕組みである。";
const CALIBRATION_TARGETS: [&str; 3] = ["認証", "認可", "例外処理"];

pub(super) fn build_segment_calibration_request() -> ComposeBatchRequest {
    ComposeBatchRequest {
        batch_id: "segment-calibration".to_string(),
        tasks: vec![ComposeTask {
            id: CALIBRATION_ID.to_string(),
            scaffold_question: CALIBRATION_SCAFFOLD.to_string(),
            targets: CALIBRATION_TARGETS
                .iter()
                .map(|target| (*target).to_string())
                .collect(),
            blank_count: CALIBRATION_TARGETS.len(),
        }],
        prompt_version: "compose-segment-calibration".to_string(),
        extra_constraints: vec![
            "これは実タスク前のcalibrationである。このtaskではsegmentsの各文字列を1文字も書き換えず、そのまま返す。target値はsegmentsへ含めず、target境界だけを正しく維持する。"
                .to_string(),
        ],
        retry_feedback: Vec::new(),
    }
}

pub(super) fn analyze_segment_calibration_output(output: &ComposeBatchOutput) -> Vec<String> {
    let Some(item) = output.items.iter().find(|item| item.id == CALIBRATION_ID) else {
        return vec![
            "Calibrationでtask idを維持できなかった。実タスクでは入力idとsegments数を厳密に維持すること。"
                .to_string(),
        ];
    };

    let mut feedback = Vec::new();
    let leaked_targets = CALIBRATION_TARGETS
        .iter()
        .copied()
        .filter(|target| item.question.contains(target))
        .collect::<Vec<_>>();
    if !leaked_targets.is_empty() {
        feedback.push(format!(
            "Calibrationではtarget値 ({}) がsegment本文にも残り、再挿入時に重複する状態になった。target値そのものをsegmentsへ書かないこと。",
            leaked_targets.join(", ")
        ));
    }

    if item.question != CALIBRATION_SCAFFOLD {
        feedback.push(
            "Calibrationでは固定するよう指定したtarget境界または周辺文面が移動した。実タスクでは文章を任意位置で分割せず、segments[i]とsegments[i+1]の境界をtargets[i]の元の意味位置として維持すること。"
                .to_string(),
        );
    }

    feedback
}

pub(super) fn segment_calibration_error_feedback(error: &ComposeError) -> Vec<String> {
    vec![format!(
        "Calibrationの出力を正しく解釈できなかった ({error})。実タスクではtask id、segments数、JSON出力契約を厳密に維持し、target値をsegmentsへ含めないこと。"
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{ComposeMetadata, ComposedItem};

    fn output(question: &str) -> ComposeBatchOutput {
        ComposeBatchOutput {
            items: vec![ComposedItem {
                id: CALIBRATION_ID.to_string(),
                question: question.to_string(),
            }],
            metadata: ComposeMetadata::default(),
        }
    }

    #[test]
    fn calibration_request_uses_three_target_boundaries() {
        let request = build_segment_calibration_request();
        assert_eq!(request.tasks.len(), 1);
        assert_eq!(request.tasks[0].blank_count, 3);
        assert_eq!(request.tasks[0].targets.len(), 3);
        assert_eq!(request.tasks[0].scaffold_question, CALIBRATION_SCAFFOLD);
    }

    #[test]
    fn matching_calibration_output_needs_no_feedback() {
        assert!(analyze_segment_calibration_output(&output(CALIBRATION_SCAFFOLD)).is_empty());
    }

    #[test]
    fn calibration_reports_target_leakage_and_boundary_drift() {
        let feedback = analyze_segment_calibration_output(&output(
            "認証<BLANK_0>は利用者が本人であることを確かめる仕組みである。<BLANK_1><BLANK_2>",
        ));
        assert_eq!(feedback.len(), 2);
        assert!(feedback.iter().any(|line| line.contains("target値")));
        assert!(feedback.iter().any(|line| line.contains("target境界")));
    }
}
