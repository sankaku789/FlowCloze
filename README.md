# Markdown文章補完問題生成システム

## 概要

このシステムは，通常のMarkdownノートから，暗記用の文章補完問題を生成するための仕組みである。

なお，本システムはMCPサーバではなく，Markdownを入力としてRust製CLIで解析し，必要に応じてLLM APIへ渡すローカルツールである。

目的は，以下の2つの学習方法の長所を統合することである。

- 2024式：Excelで文章補完問題を作成し，それを解いて暗記する方法
- 2025式：Markdownで内容をまとめ，それを見返して覚える方法

2024式は，問題演習としては強いが，暗記に特化しすぎており，後から見返すと文脈が分かりにくい。  
一方で2025式は，ノートとして読みやすいが，問題演習が不足し，知識が抽象的になりやすい。

そこで，本システムでは，

> 読めるMarkdownノートを原本にし，そこから暗記用の文章補完問題を派生生成する

ことを目指す。

---

## 基本思想

本システムでは，Markdownを単なるノートではなく，

> 読めるノート + 問題生成のための設計図

として扱う。

ただし，Markdown自体を問題文そのものにはしない。  
Markdownはあくまで，学習内容・出題範囲・答えさせたい語句を指定するための原本である。

文章補完問題そのものは，LLMによって再構成・生成する。

---

## 役割分担

本システムにおける各要素の役割は以下である。

```text
Markdown = 読めるノート + 出題意図の指定
YAML     = 生成された問題データ
LLM      = 制約付きの文章補完問題生成エンジン
Parser   = Markdownからqblockや出題対象を抽出する処理
Typst    = 表示・印刷用レイアウト
```

重要なのは，LLMにすべてを任せないことである。

LLMには文章を生成させるが，答えさせる語句や出題範囲は人間が指定する。

```text
人間が決めること:
  - どの範囲を問題化するか
  - どの語句を答えにするか
  - それぞれの語句をどの観点で問うか

LLMが行うこと:
  - 指定された語句を使って文章補完問題を生成する
  - 元ノートをそのまま穴埋めにせず，表現を変えて再構成する
  - 学習しやすい自然な文章にする
  - 必要に応じて解説やタグを生成する

LLMに任せないこと:
  - 重要語を勝手に選ぶ
  - 指定されていない語句を答えにする
  - 答えを勝手に変更する
  - 元ノートにない新しい知識を追加する
```

---

## LLMの位置づけ

本システムでは，LLMを単なる補助編集者ではなく，

> 制約付き生成エンジン

として扱う。

つまり，LLMには問題文を積極的に生成させる。  
ただし，その生成は完全自由ではなく，人間が指定した出題対象と制約に従わせる。

この方針を取る理由は，ノートの文章をそのまま穴埋めにすると，ノートの品質に強く依存してしまうためである。

また，暗記においては，同じ内容を少し違う表現で問うことも重要である。  
そのため，LLMには元ノートの内容を保持しつつ，表現を変えた文章補完問題を生成させる。

---

## Markdown記法

問題化したい語句は，Markdown内で次のように記述する。

```md
[答え]{タイプ}
```

例：

```md
[セマフォ]{term-name}はOSが提供する[プロセス間同期機能]{meaning}の一つである。
```

この場合，以下の情報を表す。

```text
答え: セマフォ
タイプ: term-name

答え: プロセス間同期機能
タイプ: meaning
```

`[ ]` の中が答え，`{ }` の中が問い方を表す。

---

## 問題タイプ

初期段階では，以下のタイプを使用する。

| タイプ | 意味 |
|---|---|
| `term-name` | 用語名を問う |
| `meaning` | 意味・定義を問う |
| `process` | 処理・動作を問う |
| `state` | 状態を問う |
| `reason` | 理由を問う |
| `merit` | 利点を問う |
| `demerit` | 欠点を問う |
| `compare` | 比較を問う |

例：

```md
[P命令]{term-name}はリソースの[獲得]{process}を要求し，許可されない場合は[待ち状態]{state}へ移行する。
```

