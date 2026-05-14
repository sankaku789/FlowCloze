# FlowCloze

FlowClozeは，Markdownで書いた学習ノートから文章補完問題を生成するローカルCLIツールです。

ノート本文はそのまま読み物として保ち，問題にしたい範囲だけを `#qblock{ ... }` で囲みます。答えにしたい語句は `[答え]{type}` として明示します。FlowClozeはその指定を中間JSONへ変換し，Geminiによる問題文生成，生成結果の検証，TypstによるPDF化までを扱います。

```text
Markdown note
  -> qblock / target extraction
  -> intermediate JSON
  -> Gemini question generation
  -> generated JSON validation
  -> Typst PDF
```

## Features

- Markdown内の `#qblock{ ... }` を問題化範囲として抽出
- `[答え]{type}` で指定した語句だけを解答対象にする
- `# 見出し1` を単元名として扱い，生成JSONとPDFに反映
- qblock IDは出現順に `qblock-001` 形式で自動採番
- Gemini APIで文章補完問題JSONを生成
- 中間JSONと生成JSONを照合し，余分な答えや空欄数のずれを検出
- Typstで「解答ページ -> 問題ページ」の順にA4横PDFを出力
- VS Code用の簡易シンタックスハイライト拡張を同梱

## Setup

必要なもの:

- Rust / Cargo
- Typst CLI
- Gemini API key（`generate` を使う場合）

ビルドとテスト:

```bash
cargo build
cargo test
```

Geminiを使う場合は `.env` を用意します。

```bash
cp .env.example .env
```

```env
GEMINI_API_KEY=your_api_key_here
GEMINI_MODEL=gemini-2.5-flash
```

`GEMINI_MODEL` は省略できます。未指定時は `gemini-2.5-flash` を使います。

## Markdown Format

### qblock

問題化したい範囲を `#qblock{ ... }` で囲みます。

```md
# ソフトウェア工学の概論

#qblock{
- [QCD]{term-name}は[品質]{meaning}，[コスト]{meaning}，[納期]{meaning}
}
```

qblock IDは書きません。出現順に `qblock-001`，`qblock-002` のようなIDが自動で付きます。

```md
#qblock{
- [情報システム]{term-name}は，人，機械，コンピュータが協調して目的を達成する仕組みである。
}
```

### targets

答えにしたい語句は `[答え]{type}` で書きます。

```md
[要求定義]{term-name}は，[要求獲得]{process}，[要求分析]{process}，[要求仕様化]{process}，[検証]{process}からなる。
```

`[]` の中が解答文字列，`{}` の中が出題観点です。Geminiには，ここで指定したtargets以外を答えにしないよう指示します。

### sections

PDF上の単元見出しとして使うのはMarkdownの見出し1だけです。

```md
# 要求定義
```

`##` や `###` はノート内の構造として残せますが，PDFの単元見出しには使いません。

### target types

現在，警告なしで使えるtypeは以下です。typeは「その語句をどの観点で問いたいか」を示すラベルです。

| type | 説明 |
|---|---|
| `term-name` | 用語名そのものを問う |
| `meaning` | 意味，定義，性質，目的などを問う |
| `process` | 手順，工程，動作，状態変化などを問う |
| `relation` | 構成，比較，分類，関係，対応などを問う |

未定義typeも抽出されますが，中間JSONの `warnings` に警告が付きます。

## CLI

### Parse Markdown

抽出されたqblock IDとtargetsをテキストで確認します。

```bash
cargo run -- notes/sample.md
```

### Write Intermediate JSON

Markdownから中間JSONを生成します。

```bash
cargo run -- --json -o generated/sample.questions.json notes/sample.md
```

`-o` を省略すると標準出力へ出します。

```bash
cargo run -- --json notes/sample.md
```

`-o` を指定した通常parseは，自動的にJSON出力として扱われます。

```bash
cargo run -- -o generated/sample.questions.json notes/sample.md
```

### Generate Questions

Geminiで文章補完問題を生成します。生成後，FlowClozeは中間JSONと照合し，検証に通ったJSONだけを保存します。

```bash
cargo run -- generate -o generated/sample.gemini.json notes/sample.md
```

モデルを明示する場合:

```bash
cargo run -- generate --model gemini-2.5-flash -o generated/sample.gemini.json notes/sample.md
```

### Validate Generated JSON

中間JSONと生成JSONを手動で検証します。

```bash
cargo run -- validate generated/sample.questions.json generated/sample.gemini.json
```

成功時は `validation ok` を出力します。失敗時は検証エラーを表示して終了コード `1` で終了します。

### Build PDF

生成JSONからPDFを作ります。デフォルトでは `templates/cloze.typ` を使い，入力JSONと同じ場所に `.pdf` を出力します。

```bash
cargo run -- pdf generated/sample.gemini.json
```

出力先やテンプレートを指定できます。

```bash
cargo run -- pdf -o generated/sample.pdf --template templates/cloze.typ generated/sample.gemini.json
```

PDFは各ページを「解答」「問題」の順に出力します。解答ページには答えを赤字で表示し，問題ページでは同じ位置を空欄にします。

## JSON Shapes

中間JSONは，Markdownから抽出した事実だけを保持します。

```json
{
  "meta": {
    "source": "notes/sample.md"
  },
  "qblocks": [
    {
      "id": "qblock-001",
      "section": "要求定義",
      "source_text": "要求定義は，「顧客が欲しいモノ」から要求仕様書をまとめる工程である。",
      "targets": [
        { "answer": "要求定義", "type": "term-name" },
        { "answer": "要求仕様書", "type": "relation" }
      ]
    }
  ]
}
```

生成JSONは，Typstテンプレートと検証器が読む形式です。

```json
{
  "questions": [
    {
      "id": "qblock-001",
      "section": "要求定義",
      "type": "context-cloze",
      "targets": [
        { "answer": "要求定義", "type": "term-name" },
        { "answer": "要求仕様書", "type": "relation" }
      ],
      "question": "＿＿＿は，顧客が欲しいモノから＿＿＿をまとめる工程である。",
      "answers": ["要求定義", "要求仕様書"],
      "source_text": "要求定義は，「顧客が欲しいモノ」から要求仕様書をまとめる工程である。",
      "explanation": "",
      "tags": [],
      "warnings": []
    }
  ]
}
```

## Editor Support

`editors/vscode-flowcloze-syntax` に，`#qblock` と `[答え]{type}` を見やすくするVS Code用の簡易拡張があります。

### Install Locally

WSL上のVS Codeを使用している場合は、VS Code Serverの拡張ディレクトリにシンボリックリンクを作成します。

```sh
mkdir -p ~/.vscode-server/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" ~/.vscode-server/extensions/flowcloze.flowcloze-syntax-0.0.1
```

その後、VS Codeで `Developer: Reload Window` を実行し、`notes/sample.md` などのMarkdownファイルを開いてください。

WSL以外のLinux環境の場合は、代わりに `~/.vscode/extensions` を使用します。

```sh
mkdir -p ~/.vscode/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" ~/.vscode/extensions/flowcloze.flowcloze-syntax-0.0.1
```

## Repository Layout

```text
src/parser.rs      Markdown qblock parser
src/json.rs        intermediate JSON conversion
src/prompt.rs      Gemini prompt builder
src/gemini.rs      Gemini API client
src/validation.rs  generated JSON validator
src/pdf.rs         Typst PDF adapter
templates/         Typst templates
notes/             sample notes
generated/         sample outputs
tests/             parser / JSON / validation tests
```
