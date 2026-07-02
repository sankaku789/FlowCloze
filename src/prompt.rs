//! 中間データから問題生成用のLLMプロンプトを組み立てる．

use crate::json::IntermediateDocument;
use crate::scaffold::ScaffoldDocument;

/// 中間データと生成前チェックリストを含むプロンプトを作る．
pub fn build_generation_prompt(
    intermediate: &IntermediateDocument,
) -> Result<String, serde_json::Error> {
    let intermediate_json = serde_json::to_string_pretty(intermediate)?;
    let checklist = build_generation_checklist(intermediate)?;
    Ok(format!(
        r#"次のMarkdown qblock由来の中間データから，文章補完問題データを生成してください．

制約:
- [答え] または [答え]{{type}} で指定された語句のみを答えにする
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
- source_text全体を問題文の素材として扱い，target以外の説明・箇条書き・見出し相当の文脈も省略せずquestionに残す
- target以外の語句は空欄にせず，学習者が文脈を読める通常の文章として残す
- qblockが大きい場合でも1つのquestionにまとめる．必要なら改行や箇条書きの形を保って読みやすくする
- source_textをそのまま抜き出してtargetだけを置換しただけの出力にしない
- Markdownの箇条書き断片ではなく，学習者に提示する文章補完問題として自然な本文に再構成する
- source_textの文脈や情報量を保ちながら，文同士のつながりを補い，必要に応じて短い導入文を加える
- 箇条書きは必要な場合だけ使い，その場合も問題文として読める表現に整える
- 元ノートにない知識を追加しない
- 不明な点や不自然な点があればwarningsに書く

出力:
- JSONのみを出力する
- Markdownのコードフェンスは付けない
- ルートキーは questions にする
- 各questionには id, section, type, targets, question, answers, source_text, explanation, tags, warnings を含める
- section は入力qblockのsectionをそのまま含める．入力にない場合は空文字列にする
- type は context-cloze にする
- targets は入力のtargetsをそのまま含める
- answers は文字列だけの配列にする．入れ子配列は使わない
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

/// scaffoldをもとに，LLMへquestion本文だけを生成させるプロンプトを作る．
pub fn build_question_composer_prompt(
    scaffold: &ScaffoldDocument,
    extra_constraints: &[String],
    retry_feedback: &[String],
) -> Result<String, serde_json::Error> {
    let scaffold_json = serde_json::to_string_pretty(scaffold)?;
    let mut prompt = format!(
        r#"次のscaffoldから，文章補完問題のquestion本文だけを自然な文章へ再構成してください。

制約:
- 出力はJSONのみとし，Markdownコードフェンスを付けない
- ルートキーは questions にする
- 各questionには id と question だけを含める
- section, type, targets, answers, source_text, explanation, tags, warnings は出力しない
- scaffold.tasks[].id と同じidだけを返す
- question内の空欄は必ず ＿＿＿ を使う
- taskごとの ＿＿＿ の数を blank_count と必ず一致させる
- ＿＿＿ の順序は answers の順序と一致させる
- answers に含まれる語句を question 本文に戻さない
- 元ノートにない知識を追加しない
- 箇条書き断片は必要に応じて文章化する
- 文章は常体にする
- 段落先頭は必要に応じて全角スペースに整える
- source_text と scaffold_question の情報量を保ち，target以外の説明を不自然に省略しない

入力scaffold:
{scaffold_json}"#
    );

    if !extra_constraints.is_empty() {
        prompt.push_str("\n\n追加制約:\n");
        for constraint in extra_constraints {
            prompt.push_str("- ");
            prompt.push_str(constraint);
            prompt.push('\n');
        }
    }

    if !retry_feedback.is_empty() {
        prompt.push_str(
            "\n\n前回の出力は検証に失敗しました。次のエラーを修正し，JSONのみを再出力してください。\n",
        );
        for feedback in retry_feedback {
            prompt.push_str("- ");
            prompt.push_str(feedback);
            prompt.push('\n');
        }
    }

    Ok(prompt)
}