この記述により，単語そのものを問うのか，意味を問うのか，処理を問うのかを明示できる。

---

## qblock

本システムでは，単なる1語穴埋めではなく，ある程度まとまった文脈を補完する問題を重視する。

そのため，複数の出題対象をまとめて1つの文章補完問題にするために，`qblock` を使用する。

```md
:::qblock{id="semaphore-basic" mode="context" title="セマフォの基本"}
[セマフォ]{term-name}は，OSが提供する[プロセス間同期機能]{meaning}の一つである。
リソース数を管理する[カウンタ]{meaning}として使われる。
[P命令]{term-name}はリソースの[獲得]{process}を要求し，許可されない場合は[待ち状態]{state}へ移行する。
[V命令]{term-name}はリソースを[解放]{process}する。
:::
```

この `qblock` 内は，1語ずつ独立した問題にするのではなく，1つの大きな文章補完問題として扱う。

---

## qblockの目的

`qblock` の目的は，単語を点で覚えるのではなく，流れや構造として復元できるようにすることである。

単なる短文穴埋めは，一問一答に近くなりやすい。

```text
P命令はリソースの＿＿＿を要求する。
```

この形式でも暗記はできるが，知識が孤立しやすい。

一方で，qblockでは以下のように，まとまった文脈の中で複数の語句を補完させる。

```text
＿＿＿は，複数のプロセスが共有資源を扱う際に用いられるOSの＿＿＿である。
この仕組みでは，＿＿＿によってリソースの＿＿＿を要求し，
利用できない場合にはプロセスを＿＿＿に移す。
処理が終わった後は，＿＿＿によってリソースを＿＿＿する。
```

この形式により，以下をまとめて確認できる。

- 用語名
- 定義
- 処理の流れ
- 状態遷移
- 仕組み全体の関係

---

## LLMによる生成方針

LLMには，Markdown本文をそのまま穴埋めにさせるのではなく，qblockの内容をもとに文章補完問題を再構成させる。

### 入力例

```md
:::qblock{id="semaphore-basic" mode="context" title="セマフォの基本"}
- [セマフォ]{term-name}
- OSが提供する[プロセス間同期機能]{meaning}
- [P命令]{term-name}: リソースの[獲得]{process}
- 許可されない場合は[待ち状態]{state}
- [V命令]{term-name}: リソースを[解放]{process}
:::
```

### 生成例

```yaml
questions:
  - id: semaphore-basic
    type: context-cloze
    title: セマフォの基本

    question: |
      ＿＿＿は，複数のプロセスが共有資源を扱う際に用いられる
      OSの＿＿＿である。
      この仕組みでは，＿＿＿によってリソースの＿＿＿を要求し，
      利用できない場合にはプロセスを＿＿＿へ移行させる。
      処理が終わった後は，＿＿＿によってリソースを＿＿＿する。

    answers:
      - セマフォ
      - プロセス間同期機能
      - P命令
      - 獲得
      - 待ち状態
      - V命令
      - 解放
```

このように，LLMは元ノートを素材として使いながら，文章補完問題として自然な形に再構成する。

---

## LLMへの制約

LLMに与える制約は以下である。

### 許可すること

- 表現の言い換え
- 文の順序の整理
- 箇条書きから説明文への変換
- 学習しやすい文章への再構成
- 接続詞や補助的な表現の追加
- 解説の生成
- タグや難易度の提案

### 禁止すること

- 指定された答えを変更する
- 指定されていない語句を答えにする
- 元ノートにない新事実を追加する
- 空欄数と解答数をずらす
- 答えの意味を変える
- 出題タイプを勝手に変更する

---

## LLMプロンプト方針

LLMには，以下のような指示を与える。

