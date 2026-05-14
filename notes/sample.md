# OSメモ

:::qblock{id="sem-001" mode="context" title="セマフォ"}
[セマフォ]{term-name}はOSが提供する[プロセス間同期機能]{meaning}の一つである。
[P命令]{term-name}はリソースの[獲得]{process}を要求し，許可されない場合は[待ち状態]{state}へ移行する。
[V命令]{term-name}はリソースを[解放]{process}する。
:::

:::qblock{id="deadlock-001" mode="context" title="デッドロック"}
[デッドロック]{term-name}は，複数のプロセスが互いの保持する資源を待ち続け，処理が進まない[状態]{state}である。
発生条件には[相互排他]{reason}，[占有と待機]{reason}，[非横取り]{reason}，[循環待ち]{reason}がある。
OSは資源割り当ての制御や検出と回復によって，デッドロックを[予防]{process}または[回復]{process}する。
:::

:::qblock{id="paging-001" mode="context" title="ページング"}
[ページング]{term-name}は，仮想記憶を固定長の[ページ]{meaning}に分割して管理する方式である。
仮想アドレスは[ページ番号]{term-name}と[ページ内オフセット]{term-name}に分けられ，ページテーブルによって物理アドレスへ変換される。
必要なページが主記憶にない場合は[ページフォールト]{state}が発生し，補助記憶からページを読み込む。
:::

:::qblock{id="scheduling-001" mode="context" title="CPUスケジューリング"}
[CPUスケジューリング]{term-name}は，実行可能状態のプロセスから次にCPUを割り当てる対象を選ぶ[処理]{process}である。
[ラウンドロビン]{term-name}では，各プロセスに一定の[タイムクォンタム]{meaning}を与え，時間切れになると待ち行列の末尾へ戻す。
この方式は応答性を高めやすい一方で，タイムクォンタムが短すぎると[コンテキストスイッチ]{demerit}の回数が増える。
:::

:::qblock{id="interrupt-001" mode="context" title="割り込み"}
[割り込み]{term-name}は，実行中の処理を一時停止して，優先度の高い事象に対応する[仕組み]{meaning}である。
入出力装置からの完了通知は[外部割り込み]{term-name}の例であり，ゼロ除算などの例外は[内部割り込み]{term-name}として扱われる。
割り込み発生時，OSは現在の状態を[保存]{process}し，割り込み処理後に元の処理へ[復帰]{process}する。
:::
