# FlowCloze

日本語 | [English](README.en.md)

FlowClozeは，Markdownで書いた学習ノートから文章補完問題を生成するCLIツールです．
問題にしたい範囲を `#qblock{ ... }` で囲み，答えにしたい語句を `[答え]` または `[答え]{type}` で指定します．FlowClozeはMarkdownを中間JSONへ変換し，Geminiによる問題生成，検証，PDF/CSV出力までを扱います．

```text
Markdown note
  -> qblock / target extraction
  -> intermediate JSON
  -> Gemini question generation
  -> generated JSON validation
  -> PDF / CSV / TUI
```

## ドキュメント

背景，記法，生成仕様，OpenAPI定義はGitHub Pages向けの `docs/` に整理しています．

- `docs/index.html`: 背景と概要
- `docs/specification.html`: Markdown記法と生成仕様
- `docs/api.html`: OpenAPIドキュメント
- `docs/openapi.yaml`: HTTP API契約

OpenAPIドキュメントは次のコマンドで再生成できます．

```bash
npm install
npm run docs:api
```

定義だけ検証する場合:

```bash
npm run docs:api:lint
```

## セットアップ

必要なもの:

- Rust / Cargo
- Typst CLI（PDF出力を使う場合）
- 日本語フォント（PDF出力で日本語を表示する場合）
- Gemini API key（Geminiで書き換え生成する場合）

Ubuntu / WSLでは，PDFの日本語表示用にNoto CJKフォントを入れてください．

```bash
sudo apt update
sudo apt install -y fonts-noto-cjk
fc-cache -fv
```

Typstから見えているか確認する場合:

```bash
typst fonts | grep "Noto Sans CJK"
```

ビルドだけ行う場合:

```bash
cargo build --release
```

### コマンドとしてインストールする

このリポジトリをcloneしたディレクトリで次を実行すると，`flowcloze` コマンドとして使えるようになります．

```bash
cargo install --path .
```

インストール先は通常 `~/.cargo/bin/flowcloze` です．`~/.cargo/bin` が `PATH` に入っていない場合は，シェル設定に追加してください．

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

確認:

```bash
flowcloze --version
```

releaseビルド済みバイナリへシンボリックリンクを張る方法でも使えます．

```bash
mkdir -p ~/.local/bin
ln -sfn "$PWD/target/release/flowcloze" ~/.local/bin/flowcloze
```

一時的に試すだけなら，インストールせずに `cargo run -- ...` でも実行できます．

## 生成設定

`.env` を使う場合:

```bash
cp .env.example .env
```

`.env` には実際のAPIキーを保存できます。一方、`config.toml`には秘密値を保存せず、`api_key_env`で環境変数名だけを指定します。設定ファイルを使う場合だけ、別途 `cp config.toml.example config.toml` を実行してください。`api set`は非推奨です。全項目と旧名の対応は [`.env.example`](.env.example) を参照してください。

```env
GEMINI_API_KEY=YOUR_GEMINI_API_KEY
FLOWCLOZE_PROVIDER=gemini
```

`generate` は `--provider gemini|local` でプロバイダを選びます。`--backend` は互換用の別名です。`--model`、`--rewrite always|never|auto`、`--fallback error|draft`、`--structured-output auto|on|off`、`--verbose` も指定できます。

```bash
flowcloze generate --provider gemini --model gemini-2.5-flash \
  --rewrite auto --fallback draft --structured-output auto --verbose \
  -o sample/generated.json sample/sample.md
```

`--rewrite never` はIdentity生成を使うためAPIキーやプロバイダ接続を必要としません。`auto` はリスト、複数行、終端記号なし、または短い本文だけを書き換え、その他はIdentity生成します。`--fallback error`（既定値）は失敗をそのまま返します。`--fallback draft` は通信系または内容検証系の失敗時に、失敗したtaskだけをIdentity下書きへ戻します。ただしID・固定フィールド・順序の対応付けが不正な場合は下書きに戻しません。