```text
次のMarkdown qblockから，文章補完問題を生成してください。

制約:
- [答え]{type} で指定された語句のみを答えにする
- answerの内容は [] 内の文字列をそのまま使う
- typeは {} 内の文字列をそのまま使う
- answersの順序は，学習上自然な順序にしてよいが，答えは増減させない
- 空欄数とanswers数を必ず一致させる
- 元ノートの内容をそのまま穴埋めにせず，表現を少し変えて文章補完問題として再構成する
- 元ノートにない知識を追加しない
- 不明な点や不自然な点があればwarningsに書く

出力はYAML形式にしてください。
```

---

## YAMLを採用する理由

本システムでは，生成データの保存形式としてYAMLを採用する。

理由は，qblockを多用し，大きめの文章補完問題を中心に扱うためである。

YAMLは以下に向いている。

- 長文の問題文
- 複数行の文章
- 複数の解答
- 解説
- タグ
- メタ情報
- 将来的な復習ログ

特に，複数行文字列を自然に書ける点が大きい。

```yaml
question: |
  ＿＿＿はOSが提供する＿＿＿の一つである。
  リソース数を管理する＿＿＿として使われる。
```

JSONより人間が読みやすく，TOMLより長文や階層構造を扱いやすい。

---

## YAML形式

生成されるYAMLの基本構造は以下である。

```yaml
meta:
  source: notes/os-2nd.md
  title: 並行プロセス
  generated_at: 2026-05-13

questions:
  - id: semaphore-basic
    type: context-cloze
    title: セマフォの基本

    targets:
      - answer: セマフォ
        type: term-name
      - answer: プロセス間同期機能
        type: meaning
      - answer: P命令
        type: term-name
      - answer: 獲得
        type: process
      - answer: 待ち状態
        type: state
      - answer: V命令
        type: term-name
      - answer: 解放
        type: process

    question: |
      ＿＿＿は，複数のプロセスが共有資源を扱う際に用いられる
      OSの＿＿＿である。
      この仕組みでは，＿＿＿によってリソースの＿＿＿を要求し，
      利用できない場合にはプロセスを＿＿＿へ移行させる。
      処理が終わった後は，＿＿＿によってリソースを＿＿＿する。

    answers:
      - セマフォ
      - プロセス間同期機能
      - P命令
      - 獲得
      - 待ち状態
      - V命令
      - 解放

    source_text: |
      セマフォはOSが提供するプロセス間同期機能の一つである。
      P命令はリソースの獲得を要求し，許可されない場合は待ち状態へ移行する。
      V命令はリソースを解放する。

    explanation: |
      セマフォは，複数のプロセスが共有資源を扱うときに同期を取るための仕組みである。
      P命令でリソースの獲得を要求し，V命令でリソースを解放する。

    tags:
      - OS
      - 同期
      - セマフォ

    warnings: []
```

---

## source_textを残す理由

YAMLには，生成された問題だけでなく，元になった文章も `source_text` として残す。

理由は以下である。

- 生成ミスを確認できる
- LLMによる再生成に使える
- 問題の根拠を確認できる
- 後からノートと問題の対応を追える
- 将来的に「元文を見る」機能を作れる

---

## targetsを残す理由

`targets` には，Markdown内で指定された答えとタイプを保存する。

これにより，LLMが生成した問題文と，人間が指定した出題対象を分離できる。

```yaml
targets:
  - answer: セマフォ
    type: term-name
  - answer: プロセス間同期機能
    type: meaning
```

`targets` は，人間の出題意図を保存する領域である。  
一方，`question` はLLMが生成した文章補完問題である。

この分離により，

```text
出題意図 = 人間が管理
問題文 = LLMが生成
```

という設計を保てる。

---

## 生成モード

本システムでは，少なくとも以下の2つの生成モードを用意する。

### inline mode

`[答え]{タイプ}` ごとに，短い問題を生成する。

```md
[並行]{term-name}は1つのCPUで同時に複数のプロセスをすすめることである。
```

生成例：

```text
＿＿＿は1つのCPUで同時に複数のプロセスをすすめることである。
```

### block mode

`:::qblock ... :::` の範囲を，1つの大きな文章補完問題として生成する。

文章補完問題を重視する本システムでは，block modeを主役とする。

---

## 推奨される使い方

### 1. Markdownでノートを書く

