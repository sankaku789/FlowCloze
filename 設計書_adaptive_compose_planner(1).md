# FlowCloze 設計書

## 1. 背景

FlowCloze は、Markdown ノート内の `#qblock` と `[answer]{type}` 記法から、文章補完問題を生成する CLI ツールである。

現状では、Markdown を中間表現へ変換したあと、Gemini API に問題本文の生成を依頼し、生成結果 JSON を検証・正規化して保存している。

今後は Gemini API への密結合を解消し、ローカル LLM である Gemma 系モデルにも対応したい。  
ただし、LLM なしで実用的な問題文を生成することは目的にしない。

FlowCloze の価値は、厳格な教材記法を書かせることではなく、ある程度ラフに書かれた Markdown ノートから、自然な文章補完問題を作れる点にある。  
そのため、Markdown ノートの書き方を過度に制約する設計は避ける。

本設計では、LLM を排除するのではなく、LLM の責務を極力小さくし、差し替え可能にする。

## 2. 目的

### 2.1 達成したいこと

- Gemini API への密結合を解消する
- ローカル LLM を利用できるようにする
- LLM の責務を `question` 本文生成・整形に限定する
- `id`, `section`, `type`, `targets`, `answers`, `source_text` などの固定フィールドをコード側で決定的に構築する
- LLM の出力を検証しやすい形にする
- Markdown 入力の自由度を維持する
- LLM バックエンドを Gemini / local で切り替えられるようにする
- **answer leakage（answer に相当する語句が question 本文にそのまま出力される不具合）を検出できるようにする**
  - これは既存の Gemini パイプラインにも当てはまる問題であり、local LLM 対応より先に対応する（詳細は §4.6.1, §13 Phase 0）

### 2.2 達成しないこと

- LLM なしで実用的な問題 JSON を生成すること
- Markdown ノートの書き方を厳格に縛ること
- ローカル LLM と Gemini の品質を完全に一致させること
- LLM に `answers`, `targets`, `source_text` などの固定フィールドを推測させること
- 決定的レンダリングだけで自然な文章補完問題を完成させること

## 3. 基本方針

FlowCloze の中核は、Markdown から中間表現を作り、固定情報をコード側で管理したうえで、LLM に `question` 本文だけを生成させるパイプラインである。

```text
Markdown
  ↓
Parser
  ↓
IntermediateDocument
  ↓
Deterministic Scaffold Builder
  ↓
Adaptive Compose Planner
  ↓
LLM Question Composer
  ↓
Result Collector
  ↓
Normalizer / Merger
  ↓
Validator
  ↓
GeneratedDocument
```

ここで `Deterministic Scaffold Builder` は、最終問題文を作るための機能ではない。  
LLM に渡す足場、検証材料、プロンプト入力を作るための内部工程である。

LLM は必要だが、任せる範囲を `question` 本文だけに限定する。

## 4. 責務分離

### 4.1 Parser

Markdown から qblock と answer target を抽出する。

責務:

- `#qblock{...}` の範囲検出
- `[answer]` および `[answer]{type}` の検出
- section heading の検出
- 段落境界の保持
- 元テキストの保持

Parser は入力をできるだけ寛容に扱う。  
ただし、構文として解釈できないものは明確なエラーにする。

### 4.2 Intermediate Builder

Parser の結果から `IntermediateDocument` を構築する。

責務:

- task id の付与
- source text の保持
- answer list の構築
- target list の構築
- cloze template の構築
- blank count の算出
- section の紐付け

ここでは LLM を使わない。

### 4.3 Deterministic Scaffold Builder

`IntermediateDocument` から、LLM に渡すための scaffold を作る。

責務:

- `cloze_template` をもとに scaffold question を作る
- 空欄 `＿＿＿` の数と順序を保持する
- 元ノートの構造を可能な限り保持する
- LLM に「何を自然化すべきか」を明示する材料を作る

Scaffold は最終成果物ではない。  
そのため、自然な文章である必要はない。

例:

```markdown
#qblock{
- 短期記憶: [ワーキングメモリ]{term}
- 容量: [7±2]{number}
}
```

Scaffold 例:

```text
短期記憶: ＿＿＿
容量: ＿＿＿
```

これは最終問題文ではなく、LLM に渡すための下書きである。

### 4.4 LLM Question Composer

