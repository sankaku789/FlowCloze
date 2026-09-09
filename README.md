# FlowCloze

日本語 | [English](README.en.md)

FlowClozeは、Markdownで書いた学習ノートから文章補完問題を生成するRust製CLIツールです。
`#qblock{ ... }`で問題化する範囲を囲み、`[答え]`または`[答え]{type}`で解答対象を指定します。

Google Gemini、OllamaなどのOpenAI互換API、LLMを呼ばないIdentity生成に対応し、生成結果の検証、TUI表示、PDF / CSV出力までを1つのCLIで扱えます。

![FlowCloze TUI](fig/image.png)

## 主な機能

- Markdownから`qblock` / targetを抽出
- 中間JSONを生成
- OpenAI互換APIで問題文を生成
- `--offline`によるオフラインIdentity生成
- target、answer、空欄、ID、順序などを検証
- 生成結果をTUIで確認
- TypstによるPDF出力
- Ankilot向けCSV出力

```text
Markdown
  -> parse
  -> intermediate JSON
  -> compose
  -> validate
  -> JSON
  -> TUI / PDF / CSV
```

## 必要なもの

基本機能:

- Rust / Cargo

必要な機能に応じて:

- Google API key: Geminiで生成する場合
- Ollama: ローカルLLMを使う場合
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

`flowcloze`コマンドとしてインストールする場合:

```bash
cargo install --path . --force
flowcloze --version
```

標準Typstテンプレートはバイナリに内包されています。PDF出力を初めて使うと、FlowCloze自身が`~/.config/flowcloze/templates/cloze.typ`（`XDG_CONFIG_HOME`設定時はその配下）へ自動展開します。そのため、インストール手順はシェルやOS固有のスクリプトに依存しません。

インストール先は通常`~/.cargo/bin/flowcloze`です。

一時的に試すだけなら、インストールせずに`cargo run -- ...`でも実行できます。

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

組み込みの`gemini-flash` model profileを使う場合:

```bash
flowcloze auth add google
flowcloze generate --model gemini-flash \
  -o sample/generated.json sample/sample.md
```

`gemini-flash`はGoogleの`gemini-2.5-flash`へ接続します。Googleを含むすべてのproviderはOpenAI互換adapterを通して呼び出されます。

LLMを呼ばずに生成する場合:

```bash
flowcloze generate --offline \
  -o sample/generated.json sample/sample.md
```

Ollamaを使う場合:

```bash
flowcloze model add local-qwen --provider ollama --model qwen3:14b
flowcloze provider check ollama
flowcloze generate --model local-qwen \
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

標準テンプレートはバイナリに内包され、PDF出力時に`~/.config/flowcloze/templates/cloze.typ`へ自動展開されます。`typst_template`を設定した場合は、そのカスタムテンプレートを優先します。

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

`plan`はProvider APIへ接続せず、`generate`が使うものと同じplannerでqblockのまとめ方を表示します。

```bash
flowcloze plan --model gemini-flash sample/sample.md
```

各batchに含まれるqblock番号、推定input/output、空欄数、重いqblockの単独処理を確認できます。

すべてのqblockをAPIへ送らない計画は、次のように確認します。

```bash
flowcloze plan --offline sample/sample.md
```

この場合、すべてのqblockが`identity (no API)`として表示され、providerは初期化されません。

## 生成設定

FlowClozeは設定をユーザー単位の標準ディレクトリへ集約します。

```text
~/.config/flowcloze/
  config.yaml
  model.yaml
  auth.yaml
  templates/cloze.typ
```

`XDG_CONFIG_HOME`が設定されている場合は、`$XDG_CONFIG_HOME/flowcloze/`を使います。開発時は例えば次のように分離できます。

```bash
export XDG_CONFIG_HOME="$PWD/.dev-config"
```

ファイルの役割:

- `config.yaml`: 既定model、生成、batch、quota、Typst templateの設定
- `model.yaml`: provider catalogとmodel profile
- `auth.yaml`: API keyのみ

GoogleのAPI keyは次のコマンドで非表示入力します。

```bash
flowcloze auth add google
```

API keyは`auth.yaml`だけへ保存されます。Unix系OSでは設定ディレクトリを`0700`、`auth.yaml`を`0600`で扱います。Ollamaには認証情報が不要です。

`config.yaml`の例:

```yaml
default_model: gemini-flash
generation:
  fallback: draft