通常のノートとして読めるようにMarkdownを書く。

```md
## セマフォ

セマフォは，OSが提供するプロセス間同期機能の一つである。
P命令とV命令を使って，リソースの獲得と解放を制御する。
```

### 2. 問題化したい範囲をqblockで囲む

```md
:::qblock{id="semaphore-basic" mode="context" title="セマフォの基本"}
[セマフォ]{term-name}は，OSが提供する[プロセス間同期機能]{meaning}の一つである。
[P命令]{term-name}はリソースの[獲得]{process}を要求し，許可されない場合は[待ち状態]{state}へ移行する。
[V命令]{term-name}はリソースを[解放]{process}する。
:::
```

### 3. Parserで出題対象を抽出する

Parserは以下を抽出する。

- qblockのid
- qblockのtitle
- qblockのsource_text
- `[答え]{type}` で指定されたtargets

### 4. LLMに文章補完問題を生成させる

LLMは，targetsを必ず使って文章補完問題を生成する。  
このとき，元ノートをそのまま穴埋めにするのではなく，表現を少し変えて再構成する。

### 5. YAMLとして保存する

生成結果を `.questions.yaml` として保存する。

### 6. 必要に応じて人間が確認・修正する

LLMが生成した問題文を確認し，必要であれば編集する。

---

## 現在の実装状況

現在の実装はRust版で進めている。

実装済みの範囲は以下である。

| Phase | 状態 | 内容 |
|---|---|---|
| Phase 1 | 実装済み | `[答え]{タイプ}` と `qblock` の仕様をRustのモデルに反映 |
| Phase 2 | 実装済み | Markdownからqblock，source_text，targetsを抽出 |
| Phase 3 | 実装済み | 抽出した中間データをYAMLとして出力 |
| Phase 4 | 実装済み | Gemini APIで文章補完問題YAMLを生成 |
| Phase 5 | 実装済み | 生成YAMLを中間データと照合して検証 |
| Phase 6 | 実装済み | Typstテンプレートで生成YAMLを読み込み，PDFとして出力 |
| Phase 7 | 未実装 | 復習ログ，苦手分野，復習間隔管理 |

現時点では，Markdownを入力として，Geminiに問題を生成させ，生成結果を検証してYAMLとして保存し，TypstでPDF化できる。

---

## 現在のディレクトリ構成

```text
FlowCloze/
├─ notes/
│  └─ sample.md
├─ generated/
│  ├─ sample.questions.yaml
│  ├─ sample.generated.yaml
│  └─ sample.gemini.yaml
├─ src/
│  ├─ lib.rs
│  ├─ main.rs
│  ├─ models.rs
│  ├─ parser.rs
│  ├─ yaml.rs
│  ├─ prompt.rs
│  ├─ gemini.rs
│  ├─ validation.rs
│  └─ pdf.rs
├─ templates/
│  └─ cloze.typ
├─ tests/
│  ├─ fixtures/
│  ├─ parser.rs
│  ├─ yaml.rs
│  ├─ prompt.rs
│  ├─ gemini.rs
│  └─ validation.rs
├─ Cargo.toml
├─ Cargo.lock
├─ .env.example
└─ README
```

各Rustモジュールの役割は以下である。

| ファイル | 役割 |
|---|---|
| `src/models.rs` | qblockとtargetのドメインモデル |
| `src/parser.rs` | Markdownからqblockとtargetsを抽出 |
| `src/yaml.rs` | 中間データをYAMLへ変換 |
| `src/prompt.rs` | Geminiに渡す生成プロンプトを構築 |
| `src/gemini.rs` | Gemini APIのRESTクライアント |
| `src/validation.rs` | 生成結果YAMLの検証 |
| `src/pdf.rs` | Typstを呼び出すPDF出力アダプタ |
| `src/main.rs` | CLI |
| `templates/cloze.typ` | 生成YAMLを直接読み込むTypstレイアウト |

---

## 環境構築

### 1. Rustをインストールする

Rustが入っていない場合は，以下のどちらかでインストールする。