Scaffold と元ノートをもとに、自然な文章補完問題の `question` 本文を生成する。

責務:

- scaffold question を自然な文章へ再構成する
- 箇条書きを必要に応じて文章化する
- 文同士を自然につなぐ
- 常体に整える
- 段落先頭を全角スペースに整える
- 元ノートにある情報だけを使う

禁止事項:

- 空欄の数を変える
- 空欄の順序を変える
- answer に相当する語句を本文に戻す
- 元ノートにない知識を追加する
- `answers`, `targets`, `source_text` を生成・推測する
- JSON の固定フィールドを作る

LLM の入力は、なるべく明示的にする。

```json
{
  "tasks": [
    {
      "id": "q1",
      "source_text": "...",
      "cloze_template": "...",
      "scaffold_question": "...",
      "blank_count": 2,
      "answers": ["...", "..."]
    }
  ]
}
```

LLM の出力は `id` と `question` のみに限定する。

```json
{
  "questions": [
    {
      "id": "q1",
      "question": "..."
    }
  ]
}
```

### 4.4.1 構造化出力（response_format / JSON Schema）の扱い（調査結果）

`id`/`question` へ出力を絞るだけでなく、可能な backend では構造化出力（JSON Schema 制約付きデコード）を併用し、JSON parse error 自体を減らす。ただし backend ごとに対応状況・安定性が異なるため、**構造化出力は「あれば使う最適化」とし、Validator による事後検証と retry を安全網として必ず残す**。

調査結果（2026年時点）:

```text
Gemini API
  - generateContent の generationConfig.response_mime_type / response_json_schema で
    JSON Schema 制約付き出力が可能（既存の gemini.rs が既に利用している）
  - 現状の schema は id/section/type/targets/... を全て required にしており，
    4.4 で定めた「id/question のみ」という契約と矛盾している
  - Phase 2 でスキーマ自体を id/question のみに絞る（§7.3 参照）

Ollama（gpt-oss, gemma 系を含む）
  - v0.3 以降，chat/generate API の format パラメータに JSON Schema を渡すことで
    grammar 制約付き JSON 出力が可能
  - gpt-oss を含む主要モデルで動作実績あり
  - 素の json のみでも {"format": "json"} で崩れにくくなる

llama.cpp server（OpenAI互換 /v1/chat/completions）
  - response_format: {"type": "json_schema", ...} をサポート
  - ただし gpt-oss（harmony chat format）と組み合わせた場合，
    reasoning channel のトークンが content に混入してレスポンス全体の
    パースに失敗する不具合が報告されている（2026年4月時点，未解決の場合がある）
  - GPT-OSS を local backend で使う場合は，llama.cpp server 経由よりも
    Ollama 経由の方が現状安定している可能性が高い

vLLM（OpenAI互換 /v1/chat/completions）
  - response_format（json_schema）/ guided_json による構造化出力に対応
  - バックエンド（xgrammar / outlines / guidance など）により
    対応する JSON Schema の機能差がある（例: 複雑な $ref やネストで失敗する報告あり）
```

設計方針への反映:

- `llm::client::LlmClient` trait には構造化出力の可否を必須にしない
  （`generate_text(&self, prompt: &str) -> Result<String, LlmError>` のままでよい）
- 構造化出力が使える backend では，実装側（`gemini.rs`, `local_openai.rs` 等）が
  内部で response_format / format を設定してよい。ただし公開契約は
  「`id`/`question` のみを持つ JSON 文字列を返す」ことのみとし，
  呼び出し側（Composer / Result Collector）は構造化出力の有無を意識しない
- Validator と retry ループは，構造化出力が効かない backend／モデルの組み合わせに対する
  安全網として引き続き必須とする（構造化出力があるからといって Validator を弱めない）
- `LOCAL_LLM_BASE_URL` を Ollama 互換にするか llama.cpp server 互換にするかで
  gpt-oss の安定性が変わりうる点を README／設定例に明記する（§6.5, §7.4）

### 4.5 Normalizer / Merger

LLM の出力と中間表現を合成し、最終的な `GeneratedDocument` を作る。


責務:

- LLM 出力から `question` だけを採用する
- `id`, `section`, `type`, `targets`, `answers`, `source_text` は中間表現から再構築する
- paragraph indent を補正する
- 余分な Markdown code fence を除去する
- JSON 以外の前置きや後書きが混ざった場合は可能な範囲で抽出する

