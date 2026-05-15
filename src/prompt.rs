//! LLMに渡すプロンプトを組み立てる処理。

use crate::json::IntermediateDocument;

/// 中間データから文章補完問題生成用のプロンプトを作る。
pub fn build_generation_prompt(
    intermediate: &IntermediateDocument,
) -> Result<String, serde_json::Error> {
    let intermediate_json = serde_json::to_string_pretty(intermediate)?;
    let checklist = build_generation_checklist(intermediate)?;
    Ok(format!(
        r#"次のMarkdown qblock由来の中間データから，文章補完問題データを生成してください。

制約:
- [答え]{{type}} で指定された語句のみを答えにする
- answerの内容は targets[].answer の文字列をそのまま使う
- 文章は常体とすること
- typeは targets[].type の文字列をそのまま使う
- qblockごとに，入力targetsの先頭から順番に空欄化する
- question内の空欄順，answersの順序，入力targetsの順序を一致させる
- 空欄数とanswers数を必ず一致させる
- 1つのtargetにつき，question内に必ず1つの ＿＿＿ を置く
- answerを文中に残したままanswersへ入れない
- targetsにあるanswerはすべてanswersに含める
- targetsにないanswerを追加しない
- 意味が近いtarget同士でも，1つの空欄にまとめない
- 元ノートの内容をそのまま穴埋めにせず，表現を少し変えて文章補完問題として再構成する
- 元ノートにない知識を追加しない
- 不明な点や不自然な点があればwarningsに書く

出力:
- JSONのみを出力する
- Markdownのコードフェンスは付けない
- ルートキーは questions にする
- 各questionには id, section, type, targets, question, answers, source_text, explanation, tags, warnings を含める
- section は入力qblockのsectionをそのまま含める。入力にない場合は空文字列にする
- type は context-cloze にする
- targets は入力のtargetsをそのまま含める
- answers は文字列だけの配列にする。入れ子配列は使わない
- tags と warnings が空の場合は空配列にする
- question内の空欄は必ず ＿＿＿ を使う

生成前チェックリスト:
{checklist}

中間データ:
{intermediate_json}"#
    ))
}

fn build_generation_checklist(
    intermediate: &IntermediateDocument,
) -> Result<String, serde_json::Error> {
    let mut lines = Vec::new();
    for qblock in &intermediate.qblocks {
        let answers = qblock
            .targets
            .iter()
            .map(|target| target.answer.as_str())
            .collect::<Vec<_>>();
        lines.push(format!(
            "- {}: blanks={}, answers={}",
            qblock.id,
            answers.len(),
            serde_json::to_string(&answers)?
        ));
    }
    Ok(lines.join("\n"))
}