設定値の優先順位は、CLI、canonical環境変数、（ある場合のみ）legacy環境変数、`config.toml`、既定値です。空の環境変数は未指定として扱います。`FLOWCLOZE_CONFIG` で設定ファイルのパスを変更できます。設定ファイルの例は [`config.toml.example`](config.toml.example) を参照してください。

`--verbose` または `FLOWCLOZE_LOG=debug` はstderrに観測用JSON Linesを出力します。そこにMarkdown本文、prompt、プロバイダ応答、認証情報は含まれません。`max_concurrent_batches` は入力検証と観測に使われますが、現在の実行は逐次です。

互換性のためのCLI保存機能:

```bash
flowcloze api set --key your_api_key_here
```

## 最小例

入力Markdown:

```md
# ソフトウェア工学の概論

#qblock{
[QCD]{term-name}は[品質]{meaning}，[コスト]{meaning}，[納期]{meaning}を表す．
}
```

抽出結果を確認:

```bash
flowcloze sample/sample.md
```

中間JSONを書き出す:

```bash
flowcloze --json -o sample/sample.json sample/sample.md
```

Geminiで問題を生成:

```bash
flowcloze generate -s -o sample/generated.json sample/sample.md
```

LLMに渡すscaffoldを確認:

```bash
flowcloze inspect-scaffold sample/sample.md
```

batch policyを指定して生成:

```bash
flowcloze generate --batch small -s -o sample/generated.json sample/sample.md
```

OllamaまたはLM StudioのOpenAI互換サーバでローカルLLMを使って生成:

標準ローカルモデルを取得し，OllamaまたはLM Studioのローカルサーバを起動してから実行します。URLは `FLOWCLOZE_BASE_URL`、互換用の `LOCAL_LLM_BASE_URL`、`config.toml`、既定候補の順で解決します。未設定時はOllama (`http://localhost:11434/v1`) を先に試し，失敗したらLM Studio (`http://localhost:1234/v1`) を試します。

Ollamaを使う場合:

```bash
ollama pull gemma4:e2b-it-qat
```

LM Studioを使う場合は，LM Studio上で`gemma4:e2b-it-qat`を取得・ロードし，Local Serverを起動します。

```bash
flowcloze local check
```

```bash
flowcloze generate --provider local -s -o sample/generated.json sample/sample.md
```

PDFを作る:

```bash
flowcloze pdf -o sample/sample.pdf sample/generated.json
```

Ankilot向けCSVを書き出す:

```bash
flowcloze csv -o sample/sample.csv sample/generated.json
```

## よく使うコマンド

```bash
flowcloze --help
flowcloze --version
cargo test
npm run docs:api:lint
npm run docs:api
```

## エディタサポート

`editors/vscode-flowcloze-syntax` に，`#qblock` と `[答え]` / `[答え]{type}` を見やすくするVS Code用の簡易拡張があります．

WSL上のVS Codeを使用している場合:

```sh
mkdir -p ~/.vscode-server/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" ~/.vscode-server/extensions/flowcloze.flowcloze-syntax-0.0.1
```

WSL以外のLinux環境の場合:

```sh
mkdir -p ~/.vscode/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" ~/.vscode/extensions/flowcloze.flowcloze-syntax-0.0.1
```

その後，VS Codeで `Developer: Reload Window` を実行してください．

## リポジトリ構成

```text
src/parser.rs      Markdown qblockパーサ
src/json.rs        中間JSON変換
src/prompt.rs      Geminiプロンプト生成
src/gemini.rs      Gemini APIクライアント
src/validation.rs  生成JSONバリデータ
src/csv.rs         Ankilot CSVエクスポータ
src/pdf.rs         Typst PDFアダプタ
docs/              Pages向けドキュメントとOpenAPI定義
templates/         Typstテンプレート
sample/            サンプルノートと出力例
tests/             パーサ / JSON / 検証のテスト
```

## ライセンス

Apache License, Version 2.0 または MIT license のいずれかを選択して利用できます．
