//! 中間データから問題生成用のLLMプロンプトを組み立てる．

use crate::json::IntermediateDocument;

/// 中間データと生成前チェックリストを含むプロンプトを作る．
pub fn build_generation_prompt(
    intermediate: &IntermediateDocument,
) -> Result<String, serde_json::Error> {
    let intermediate_json = serde_json::to_string_pretty(intermediate)?;
    let checklist = build_generation_checklist(intermediate)?;
    Ok(format!(
        r#"あなたの責務は，バラバラのノート文を1つの文章補完問題の本文へ編集することです．
解答・target・source_textの管理は後処理が行います．あなたはquestion本文の編集に集中してください．

やること:
- tasks 1件につき questions 1件を同じ順序で作る
- 生成対象は question 本文だけ
- question は tasks[].cloze_template の「元の文章」を読み，「穴埋め下書き」にある断片的な文を1つの自然な文章補完問題の文章へ整えたものにする
- 穴埋め下書きにある ＿＿＿ の数と順序は変えない
- ＿＿＿ に対応する答え語句を question 内に残さない
- targetでない説明・条件・例・比較は，文同士をつなぎ直しながら本文に残す
- 箇条書きや見出しは，必要に応じて読める文章へ言い換える
- question本文は常体（だ・である調）で書く
- 元ノートにない知識は足さない

コード側で行うこと:
- section / type / targets / answers / source_text は中間データから決定的に組み立てる
- questionの並び順も中間データのtasks順に整える
- あなたはこれらの固定フィールドを推測・整形しなくてよい

出力:
- JSONのみ
- ルートキーは questions
- 各questionは id と question だけを持つ

空欄数チェック:
{checklist}

中間データ:
{intermediate_json}"#
    ))
}

fn build_generation_checklist(
    intermediate: &IntermediateDocument,
) -> Result<String, serde_json::Error> {
    let mut lines = Vec::new();
    for task in &intermediate.tasks {
        lines.push(format!(
            "- {}: question内の ＿＿＿ は{}個",
            task.id,
            task.answers.len()
        ));
    }
    Ok(lines.join("\n"))
}
