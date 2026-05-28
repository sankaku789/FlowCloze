# FlowCloze

日本語 | [English](README.en.md)

FlowClozeは，Markdownで書いた学習ノートから文章補完問題を生成するCLIツールです．

ノート本文はそのまま読み物として残し，問題にしたい範囲だけを `#qblock{ ... }` で囲みます．答えにしたい語句は `[答え]` として明示します．FlowClozeはその指定を中間JSONへ変換し，Geminiには断片的な文を1つの文章補完問題本文へ整える作業だけを任せます．解答・target・section・source_textなどの固定情報はRust側で組み立て，検証した上でPDF化します．

```text
Markdown note
  -> qblock / target extraction
  -> intermediate JSON
  -> Gemini edits question text only
  -> generated JSON normalization
  -> validation
  -> Typst PDF
```

## 背景

試験などの勉強をするとき，ノートにまとめたり暗記シートを作成したりすると思います．私もこれまで，主に次の2つの方法で勉強していました．

1. Markdown形式のノート
2. 手作りの暗記シート（EXCEL-PDF）

1つ目は，資料を読みながら内容を自分の言葉でまとめていく方法です．後から見返しやすい一方で，内容を抽象的に覚えてしまい，具体的な用語や定義を問われたときに対応しづらいことがありました．

2つ目は，資料を読みながら要点を整理し，覚えたい語句を空欄にした**文章補完問題**を自分で作る方法です．作成した問題はEXCELに入力して暗記シートの形に整え，PDFとして出力したあと，ノートアプリに取り込んで使っていました．1問1答ではなく文章補完問題にしていたのは，前後の文脈から語句の定義や意味を思い出せるため，単語だけを切り出して覚えるよりも印象に残りやすかったからです．

ただし，この方法では問題文を考えるだけでなく，EXCELへ転記し，PDFとして使える形に整える作業も必要になります．その結果，実際に暗記を始める前の準備段階でかなりのリソースを使ってしまっていました．

そこで，Markdownノートの書きやすさと，文章補完問題の覚えやすさを両立できないかと考えました．そのために作ったのがFlowClozeです．

## システム構成
FlowClozeは，Markdownノートから問題範囲と解答対象を抽出し，中間JSONを作ります．Geminiは中間JSON内の「元の文章」と「穴埋め下書き」を見て，`question` 本文だけを自然な文章補完問題へ編集します．その後，FlowClozeが中間JSONをもとに生成JSON全体を正規化し，空欄数と解答順を検証します．生成した問題はJSONとして保存できるほか，TypstによるPDF出力や，Ankilotへ取り込むためのCSV出力にも対応しています．

システム構成は次の通りです．

```mermaid
flowchart LR
    note[Markdown note<br/>#qblock / targets] --> parse[Parser<br/>qblock / target / section]
    parse --> intermediate[Intermediate JSON<br/>fixed structure]
    intermediate --> prompt[Prompt<br/>source + cloze draft]
    prompt --> gemini[Gemini API<br/>question text only]
    gemini --> normalize[Normalizer<br/>fill fixed fields]
    intermediate --> normalize
    normalize --> validate[Validator]
    validate --> json[Generated JSON]
    json --> pdf[PDF<br/>via Typst]
    json --> csv[Ankilot CSV]
    json --> tui[TUI viewer]
```

## 主な機能

主な機能は次の通りです．

- Markdown内の `#qblock{ ... }` を問題化範囲として抽出
- `[答え]` または `[答え]{type}` で指定した語句だけを解答対象にする
- 直近のMarkdown見出し，またはqblock内の先頭 `# 見出し1` を単元名として扱い，生成JSONとPDFに反映
- qblock IDは出現順に `qblock-001` 形式で自動採番
- Gemini APIで断片的なノート文を1つの文章補完問題本文へ編集
- 中間JSONから `section` / `targets` / `answers` / `source_text` を決定的に組み立て
- 中間JSONと生成JSONを照合し，空欄数・解答順のずれを検出
- 出力前に，TUI上で生成された問題を確認可能
- Typstで「解答ページ -> 問題ページ」の順にA4横PDFを出力
- AnkilotにインポートできるようCSV出力に対応
- VS Code用の簡易シンタックスハイライト拡張を同梱