LLM が `question` を返せなかった task は、成功扱いにしない。  
Scaffold をそのまま最終成果物として保存することは、標準動作では行わない。

### 4.6 Validator

生成結果を検証する。

責務:

- question 数が task 数と一致すること
- question id が task id と対応すること
- question 内の空欄数が answer 数と一致すること
- answer が question 内に漏れていないこと
- answers の順序が維持されていること
- targets が中間表現と一致すること
- source_text が中間表現と一致すること
- 必須フィールドが存在すること

Validator は厳密にする。  
ただし、Markdown の書き方に関する指摘は warning として扱う。

### 4.6.1 Answer Leakage 検出（優先実装）

現状の Validator（`validate_documents`）には、この検証が実装されていない。  
LLM バックエンドの抽象化やローカル LLM 対応を待たず、**既存の Gemini パイプラインに対して先行して実装する**。

理由:

- answer leakage は LLM バックエンドに依存しない、既存パイプラインでも起こりうる不具合である
- 空欄数と answers 数が一致していても、本文中に answer の語句がそのまま残っていれば、穴埋め問題として成立しない
- 検出ロジック自体は `IntermediateDocument` と生成済み `question` だけで完結し、Adaptive Compose Planner や LlmClient 抽象化に依存しない

検出方法（初期実装）:

```text
対象: 生成された question 本文（＿＿＿ を含む状態のテキスト）
判定: task.answers に含まれる各 answer 文字列が、
      question 本文中に部分文字列として出現するか

出現した場合:
  ValidationError::AnswerLeakage { id, answer } を報告する
```

補足:

- 判定は「＿＿＿ に戻すと重複してしまう」ケースを拾うための保守的なチェックであり、完全な意味解析は行わない
- answer が短い一般語（例: 助詞、汎用的な単語）の場合、誤検知（false positive）が増える可能性がある
  - 初期実装では誤検知を許容し、warning ではなく error として扱う
  - 誤検知が実運用上多い場合は、大局的な緩和（最小文字数閾値など）を Phase 0 の中で検討する
- retry 時のフィードバック文にも、どの id のどの answer が漏れたかを含める（既存の `build_validation_retry_feedback` の仕組みをそのまま使う）

## 5. Lint と Warning

Markdown 入力の自由度を維持するため、書き方の制約は原則としてエラーではなく warning として扱う。

例:

- 箇条書きが多いため、LLM による文章化の難度が高い
- 1つの qblock に answer が多すぎる
- source text が短すぎる
- answer の前後に助詞がなく、自然な穴埋めにならない可能性がある
- 同じ answer が複数回出現している

Warning は生成を止めない。  
ただし、qblock と answer target を構文的に解釈できない場合はエラーにする。

## 6. CLI 設計

### 6.1 基本

```bash
flowcloze generate input.md
```

標準モード。  
設定された LLM backend を使って `question` 本文を生成する。

### 6.2 Backend 指定

```bash
flowcloze generate --backend gemini input.md
flowcloze generate --backend local input.md
```

### 6.3 モデル指定

```bash
flowcloze generate --backend gemini --model gemini-2.5-flash input.md
flowcloze generate --backend local --model gemma-4-e2b input.md
```

### 6.4 Scaffold 確認

Scaffold は最終成果物ではないため、`generate --no-llm` のような生成モードは標準では提供しない。

代わりに、デバッグ用途として scaffold を確認できるコマンドを用意する。

```bash
flowcloze inspect-scaffold input.md
```

このコマンドは、LLM に渡される下書きや入力 JSON を確認するためのものであり、学習用の最終問題 JSON を出力するものではない。

### 6.5 設定

`.env` 例:

```env
FLOWCLOZE_LLM_BACKEND=local

GEMINI_API_KEY=...
GEMINI_MODEL=gemini-2.5-flash

LOCAL_LLM_BASE_URL=http://localhost:11434/v1
LOCAL_LLM_MODEL=gemma-4-e2b
LOCAL_LLM_API_KEY=
```

## 7. Rust モジュール構成案

```text
src/
  main.rs
  lib.rs

  parser.rs
  json.rs

  render/
    mod.rs
    scaffold.rs
    normalize.rs
    merge.rs

  llm/
    mod.rs
    client.rs
    gemini.rs
    local_openai.rs
    prompt.rs
    response.rs
    token_estimate.rs

  validate.rs
  csv.rs
  pdf.rs
  view.rs
```