#### rustupを使う場合

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

#### Ubuntu / WSLでaptを使う場合

```bash
sudo apt update
sudo apt install -y cargo rustc
```

確認する。

```bash
cargo --version
rustc --version
```

### 2. リポジトリをビルドする

```bash
cargo build
```

### 3. テストを実行する

```bash
cargo test
```

現在のテストは以下を確認している。

- qblockの抽出
- targetsの抽出
- source_textの生成
- コードフェンス内qblockの無視
- 不正なqblockのエラー
- 中間YAML出力
- Gemini用プロンプト生成
- Geminiレスポンス整形
- 生成YAMLの検証

### 4. Gemini APIキーを設定する

`.env.example` を参考に `.env` を作る。

```bash
cp .env.example .env
```

`.env` にAPIキーを設定する。

```env
GEMINI_API_KEY=your_api_key_here
GEMINI_MODEL=gemini-2.5-flash
```

`.env` は秘密情報を含むためGit管理しない。

---

## CLIの使い方

### Markdownを解析してtargetsを表示する

```bash
cargo run -- notes/sample.md
```

出力例：

```text
sem-001: セマフォ
  - セマフォ (term-name)
  - プロセス間同期機能 (meaning)
  - P命令 (term-name)
  - 獲得 (process)
  - 待ち状態 (state)
  - V命令 (term-name)
  - 解放 (process)
```

### 中間YAMLを標準出力する

```bash
cargo run -- --yaml notes/sample.md
```

### 中間YAMLをファイルに保存する

```bash
cargo run -- --yaml -o generated/sample.questions.yaml notes/sample.md
```

生成される中間YAMLは，LLMに渡すためのデータである。

```yaml
meta:
  source: notes/sample.md
qblocks:
- id: sem-001
  mode: context
  title: セマフォ
  source_text: |-
    セマフォはOSが提供するプロセス間同期機能の一つである。
    P命令はリソースの獲得を要求し，許可されない場合は待ち状態へ移行する。
    V命令はリソースを解放する。
  targets:
  - answer: セマフォ
    type: term-name
```

### Geminiで問題を生成する

```bash
cargo run -- generate notes/sample.md
```

ファイルに保存する場合：

```bash
cargo run -- generate -o generated/sample.gemini.yaml notes/sample.md
```

モデルを明示する場合：

```bash
cargo run -- generate --model gemini-2.5-flash -o generated/sample.gemini.yaml notes/sample.md
```

`generate` は内部で以下を行う。

```text
Markdown
  ↓
Parser
  ↓
中間データ
  ↓
Gemini API
  ↓
生成YAML
  ↓
Phase 5検証
  ↓
保存または標準出力
```

生成結果が検証に失敗した場合，ファイルには保存しない。

### 生成YAMLを検証する

```bash
cargo run -- validate generated/sample.questions.yaml generated/sample.gemini.yaml
```

成功時：

```text
validation ok
```

### 生成YAMLをPDFにする

```bash
cargo run -- pdf generated/sample.gemini.yaml -o generated/sample.pdf
```

`pdf` は内部で `typst compile` を呼び出す。  
レイアウトは `templates/cloze.typ` に分離しており，Rust側は生成YAMLのパスをTypstへ渡すだけである。

出力先を省略した場合は，入力YAMLの拡張子を `.pdf` に変えたパスへ保存する。

```bash
cargo run -- pdf generated/sample.gemini.yaml
```

別のTypstテンプレートを使う場合：

```bash
cargo run -- pdf --template templates/cloze.typ generated/sample.gemini.yaml -o generated/sample.pdf
```

現在の `templates/cloze.typ` は，生成YAMLをTypst側の `yaml(...)` で読み込み，解答入り版と演習用の空欄版を同じ紙面に並べる。

---

## YAML形式

### 中間YAML

中間YAMLは，Parserが抽出した人間の出題意図を保存する。

