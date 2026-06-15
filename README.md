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
- Gemini API key（`generate` コマンドを使う場合）

ビルド:

```bash
cargo build --release
```

ローカルでコマンドとして使う場合:

```bash
mkdir -p ~/.local/bin
ln -sfn "$PWD/target/release/flowcloze" ~/.local/bin/flowcloze
```

一時的に試すだけなら `cargo run -- ...` でも実行できます．

## Gemini API設定

`.env` を使う場合:

```bash
cp .env.example .env
```

`.env` に設定します．

```env
GEMINI_API_KEY=your_api_key_here
GEMINI_MODEL=gemini-2.5-flash
```

CLIから保存する場合:

```bash
flowcloze api set --key your_api_key_here --model gemini-2.5-flash
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