### 7.1 `render::scaffold`

```rust
pub fn build_scaffold_document(
    intermediate: &IntermediateDocument,
) -> ScaffoldDocument
```

LLM に渡す scaffold を構築する。

### 7.2 `llm::client`

```rust
pub trait LlmClient {
    fn generate_text(&self, prompt: &str) -> Result<String, LlmError>;
}
```

Gemini と Local LLM はこの trait を実装する。

### 7.3 `llm::gemini`

Gemini generateContent API 用クライアント。

### 7.4 `llm::local_openai`

OpenAI 互換 API 用クライアント。  
Ollama、llama.cpp server、vLLM などを想定する。

### 7.4.1 `llm::token_estimate`（複数モデル対応のトークン推定）

背景:

- Gemini（SentencePiece 系）、Gemma（SentencePiece）、GPT-OSS（`o200k_harmony`、tiktoken 系 BPE）は、それぞれ異なるトークナイザを使う
- BatchPolicy の `max_estimated_input_tokens` のために、backend ごとに正確なトークナイザを実装・同梱するのはコストが高く、モデル追加のたびにメンテナンスが必要になる
- 一方で、token 推定の目的は「明らかに大きすぎる batch を作らない」ことであり、課金額の厳密な算出ではない

方針:

```rust
pub trait TokenEstimator {
    fn estimate(&self, text: &str) -> usize;
}
```

- 初期実装では、全 backend 共通で**文字数ベースの近似 Estimator** をデフォルトにする
  - 日本語（かな・漢字主体）: 文字数 × 係数（初期値 1.0〜1.5 程度、実測して調整する）
  - 英数字主体のテキスト: 4 文字 ≒ 1 トークン程度を目安にする
  - 実装は単純な文字種判定（ひらがな/カタカナ/漢字/その他）による按分でよい
- `TokenEstimator` を trait 化しておくことで、将来的に特定モデル向けの厳密な実装（例: GPT-OSS 向けに `tiktoken` の `o200k_harmony` 相当を使う Estimator）を**既存の Batch Policy ロジックを変更せずに**差し替えられるようにする
- ただし初期実装では厳密な Estimator は作らない（過剰投資を避ける）
- 見積もりが外れて batch が大きすぎた場合は、§17.7 の「JSON parse error → batch を小さくして再試行」で吸収する。**token 推定はあくまで初期の batch サイズを決めるためのヒューリスティックであり、厳密さより単純さを優先する**

`.env` 例（将来的な拡張の余地として残す）:

```env
FLOWCLOZE_TOKEN_ESTIMATOR=char_heuristic
```

### 7.5 `llm::prompt`

Question Composer 専用プロンプトを構築する。

```rust
pub fn build_question_composer_prompt(
    intermediate: &IntermediateDocument,
    scaffold: &ScaffoldDocument,
) -> Result<String, serde_json::Error>
```

### 7.6 `render::merge`

LLM が返した `id/question` のみを中間表現に反映する。

```rust
pub fn merge_composed_questions(
    intermediate: &IntermediateDocument,
    composed: ComposedDocument,
) -> GeneratedDocument
```

## 8. データ構造案

### 8.1 Scaffold

```rust
#[derive(Debug, Serialize)]
pub struct ScaffoldDocument {
    pub tasks: Vec<ScaffoldTask>,
}

#[derive(Debug, Serialize)]
pub struct ScaffoldTask {
    pub id: String,
    pub source_text: String,
    pub cloze_template: String,
    pub scaffold_question: String,
    pub blank_count: usize,
    pub answers: Vec<String>,
}
```

### 8.2 LLM 出力

```rust
#[derive(Debug, Deserialize)]
pub struct ComposedDocument {
    pub questions: Vec<ComposedQuestion>,
}

#[derive(Debug, Deserialize)]
pub struct ComposedQuestion {
    pub id: String,
    pub question: String,
}
```

## 9. 生成フロー

```text
read markdown
  ↓
parse_markdown
  ↓
IntermediateDocument::from_qblocks
  ↓
build_scaffold_document
  ↓
plan_compose_batches
  ↓
build_question_composer_prompt per batch
  ↓
LlmClient::generate_text
  ↓
parse ComposedDocument
  ↓
collect composed questions
  ↓
merge_composed_questions
  ↓
validate_generated_document
  ↓
retry or fail
  ↓
write json
```