## セットアップ

### 必要なもの

- Rust / Cargo
- Typst CLI（PDF出力を使う場合）
- Gemini API key（`generate` コマンドを使う場合）

### ビルドとインストール

```bash
cargo build --release
mkdir -p ~/.local/bin
ln -sfn "$PWD/target/release/flowcloze" ~/.local/bin/flowcloze
```

`~/.local/bin` が `PATH` に入っていない場合は，シェル設定に追加してください．

動作確認には次のコマンドを使います．

```bash
flowcloze --version
cargo test
```

ビルドだけ確認したい場合は，通常のdebugビルドも使えます．

```bash
cargo build
```

このREADMEでは，以降のコマンド例はreleaseビルド後にシンボリックリンクを作成し，`flowcloze` コマンドとして実行できる前提で書いています．ローカルで一時的に試すだけなら，`flowcloze ...` の代わりに `cargo run -- ...` でも実行できます．

```bash
flowcloze sample/sample.md
```

## Markdown形式

### qblock指定

問題化したい範囲を `#qblock{ ... }` で囲みます．

```md
# ソフトウェア工学の概論

#qblock{
- [QCD]{term-name}は[品質]{meaning}，[コスト]{meaning}，[納期]{meaning}
}
```

qblock IDは書きません．出現順に `qblock-001`，`qblock-002` のようなIDが自動で付きます．

```md
#qblock{
- [情報システム]{term-name}は，人，機械，コンピュータが協調して目的を達成する仕組みである．
}
```

### target指定

答えにしたい語句は `[答え]` で書きます．必要な場合は `[答え]{type}` として出題観点を明示できます．

```md
[要求定義]は，[要求獲得]，[要求分析]，[要求仕様化]，[検証]からなる．
```

`[]` の中が解答文字列です．`{}` を付けた場合は出題観点として扱います．typeを省略した場合は `term-name` として扱います．targetとanswersはRust側で中間JSONから生成JSONへコピーされるため，Geminiは解答対象を推測しません．

### 単元見出し

PDF上の単元見出しには，qblock直前のMarkdown見出し，またはqblock内の先頭 `# 見出し1` を使います．qblock直前の見出しは `#`，`##`，`###` などのレベルを問いません．

```md
# 要求定義
```

qblock内の `##` や `###` は，単元見出しではなく通常の本文として扱います．それだけで段落分割はしません．

### target type一覧

typeは任意です．指定する場合，現在，警告なしで使えるtypeは以下です．typeは「その語句をどの観点で問いたいか」を示すラベルです．

| type | 説明 |
|---|---|
| `term-name` | 用語名そのものを問う |
| `meaning` | 意味，定義，性質，目的などを問う |
| `process` | 手順，工程，動作，状態変化などを問う |
| `relation` | 構成，比較，分類，関係，対応などを問う |

未定義typeも抽出されますが，中間JSONの `warnings` に警告が付きます．

## CLIの使い方

### API設定

`generate`コマンドを使用するためには，Gemini APIキーを `.env` に保存します．モデル指定は省略できます．

```bash
flowcloze api set --key your_api_key_here
```

モデルを更新する場合:

```bash
flowcloze api set --key your_api_key_here --model gemini-2.5-flash
```


### Markdownを解析する

抽出されたqblock IDとtargetsをテキストで確認します．

```bash
flowcloze sample/sample.md
```

### 中間JSONを書き出す

Markdownから中間JSONを生成します．

```bash
flowcloze --json -o sample/sample.json sample/sample.md
```

`-o` を省略すると標準出力へ出します．

```bash
flowcloze --json sample/sample.md
```

