# FlowCloze

日本語 | [English](README.en.md)

FlowClozeは、Markdownで書いた学習ノートから文章補完問題を生成するRust製CLIツールです。
`#qblock{ ... }` で問題化する範囲を囲み、`[答え]` または `[答え]{type}` で解答対象を指定します。

Gemini、Ollama / LM StudioなどのOpenAI互換ローカルLLM、LLMを呼ばないIdentity生成に対応し、生成結果の検証、TUI表示、PDF / CSV出力までを1つのCLIで扱えます。

![FlowCloze TUI](fig/image.png)

## 主な機能

- Markdownから `qblock` / targetを抽出
- 中間JSONを生成
- GeminiまたはOpenAI互換ローカルLLMで問題文を生成
- `--rewrite never` によるオフラインIdentity生成
- target、answer、空欄、ID、順序などを検証
- 生成結果をTUIで確認
- TypstによるPDF出力
- Ankilot向けCSV出力

```text
Markdown
  -> parse
  -> intermediate JSON
  -> compose / rewrite
  -> validate
  -> JSON
  -> TUI / PDF / CSV
```

## 必要なもの

基本機能:

- Rust / Cargo

必要な機能に応じて:

- Gemini API key: Geminiで書き換え生成する場合
- OllamaまたはLM Studio: ローカルLLMを使う場合
- Typst CLI: PDF出力を使う場合
- 日本語フォント: PDFで日本語を表示する場合

Ubuntu / WSLでPDFを使う場合はNoto CJKフォントを推奨します。

```bash
sudo apt update
sudo apt install -y fonts-noto-cjk
fc-cache -fv
```

Typstから確認する場合:

```bash
typst fonts | grep "Noto Sans CJK"
```

## ビルド / インストール

```bash
git clone https://github.com/sankaku789/FlowCloze.git
cd FlowCloze
cargo build --release
```

`flowcloze` コマンドとしてインストールする場合:

```bash
cargo install --path . --force
flowcloze --version
```

標準Typstテンプレートはバイナリに内包されています。PDF出力を初めて使うと、FlowCloze自身が `~/.config/flowcloze/templates/cloze.typ`（`XDG_CONFIG_HOME` 設定時はその配下）へ自動展開します。そのため、インストール手順はシェルやOS固有のスクリプトに依存しません。

インストール先は通常 `~/.cargo/bin/flowcloze` です。

一時的に試すだけなら、インストールせずに `cargo run -- ...` でも実行できます。

## Markdown記法

```md
# ソフトウェア工学の概論

#qblock{
[QCD]{term-name}は[品質]{meaning}、[コスト]{meaning}、[納期]{meaning}を表す。
}
```

- `#qblock{ ... }`: 問題化する範囲
- `[答え]`: 解答対象
- `[答え]{type}`: 解答対象と任意の出題観点

代表的なtype:

- `term-name`: 用語名
- `meaning`: 意味・定義・性質
- `process`: 手順・工程・動作
- `relation`: 構成・比較・分類・関係

## 基本的な使い方

### Markdownを解析

```bash
flowcloze sample/sample.md
```

中間JSONを書き出す:

```bash
flowcloze --json -o sample/sample.json sample/sample.md
```

### 問題を生成

Geminiを使う場合:

```bash
flowcloze generate --provider gemini \
  -o sample/generated.json sample/sample.md
```

Gemini 3.8 Flashを使う場合:

```bash
flowcloze generate --provider gemini \
  --model gemini-3.8-flash \
  -o sample/generated.json sample/sample.md
```

Gemini 3系では非推奨のsampling parameterを送信しません。

LLMを呼ばずに生成する場合:

```bash
flowcloze generate --rewrite never \
  -o sample/generated.json sample/sample.md
```

ローカルLLMを使う場合:

```bash
flowcloze local check
flowcloze generate --provider local \
  -o sample/generated.json sample/sample.md
```

### 生成結果を検証

```bash
flowcloze validate sample/sample.json sample/generated.json
```

### TUIで確認

```bash
flowcloze view sample/generated.json
```

### PDFを生成

```bash
flowcloze pdf -o sample/sample.pdf sample/generated.json
```

標準テンプレートはバイナリに内包され、PDF出力時に `~/.config/flowcloze/templates/cloze.typ` へ自動展開されます。`typst_template` を設定した場合は、そのカスタムテンプレートを優先します。
一時的に別のTypstテンプレートを使う場合:

```bash
flowcloze pdf --template path/to/template.typ \
  -o sample/sample.pdf sample/generated.json
```

### CSVを生成

```bash
flowcloze csv -o sample/sample.csv sample/generated.json
```

## API送信前にbatch計画を確認

`plan` はProvider APIへ接続せず、`generate` が使うものと同じplannerでqblockのまとめ方を表示します。

```bash
flowcloze plan sample/sample.md
```

各batchに含まれるqblock番号、推定input/output、空欄数、重いqblockの単独処理を確認できます。`rewrite=auto` でAPIへ送らないqblockは `identity (no API)` として別表示します。

## 生成設定

FlowCloze 2.2では、設定をユーザー単位の標準ディレクトリへ集約します。

```text
~/.config/flowcloze/config.toml
~/.config/flowcloze/credentials.toml
```

`config.toml` が存在しない場合は、FlowClozeが内蔵の標準設定を初回利用時に自動生成します。`flowcloze api set` だけを先に実行した場合も同時に生成されます。既存の `config.toml` は上書きしません。