## 10. 失敗時の扱い

### 10.1 LLM レスポンスが JSON として読めない

- JSON 抽出を試みる
- 失敗したら retry
- retry 上限に達したらエラーにする
- scaffold を最終成果物として保存しない

### 10.2 LLM 出力の検証に失敗した

- validation feedback を使って再試行する
- 再試行しても失敗する場合はエラーにする
- 失敗理由は stderr に表示する

### 10.3 ローカル LLM サーバーに接続できない

- エラーにする
- Gemini への自動フォールバックは標準では行わない
- 別 backend を使いたい場合は明示的に指定する

## 11. フォールバック方針

標準動作では、LLM 失敗時に scaffold を最終成果物として保存しない。

理由:

- scaffold は自然な問題文ではない
- Markdown の自由度を維持するほど、scaffold の可読性は保証できない
- ユーザーが期待する成果物品質とズレる可能性が高い

ただし、デバッグ目的で scaffold を出力する機能は提供する。

```bash
flowcloze inspect-scaffold input.md
```

## 12. テスト方針

### 12.0 Answer Leakage 検証テスト（Phase 0）

- answer が本文にそのまま残っている
- answer が本文に残っていない（正常系）
- 複数 answer のうち一部だけ漏れている
- 短い一般語の answer で誤検知が起きやすいケース（既知の限界として記録する）
- retry フィードバックに漏れた id / answer が含まれる

### 12.1 Scaffold Builder テスト

- 単一 answer
- 複数 answer
- 箇条書き
- 複数段落
- section 付き qblock
- answer type 付き
- answer type なし
- 日本語句読点
- 英数字混在

### 12.2 LLM Merge テスト

- LLM が全 question を返す
- 一部 question が欠ける
- 余分な id が返る
- 空欄数が変わる
- answer が本文に漏れる
- JSON code fence に包まれる
- 前置き文が混ざる

### 12.3 CLI テスト

- `generate --backend gemini`
- `generate --backend local`
- `.env` の既定値
- `--model` による上書き
- ローカル LLM 接続失敗時のエラー
- `inspect-scaffold`

## 13. 移行計画

### Phase 0: Answer Leakage 検証の追加（先行対応）

- `ValidationError::AnswerLeakage` を追加する（§4.6.1）
- `validate_documents` に answer leakage 判定を追加する
- 既存の Gemini パイプライン（`generate_with_gemini`）にそのまま適用する
- retry フィードバックに leakage 内容を含める
- LlmClient 抽象化や local LLM 対応、Adaptive Compose Planner の導入を待たずに着手する
- Phase 1 以降の作業と並行して進めてよい（依存関係がないため）

### Phase 1: Scaffold Builder 追加

- `build_scaffold_document` を追加する
- `inspect-scaffold` を追加する
- 既存の生成処理にはまだ影響させない

### Phase 2: Gemini 処理を Question Composer 化

- 既存の Gemini 生成処理を `compose_questions_with_gemini` に寄せる
- LLM 出力を `id/question` のみに限定する
- 固定フィールドは必ず merge 側で再構築する
- `gemini.rs` の `response_json_schema` を `id/question` のみの schema に絞る（§4.4.1）
  - 現状は全フィールドが required になっており，プロンプトの契約と矛盾しているため

### Phase 3: LLM 抽象化

- `LlmClient` trait を追加する
- `GeminiClient` を trait 実装へ移す
- エラー型を `LlmError` に統合する

### Phase 4: Local LLM 対応

- `LocalOpenAiClient` を追加する
- `LOCAL_LLM_BASE_URL` と `LOCAL_LLM_MODEL` を読む
- OpenAI 互換 API で chat completions を呼ぶ
- 対応可能な場合は `response_format`（json_schema）を `id/question` schema で設定する（§4.4.1）
  - ただし失敗時は response_format なしのプレーンな呼び出しにフォールバックする
- Ollama / llama.cpp server / vLLM を想定した README を追加する
  - GPT-OSS（harmony chat format）を local backend で使う場合の既知の注意点を明記する（§4.4.1）
  - 動作確認は Ollama 経由を優先し，llama.cpp server 経由は別途検証してから案内する

