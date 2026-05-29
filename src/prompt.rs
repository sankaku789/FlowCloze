//! 中間データから問題生成用のLLMプロンプトを組み立てる．

use crate::json::IntermediateDocument;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct GeminiPromptDocument {
    tasks: Vec<GeminiPromptTask>,
}

#[derive(Debug, Serialize)]
struct GeminiPromptTask {
    id: String,
    cloze_template: String,
    blank_count: usize,
    answers: Vec<String>,
}

/// 中間データと生成前チェックリストを含むプロンプトを作る．
pub fn build_generation_prompt(
    intermediate: &IntermediateDocument,
) -> Result<String, serde_json::Error> {
    let prompt_input = build_prompt_input(intermediate);
    let prompt_json = serde_json::to_string_pretty(&prompt_input)?;
    Ok(format!(
        r#"あなたの責務は，バラバラのノート文を1つの文章補完問題の本文へ編集することです．
解答・target・source_textの管理は後処理が行います．あなたはquestion本文の編集に集中してください．
answersは空欄を戻したときに文が自然か確認するためだけに使い，出力JSONには含めないでください．

やること:
- tasks 1件につき questions 1件を同じ順序で作る
- 生成対象は question 本文だけ
- question は tasks[].cloze_template の「元の文章」を読み，「穴埋め下書き」にある断片的な文を1つの自然な文章補完問題の文章へ整えたものにする
- question内の ＿＿＿ の数は tasks[].blank_count と一致させる
- questionの ＿＿＿ は tasks[].answers と同じ順序で対応する
- 穴埋め下書きにある ＿＿＿ の数と順序は変えない
- ＿＿＿ は元の穴埋め下書きのanswer範囲を表すので，空欄の中身を分割して一部だけ本文側へ出さない
- ＿＿＿ の直前・直後にある助詞や語尾（例: される，する，とは言えない）は，下書きの文法関係を保つために必要なら残す
- ＿＿＿ に対応する答え語句を question 内に残さない
- 各answerを順に空欄へ戻したとき，question全体が常体の自然な日本語文になるようにする
- targetでない説明・条件・例・比較は，文同士をつなぎ直しながら本文に残す
- 箇条書きや見出しは，必要に応じて読める文章へ言い換える
- question本文は常体（だ・である調）で書く
- question本文の各段落は全角スペースで始める
- 元ノートにない知識は足さない

コード側で行うこと:
- section / type / targets / answers / source_text は中間データから決定的に組み立てる
- questionの並び順も中間データのtasks順に整える
- あなたはこれらの固定フィールドを推測・整形しなくてよい

出力:
- JSONのみ
- ルートキーは questions
- 各questionは id と question だけを持つ

Gemini用入力:
{prompt_json}"#
    ))
}

fn build_prompt_input(intermediate: &IntermediateDocument) -> GeminiPromptDocument {
    GeminiPromptDocument {
        tasks: intermediate
            .tasks
            .iter()
            .map(|task| GeminiPromptTask {
                id: task.id.clone(),
                cloze_template: task.cloze_template.clone(),
                blank_count: task.answers.len(),
                answers: task.answers.clone(),
            })
            .collect(),
    }
}