`-o` を指定した通常parseは，自動的にJSON出力として扱われます．

```bash
flowcloze -o sample/sample.json sample/sample.md
```

### 問題を生成する

Geminiで `question` 本文を生成します．Geminiが返すのは各qblockの `id` と `question` だけです．その後，FlowClozeが中間JSONから `section` / `type` / `targets` / `answers` / `source_text` を補完し，生成JSONとして正規化します．検証に失敗した場合は，検証エラーをGeminiに渡して最大3回まで再生成します．

```bash
flowcloze generate -o sample/sample.json sample/sample.md
```

`generate` 実行時に追加制約を入力できます．空行で終了します．

追加制約の入力をスキップする場合:

```bash
flowcloze generate -s -o sample/sample.json sample/sample.md
```

モデルを明示する場合:

```bash
flowcloze generate --model gemini-2.5-flash -o sample/sample.json sample/sample.md
```

### 生成JSONを検証する

中間JSONと生成JSONを手動で検証します．中間JSONと生成JSONは別ファイルとして保存しておくと確認しやすくなります．

```bash
flowcloze --json -o sample/intermediate.json sample/sample.md
flowcloze validate sample/intermediate.json sample/sample.json
```

成功時は `validation ok` を出力します．失敗時は検証エラーを表示して終了コード `1` で終了します．

### 生成JSONを表示する

生成JSONをTUIで確認することができます．

```bash
flowcloze view sample/sample.json
```

![tui example](fig/image.png)

### Ankilot CSVを書き出す

生成JSONからAnkilot取り込み用CSVを作ります．CSVはUTF-8のヘッダーなし2列形式です．

1. 表: question
2. 裏: answers

```bash
flowcloze csv -o sample/sample.csv sample/sample.json
```

`-o` を省略すると標準出力へ出します．


### PDFを作成する

生成JSONからPDFを作ります．デフォルトでは `templates/cloze.typ` を使い，入力JSONと同じ場所に `.pdf` を出力します．

```bash
flowcloze pdf sample/sample.json
```

出力先やテンプレートを指定できます．

```bash
flowcloze pdf -o sample/sample.pdf --template templates/cloze.typ sample/sample.json
```

PDFは各ページを「解答」「問題」の順に出力します．解答ページには答えを赤字で表示し，問題ページでは同じ位置を空欄にします．

### ヘルプとバージョン

ヘルプとバージョンを表示します．

```bash
flowcloze --help
flowcloze --version
```



## JSON形式

中間JSONは，Markdownから抽出したqblockを，Geminiへ渡す生成タスクとして保持します．
空欄位置，解答順，単元見出しはRust側で確定します．
`cloze_template` は「元の文章」と「穴埋め下書き」を含みます．Geminiはこれを読んで，バラバラの文を1つの文章補完問題本文へ編集します．

```json
{
  "schema_version": 3,
  "meta": {
    "source": "sample/sample.md",
    "format": {
      "blank": "＿＿＿",
      "block_separator": "\n\n",
      "paragraph_indent": "　"
    }
  },
  "tasks": [
    {
      "id": "qblock-001",
      "type": "context-cloze",
      "section": "要求定義",
      "source": {
        "raw": "[要求定義]{term-name}は，「顧客が欲しいモノ」から[要求仕様書]{relation}をまとめる工程である．",
        "plain": "要求定義は，「顧客が欲しいモノ」から要求仕様書をまとめる工程である．"
      },
      "blocks": [
        {
          "id": "qblock-001-b001",
          "kind": "paragraph",
          "starts_new_paragraph": false,
          "text": "要求定義は，「顧客が欲しいモノ」から要求仕様書をまとめる工程である．",
          "cloze_text": "　＿＿＿は，「顧客が欲しいモノ」から＿＿＿をまとめる工程である．",
          "target_refs": [0, 1]
        }
      ],
      "cloze_template": "元の文章:\n要求定義は，「顧客が欲しいモノ」から要求仕様書をまとめる工程である．\n\n穴埋め下書き:\n　＿＿＿は，「顧客が欲しいモノ」から＿＿＿をまとめる工程である．",
      "targets": [
        { "index": 0, "answer": "要求定義", "type": "term-name", "block_id": "qblock-001-b001" },
        { "index": 1, "answer": "要求仕様書", "type": "relation", "block_id": "qblock-001-b001" }
      ],
      "answers": ["要求定義", "要求仕様書"]
    }
  ]
}
```