### Phase 5: 品質評価

- 実ノート fixture を用意する
- Gemini と Local LLM（GPT-OSS を含む）の出力を比較する
- 空欄数、answer leakage、自然さ、処理時間を確認する

## 14. リスク

### 14.1 ローカル LLM の JSON 出力が不安定

対策:

- 出力 JSON を `id/question` のみに限定する
- JSON 抽出処理を強化する
- validation retry を行う
- 失敗時は明確にエラーにする

### 14.2 LLM が元ノートにない知識を足す

対策:

- prompt で禁止する
- answer leakage を検出する（Phase 0 で先行実装済み、§4.6.1）
- source_text と照合可能な検証を増やす
- scaffold と source_text をプロンプトに明示する

### 14.3 Markdown 入力の自由度が高く、LLM が迷う

対策:

- scaffold を作って LLM への入力を安定させる
- warning を出す
- qblock 単位を小さくするヒントを表示する
- ただし Markdown 記法自体は厳格化しない

### 14.4 アーキテクチャ変更が大きい

対策:

- 既存機能を壊さず段階移行する
- まず scaffold を内部導入する
- Gemini 置換より先に LLM 入出力を狭める

## 15. 判断

FlowCloze は、LLM なしで問題を完成させるツールではなく、LLM の力を使って自然な文章補完問題を作るツールである。

ただし、LLM に任せる責務は最小化する。

LLM は次だけを担当する。

```text
source_text と scaffold_question をもとに、question 本文だけを自然化する
```

それ以外の情報はコード側で決定的に管理する。

この設計により、次の性質を両立する。

- Markdown 入力の自由度
- Gemini / Gemma / Local LLM の切り替え
- LLM 出力の検証可能性
- 固定フィールドの決定性
- 将来的な拡張性

## 16. まとめ

本設計の中心は以下である。

```text
Parser と Validator は厳密にする。
Markdown 入力は寛容にする。
Scaffold Builder は LLM の足場を作る。
LLM Question Composer は question 本文だけを担当する。
固定フィールドはコード側で決定的に構築する。
```

これにより、FlowCloze は Gemini API に密結合した生成ツールから、LLM バックエンドを差し替え可能な Markdown-to-Cloze 生成パイプラインへ移行できる。

## 17. Adaptive Compose Planner

### 17.1 背景

長い qblock や多数の qblock を処理する際、LLM にすべてを一度に投げる方式には次の問題がある。

- 1つの task の失敗が document 全体の再生成につながる
- retry 時に成功済み task まで巻き添えになる
- prompt が肥大化し、入力 token が無駄になりやすい
- 長文では JSON parse error や missing id が起きやすい
- backend ごとの最適な呼び出し単位が異なる

一方で、常に 1 qblock ずつ投げる方式にも問題がある。

- Gemini では request 数が増え、API 制限に当たりやすくなる
- Local LLM でも呼び出し overhead が増える
- task 間の文体が揺れやすい
- 短すぎる入力では LLM が十分な文脈を得られないことがある

そのため、固定的な一括方式や固定的な分割方式ではなく、backend policy と token budget に応じて呼び出し単位を調整する `Adaptive Compose Planner` を導入する。

### 17.2 基本方針

`qblock / task` は問題の意味単位である。  
`compose batch` は LLM 呼び出し最適化の単位である。

この2つを混同しない。

```text
qblock / task
  = ユーザーが指定した問題化範囲

compose batch
  = LLM に一度に投げる実行単位
```

初期実装では、次の方針を採る。

```text
通常時:
  token budget 内で複数 task を batch 化する

失敗時:
  失敗した task だけを小さな batch で再試行する

さらに失敗:
  task 単独で再試行する

巨大 task:
  初期実装では segment 化せず、明確なエラーまたは warning にする
```

重要なのは、document 全体を再生成しないことである。

### 17.3 初期実装でやること

初期実装では、以下の範囲に留める。

- token / 文字数ベースで task を batch 化する
- batch 単位で LLM に投げる
- batch 結果を task 単位で検証する
- 成功した task は確定する
- 失敗した task だけ retry queue に戻す
- retry 時は task 単独で投げる
- retry 上限を超えたらエラーにする

初期実装では qblock 内 segment 化は行わない。

理由:

