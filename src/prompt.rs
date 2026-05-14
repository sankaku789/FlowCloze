//! LLMに渡すプロンプトを組み立てる処理。

use crate::json::IntermediateDocument;

/// 中間データから文章補完問題生成用のプロンプトを作る。
pub fn build_generation_prompt(
    intermediate: &IntermediateDocument,
) -> Result<String, serde_json::Error> {
    let intermediate_json = serde_json::to_string_pretty(intermediate)?;
    Ok(format!(
        r#"次のMarkdown qblock由来の中間データから，文章補完問題データを生成してください。

制約:
- [答え]{{type}} で指定された語句のみを答えにする
- answerの内容は targets[].answer の文字列をそのまま使う
- 文章は常体とすること
- typeは targets[].type の文字列をそのまま使う
- answersの順序は，問題文の空欄順に一致させる
- 空欄数とanswers数を必ず一致させる
- targetsにあるanswerはすべてanswersに含める
- targetsにないanswerを追加しない
- 元ノートの内容をそのまま穴埋めにせず，表現を少し変えて文章補完問題として再構成する
- 元ノートにない知識を追加しない
- 不明な点や不自然な点があればwarningsに書く

出力:
- JSONのみを出力する
- Markdownのコードフェンスは付けない
- ルートキーは questions にする
- 各questionには id, type, title, targets, question, answers, source_text, explanation, tags, warnings を含める
- type は context-cloze にする
- targets は入力のtargetsをそのまま含める
- answers は文字列だけの配列にする。入れ子配列は使わない
- tags と warnings が空の場合は空配列にする
- question内の空欄は必ず ＿＿＿ を使う

中間データ:
{intermediate_json}"#
    ))
}