Geminiからの生出力は，概念的には次の最小JSONです．

```json
{
  "questions": [
    {
      "id": "qblock-001",
      "question": "＿＿＿は，顧客が欲しいモノから＿＿＿をまとめる工程である．"
    }
  ]
}
```

FlowClozeはこの出力を中間JSONと照合し，最終的な生成JSONへ正規化します．生成JSONは，Typstテンプレートと検証器が読む形式です．

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
      "question": "＿＿＿は，顧客が欲しいモノから＿＿＿をまとめる工程である．",
      "answers": ["要求定義", "要求仕様書"],
      "source_text": "要求定義は，「顧客が欲しいモノ」から要求仕様書をまとめる工程である．",
      "explanation": "",
      "tags": [],
      "warnings": []
    }
  ]
}
```

## 責務分担

現在の生成フローでは，GeminiにJSON全体の正しさを任せません．各部分の責務は次の通りです．

- `parser.rs`: Markdownから `#qblock`，target，section，target位置を抽出する
- `json.rs`: 中間JSONを作る．`blocks`，`cloze_text`，`cloze_template`，`targets`，`answers` を確定する
- `prompt.rs`: Geminiへ「question本文だけを編集する」ための短い指示を作る
- `gemini.rs`: Gemini APIを呼び，`id` と `question` の最小JSONを受け取る
- `main.rs`: Gemini出力を中間JSONで正規化し，`section` / `type` / `targets` / `answers` / `source_text` を補完する
- `validation.rs`: 空欄数，解答順，target対応を検証する
- `templates/cloze.typ`: 生成JSONからPDFを組版する

## エディタサポート

`editors/vscode-flowcloze-syntax` に，`#qblock` と `[答え]` / `[答え]{type}` を見やすくするVS Code用の簡易拡張があります．

### ローカルにインストールする

WSL上のVS Codeを使用している場合は，VS Code Serverの拡張ディレクトリにシンボリックリンクを作成します．

```sh
mkdir -p ~/.vscode-server/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" ~/.vscode-server/extensions/flowcloze.flowcloze-syntax-0.0.1
```

その後，VS Codeで `Developer: Reload Window` を実行し，`sample/sample.md` などのMarkdownファイルを開いてください．

WSL以外のLinux環境の場合は，代わりに `~/.vscode/extensions` を使用します．

```sh
mkdir -p ~/.vscode/extensions
ln -sfn "$PWD/editors/vscode-flowcloze-syntax" ~/.vscode/extensions/flowcloze.flowcloze-syntax-0.0.1
```

## リポジトリ構成

```text
src/parser.rs      Markdown qblockパーサ
src/json.rs        中間JSON変換
src/prompt.rs      Geminiプロンプト生成
src/gemini.rs      Gemini APIクライアント
src/validation.rs  生成JSONバリデータ
src/csv.rs         Ankilot CSVエクスポータ
src/pdf.rs         Typst PDFアダプタ
templates/         Typstテンプレート
sample/            サンプルノートと出力例
tests/             パーサ / JSON / 検証のテスト
```

## 開発について

このプログラムの開発には，バイブコーディングを利用しています．そのため，利用中にバグや重大な問題を見つけた場合は，Issueに内容を書いてください．修正できる場合は，ブランチを切って変更を入れ，Pull Requestを送ってください．ご協力よろしくお願いします．

## ライセンス

Apache License, Version 2.0 または MIT license のいずれかを選択して利用できます．