```yaml
meta:
  source: notes/sample.md
qblocks:
- id: sem-001
  mode: context
  title: セマフォ
  source_text: |-
    セマフォはOSが提供するプロセス間同期機能の一つである。
  targets:
  - answer: セマフォ
    type: term-name
```

### 生成YAML

生成YAMLは，LLMが生成した文章補完問題である。

```yaml
questions:
  - id: sem-001
    type: context-cloze
    title: セマフォ
    targets:
      - answer: セマフォ
        type: term-name
    question: |
      ＿＿＿は，OSが提供する同期機能の一つである。
    answers:
      - セマフォ
    source_text: |
      セマフォはOSが提供するプロセス間同期機能の一つである。
    explanation: |
      セマフォは共有資源を扱う際の同期機構である。
    tags:
      - OS
    warnings: []
```

---

## 検証ルール

LLM出力後，プログラム側で必ず検証を行う。

現在の実装では以下を確認する。

```text
- 生成YAMLとして読めるか
- 中間YAMLとして読めるか
- questionが空でないか
- question内の空欄数 == answers数
- answersの各要素が元のtargetsに含まれているか
- 元のtargetsのanswerがanswersに欠けていないか
- question idが重複していないか
- question idが中間データに存在するか
```

空欄は `＿＿＿` という文字列で数える。

LLMは便利だが，出力の正しさを完全には信用しない。  
そのため，生成後のバリデーションを必須とする。

---

## 開発フェーズ

### Phase 1：Markdown記法の確定

実装済み。  
`[答え]{タイプ}` と `qblock` の仕様を `models.rs` と `parser.rs` に反映している。

### Phase 2：Parser実装

実装済み。  
Markdownからqblock，targets，source_textを抽出する。

### Phase 3：YAML出力

実装済み。  
抽出した中間データをYAMLとして保存できる。

### Phase 4：LLM生成

実装済み。  
Gemini APIを使い，targetsを固定した文章補完問題を生成する。

### Phase 5：検証処理

実装済み。  
LLM出力について，空欄数，answers，targets，id重複などを検証する。

### Phase 6：出力機能

実装済み。  
生成YAMLをTypstテンプレート側で読み込み，問題欄と解答欄を持つPDFとして出力する。

この層はコア処理から分離する。

```text
core:
  Markdown解析
  中間YAML生成
  LLM生成
  生成YAML検証

render:
  templates/cloze.typ
  src/pdf.rs
```

Rustの `src/pdf.rs` は，TypstテンプレートへYAMLパスを渡して `typst compile` を呼び出すだけである。  
紙面レイアウトや罫線，解答表示の有無は `templates/cloze.typ` 側で管理する。

Web UIなど，Typst以外の出力機能は未実装である。

### Phase 7：復習支援

未実装。  
将来的に以下を追加する。

- 間違えた問題の記録
- 苦手分野の抽出
- 復習間隔の管理
- 難易度調整

---

## 設計上の重要ポイント

### Markdownは唯一の原本

Markdownを知識の原本とする。  
YAMLは生成物であり，必要に応じて再生成できるものとする。

### qblockを主役にする

本システムは文章補完問題を重視するため，qblockを多用する前提で設計する。

### LLMは生成エンジンとして使う

LLMには，単なる整形ではなく，文章補完問題の生成を任せる。  
ただし，答え集合は人間が指定する。

### 答えさせる語句は人間が決める

LLMに重要語選定を任せると，自分が重要だと考える部分とズレる可能性がある。  
そのため，答えさせる語句はMarkdown内で明示する。

### 表現の言い換えを許可する

元ノートそのままの穴埋めではなく，あえて表現を変えた問題を生成する。  
これにより，単なる丸暗記ではなく，内容の再構成を促す。

---

## 最終目標

本システムの最終目標は，以下である。

> 読めるMarkdownノートを維持しながら，そこからLLMによって文章補完問題を生成する。

これにより，

- 2024式の「問題演習による具体的な暗記」
- 2025式の「読み返しやすいノート」
- LLMによる「問題作成負担の軽減」
- 表現の言い換えによる「理解を伴う記憶」

を同時に実現する。