batch:
  mode: auto
  max_retries: 2
quotas:
  gemini-flash:
    rpm: 5
    tpm: 250000
# typst_template: /path/to/custom.typ
```

`typst_template`は標準テンプレートを差し替えたい場合だけ指定します。未指定時は内蔵テンプレートを自動展開します。`flowcloze pdf --template ...`を指定した場合はCLI指定を優先します。

`batch.mode: auto`では、qblock番号ではなく各qblockの推定入力token・推定出力token・blank数を独立したbudgetとして評価します。軽いqblockは同じrequestへ再packingし、いずれかのbudgetを大きく消費するqblockは単独requestにします。最終出力は元のqblock順へ戻します。batch全体の出力が壊れた場合は次回batchを縮小し、qblock固有の検証失敗は成功済みqblockを保持したまま失敗分だけ再試行します。

主な`generate`オプション:

```text
--model <profile>
--fallback error|draft
--batch auto|small|one-task
--offline
--verbose
-s, --skip-constraints
```

例:

```bash
flowcloze generate \
  --model gemini-flash \
  --fallback draft \
  --batch auto \
  --verbose \
  -o sample/generated.json \
  sample/sample.md
```

`fallback`:

- `error`: 失敗をそのままエラーにする
- `draft`: 通信または内容検証に失敗したtaskをIdentity下書きへ戻す

`--offline`はproviderを呼ばず、すべてのtaskをIdentity生成します。

設定値は**CLI override > `~/.config/flowcloze/config.yaml` > 組み込み既定値**の順に解決されます。未知fieldは設定エラーになります。

`.env`、カレントディレクトリの設定ファイル、旧設定用環境変数は自動では読みません。

## ProviderとModel

組み込み定義:

- provider `google`: `https://generativelanguage.googleapis.com/v1beta/openai`、API keyが必要
- provider `ollama`: `http://localhost:11434/v1`、認証不要
- model profile `gemini-flash`: provider `google`、model `gemini-2.5-flash`

利用可能なmodel profileを確認する:

```bash
flowcloze model list
```

Ollamaのmodel profileを追加または上書きする:

```bash
flowcloze model add local-qwen --provider ollama --model qwen3:14b
```

`model add`は`model.yaml`のmodel profileだけを変更します。Provider URLやAPI keyはmodelへ保存しません。

providerの到達性を確認する:

```bash
flowcloze provider check google
flowcloze provider check ollama
```

Ollamaの場合:

```bash
ollama pull qwen3:14b
flowcloze provider check ollama
```

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

`generate`は解析、batch、検証、保存の進捗をstderrへ表示します。

`--verbose`または`FLOWCLOZE_LOG=debug`を指定すると、観測用JSON Linesもstderrへ出力します。Markdown本文、prompt、provider応答、認証情報はログに含めません。

## 開発

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

GoogleとOllamaは同じOpenAI互換adapterを使うため、Gemini専用featureの追加検証は不要です。

## エディタサポート

`editors/vscode-flowcloze-syntax`に、`#qblock`と`[答え]` / `[答え]{type}`を見やすくするVS Code用の簡易拡張があります。

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

その後、VS Codeで`Developer: Reload Window`を実行してください。

## リポジトリ構成

```text
src/application/   use case orchestration
src/cli/           CLI parsing and commands
src/config/        YAML configuration and authentication
src/core/          parser, model, and validation core
src/generation/    planning and question generation
src/output/        JSON, TUI, CSV, and PDF output
src/providers/     provider catalog and OpenAI-compatible adapter
src/runtime/       progress and observability
templates/         Typst templates
sample/            sample inputs / outputs
editors/           editor support
tests/             integration tests
```

## ライセンス

Apache License, Version 2.0またはMIT Licenseのいずれかを選択して利用できます。