- 文脈分断の危険が高い
- segment 結合後の検証が複雑になる
- 「前者」「後者」「この方法」などの照応関係が壊れやすい
- まずは batch / task 単位の再試行だけでも、現状の全体再生成より改善できる

### 17.4 将来的に検討すること

将来的に、1 task が大きすぎて単独でも処理できない場合のみ、segment 化を検討する。

その場合も、単純に段落を切って投げるのではなく、次の構造で LLM に渡す。

```json
{
  "task_id": "q1",
  "segment_id": "q1-s2",
  "context_before": "参照用。書き換え禁止。",
  "compose_target": "この範囲だけを書き換える。",
  "context_after": "参照用。書き換え禁止。",
  "blank_count_in_target": 2,
  "answers_in_target": ["...", "..."]
}
```

LLM には `compose_target` に対応する question fragment だけを返させる。  
segment 結合後は、qblock 全体で必ず final validation を行う。

ただし、segment 化は初期実装の対象外とする。

### 17.5 Batch Policy

backend ごとに初期 policy を変える。  
`max_estimated_input_tokens` の見積もりには §7.4.1 の `TokenEstimator`（初期実装は文字数ベースの近似）を使う。

Gemini は API request 数を抑えたいので、初期 batch を大きめにし、複数 batch を並列に投げてよい。

```rust
BatchPolicy {
    max_tasks_per_batch: 8,
    max_estimated_input_tokens: 12000,
    max_retry_count: 2,
    max_concurrent_batches: 3,
}
```

Local LLM は rate limit より品質と安定性を優先し、小さめ batch から始める。  
また、ローカルサーバー（Ollama 等）は同時リクエストに弱いことが多いため、初期値では並列化しない。

```rust
BatchPolicy {
    max_tasks_per_batch: 2,
    max_estimated_input_tokens: 4000,
    max_retry_count: 2,
    max_concurrent_batches: 1,
}
```

GPT-OSS のようなローカルモデルを Ollama 経由で使う場合も、上記の Local LLM policy をそのまま初期値として使う。  
GPT-OSS 固有のチューニング（batch サイズや並列数）が必要になった場合は、モデル名ごとの override として `.env` で上書きできるようにする。

`max_concurrent_batches` は「同時に投げてよい batch 数」を表す。  
Gemini のようにサーバー側のレート制御・スケーリングが期待できる backend では並列化のメリットが大きく、単一プロセスのローカルサーバーでは並列化がスループット向上につながらない（むしろ品質や安定性を損なう）ことが多い、という前提を初期値に反映する。

これらは固定仕様ではなく初期値である。  
`.env` や CLI option で上書きできるようにする。

```env
FLOWCLOZE_BATCH_POLICY=auto
FLOWCLOZE_MAX_TASKS_PER_BATCH=8
FLOWCLOZE_MAX_INPUT_TOKENS=12000
FLOWCLOZE_MAX_CONCURRENT_BATCHES=3
```

CLI 例:

```bash
flowcloze generate --backend gemini --batch auto input.md
flowcloze generate --backend local --batch small input.md
flowcloze generate --backend local --batch one-task input.md
```

### 17.6 状態遷移

初期実装では、task の compose mode は2つだけにする。

```rust
enum ComposeMode {
    Batched,
    SingleTask,
}
```

```rust
struct ComposeTaskState {
    task_id: String,
    estimated_tokens: usize,
    retry_count: u32,
    mode: ComposeMode,
}
```

状態遷移:

```text
Batched
  ├─ success → done
  └─ fail    → SingleTask

SingleTask
  ├─ success → done
  └─ fail    → error
```

将来的に segment 化を導入する場合のみ、`Segmented` を追加する。

```rust
enum ComposeMode {
    Batched,
    SingleTask,
    Segmented,
}
```

### 17.7 失敗理由ごとの再計画

失敗したら常に小さくする、という単純な方針にはしない。  
失敗理由によって再計画方法を変える。

```text
JSON parse error
  → batch が大きすぎる可能性がある
  → batch を小さくして再試行

missing task / missing id
  → batch 内で task が落ちた可能性がある
  → 該当 task を単独で再試行

blank count mismatch
  → 該当 task を単独で再試行

answer leakage
  → batch 縮小だけでは改善しない可能性がある
  → 制約を強めた retry prompt で単独再試行

元ノートにない知識の追加
  → 分割すると悪化する可能性がある
  → source_text と scaffold を明示した retry prompt で再試行
```