`XDG_CONFIG_HOME` が設定されている場合は、`$XDG_CONFIG_HOME/flowcloze/` を使います。開発時は例えば次のように分離できます。

```bash
export XDG_CONFIG_HOME="$PWD/.dev-config"
```

標準TypstテンプレートはPDF出力時に自動展開されるため、テンプレート配置のための追加インストール操作は不要です。

APIキーは `config.toml` ではなく、専用の `credentials.toml` に保存します。`flowcloze api set` を実行するとproviderを対話式に選択し、APIキーは非表示入力できます。

```bash
flowcloze api set
```

Unix系OSでは `credentials.toml` を `0600`、設定ディレクトリを `0700` で作成します。

`config.toml` の例:

```toml
provider = "gemini"
model = "gemini-2.5-flash"
rewrite = "always"
fallback = "error"
structured_output = "auto"
batch = "auto"
max_tasks_per_batch = 12
max_input_tokens = 18000
max_output_tokens = 6000
max_blanks_per_batch = 24
# typst_template = "/path/to/custom.typ"
```

`typst_template` は標準テンプレートを差し替えたい場合だけ指定します。未指定時は内蔵テンプレートを自動展開します。`flowcloze pdf --template ...` を指定した場合はCLI指定を優先します。

`batch = "auto"` では、qblock番号ではなく各qblockの推定入力token・推定出力token・blank数を独立したbudgetとして評価します。軽いqblockは同じrequestへ再packingし、いずれかのbudgetを大きく消費するqblockは単独requestにします。最終出力は元のqblock順へ戻します。batch全体の出力が壊れた場合は次回batchを縮小し、qblock固有の検証失敗は成功済みqblockを保持したまま失敗分だけ再試行します。

主な `generate` オプション:

```text
--provider gemini|local
--model <model>
--rewrite always|never|auto
--fallback error|draft
--structured-output auto|on|off
--batch auto|small|one-task
--verbose
-s, --skip-constraints
```

例:

```bash
flowcloze generate \
  --provider gemini \
  --model gemini-2.5-flash \
  --rewrite auto \
  --fallback draft \
  --structured-output auto \
  --verbose \
  -o sample/generated.json \
  sample/sample.md
```

`rewrite`:

- `always`: providerで書き換える
- `never`: providerを呼ばずIdentity生成する
- `auto`: 入力内容に応じて書き換えの要否を選ぶ

`fallback`:

- `error`: 失敗をそのままエラーにする
- `draft`: 通信または内容検証に失敗したtaskをIdentity下書きへ戻す

設定値は **CLI > `~/.config/flowcloze/config.toml` > 組み込み既定値** の順に解決されます。
`.env`、カレントディレクトリの `config.toml`、旧設定用環境変数は自動では読みません。

## ローカルLLM

OllamaまたはLM StudioのOpenAI互換サーバを利用できます。

既定ではOllama (`http://localhost:11434/v1`) を先に試し、接続できない場合はLM Studio (`http://localhost:1234/v1`) を試します。
`FLOWCLOZE_BASE_URL` で接続先を明示できます。

既定のローカルモデルは `gemma4:e2b-it-qat` です。

Ollamaの場合:

```bash
ollama pull gemma4:e2b-it-qat
flowcloze local check
```

LM Studioの場合は、同じモデルをロードしてLocal Serverを起動したあと `flowcloze local check` を実行してください。

## Scaffold確認

LLMへ渡すscaffold JSONを確認できます。

```bash
flowcloze inspect-scaffold sample/sample.md
```

ファイルへ保存する場合:

```bash
flowcloze inspect-scaffold \
  -o sample/scaffold.json sample/sample.md
```

## ログ / 観測

`generate` は解析、batch、検証、保存の進捗をstderrへ表示します。

`--verbose` または `FLOWCLOZE_LOG=debug` を指定すると、観測用JSON Linesもstderrへ出力します。Markdown本文、prompt、provider応答、認証情報はログに含めません。

## 開発

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Gemini native adapterも確認する場合:

```bash
cargo clippy --all-targets --features gemini-native -- -D warnings
cargo test --features gemini-native
```

## エディタサポート

`editors/vscode-flowcloze-syntax` に、`#qblock` と `[答え]` / `[答え]{type}` を見やすくするVS Code用の簡易拡張があります。

WSL上のVS Code:

```bash
mkdir -p ~/.vscode-server/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" \
  ~/.vscode-server/extensions/flowcloze.flowcloze-syntax-0.0.1
```

WSL以外のLinux:

```bash
mkdir -p ~/.vscode/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" \
  ~/.vscode/extensions/flowcloze.flowcloze-syntax-0.0.1
```

その後、VS Codeで `Developer: Reload Window` を実行してください。

## リポジトリ構成

```text
src/parser.rs          Markdown parser
src/json.rs            intermediate JSON
src/planner.rs         generation planning
src/compose.rs         question composition core
src/orchestration.rs   generation orchestration
src/config.rs          configuration resolution
src/gemini.rs          Gemini adapter
src/local_openai.rs    local OpenAI-compatible adapter
src/validation.rs      generated JSON validation
src/observability.rs   structured events / logging
src/csv.rs             Ankilot CSV export
src/pdf.rs             Typst PDF adapter
src/main.rs            CLI entry point
templates/             Typst templates
sample/                sample inputs / outputs
editors/               editor support
tests/                 integration tests
```

## ライセンス

Apache License, Version 2.0 または MIT License のいずれかを選択して利用できます。