Planner は validation error を単なる失敗として扱わず、再計画の入力として扱う。

### 17.8 Result Collector

`Result Collector` は、batch ごとの LLM 出力を task 単位で集約する。

責務:

- 成功した task の `id/question` を保持する
- 失敗した task を retry queue に戻す
- 成功済み task を再生成しない
- 全 task が揃った時点で `ComposedDocument` を構築する
- 余分な id を無視または warning にする
- 同じ id が複数返った場合は最新 retry 結果を優先する

ただし、成功済み task であっても、最終的には document 全体で再検証する。

## 18. Adaptive Compose Planner の危険性と対策

### 18.1 文脈分断

長い qblock を segment 化すると、文章全体のつながりが壊れる可能性がある。

例:

```text
前段: この理論には2つの立場がある。
後段: 前者は＿＿＿であり、後者は＿＿＿である。
```

後段だけを LLM に渡すと、「前者」「後者」が何を指すか分からない。  
その結果、LLM が勝手に補足したり、曖昧なまま文章化したりする可能性がある。

対策:

- 初期実装では segment 化しない
- 巨大 task は明確なエラーまたは warning にする
- 将来的に segment 化する場合は `context_before`, `compose_target`, `context_after` を分けて渡す
- LLM には `compose_target` だけを出力させる
- segment 結合後に qblock 全体で再検証する

### 18.2 文体の揺れ

複数 batch に分けると、batch ごとに文体が揺れる可能性がある。

危険性:

- ある qblock は硬い文体、別の qblock は柔らかい文体になる
- 成功済み task を固定すると、全体最適化が難しくなる
- Local LLM では短すぎる入力により、箇条書きがそのまま残る可能性がある

対策:

- style contract をすべての prompt に含める
- 常体、全角スペース、空欄表記などの機械的 normalizer を強化する
- 最終段で global normalization を行う
- ただし、全体を再度 LLM に投げ直す global polish は標準では行わない

### 18.3 検証階層の複雑化

Adaptive batching により、検証対象が document / batch / task の複数階層になる。

危険性:

- batch 単位では成功しても document 全体では不整合が出る
- task 単位では空欄数が合っていても、全体順序が崩れる
- 余分な id や欠落 id の扱いが曖昧になる

対策:

```text
partial validation
  batch / task 単位の軽い検証

final validation
  GeneratedDocument 全体の厳密検証
```

成功済み task でも、最終 document validation を必ず通す。

### 18.4 再試行ループの暴走

失敗理由を分類せずに再試行すると、改善しない失敗に対して retry が増え続ける可能性がある。

対策:

- retry 回数に上限を設ける
- 失敗理由ごとに再計画方法を変える
- `SingleTask` で失敗したら初期実装では error にする
- segment 化による救済は後続フェーズに回す

### 18.5 再現性とデバッグ

Adaptive batching は、backend、model、policy、retry 結果によって呼び出し単位が変わる。  
そのため、同じ Markdown でも出力や失敗箇所が変わる可能性がある。

対策として、debug log に以下を残す。

```text
task_id
backend
model
batch_id
compose_mode
retry_count
prompt_hash
source_hash
estimated_tokens
```

少なくとも `--verbose` または `FLOWCLOZE_LOG=debug` で確認できるようにする。

## 19. Adaptive Compose Planner の導入フェーズ

### Phase A: task 単位 validation と retry

- 既存の一括生成を維持しつつ、validation error を task 単位で分類できるようにする
- 成功 / 失敗 task を識別する
- まだ batch planner は導入しない

### Phase B: token budget batch

- token / 文字数ベースで batch を作る
- batch ごとに LLM を呼ぶ
- Result Collector で task 結果を集約する

### Phase C: task 単独 retry

- 失敗 task だけを retry queue に戻す
- retry 時は SingleTask mode にする
- retry 上限を超えたら error にする

### Phase D: 巨大 task の扱い

- 単独 task でも大きすぎる場合は warning または error にする
- qblock を小さくするヒントを表示する
- segment 化はまだ行わない

### Phase E: segment 化の検討

- 本当に必要になった場合のみ導入する
- `context_before / compose_target / context_after` を必須にする
- segment 結合後の qblock 全体 validation を強化する
