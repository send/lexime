# Lexime 仕様書 (v2.0)

## 概要

Lexime は macOS 向けの軽量・高速な日本語予測入力システム。コンセプトは**思考の速度で日本語を書ける開発者向け IME** (Speed of Thought)。

## アーキテクチャ

```
┌──────────────────────────────────────────────────┐
│  macOS (InputMethodKit)                          │
│  ┌────────────────────────────────────────────┐  │
│  │  Swift: IME Frontend                       │  │
│  │  - AppContext (リソース初期化)              │  │
│  │  - LeximeInputController (イベント駆動)    │  │
│  │  - MarkedTextManager (インライン表示)       │  │
│  │  - CandidateManager (候補状態管理)          │  │
│  │  - CandidatePanel (候補ウィンドウ)          │  │
│  └─────────────┬──────────────────────────────┘  │
│                │ UniFFI (自動生成バインディング)   │
│  ┌─────────────▼──────────────────────────────┐  │
│  │  Rust: 変換エンジン (lex_engine)            │  │
│  │  ┌──────────────────────────────────────┐  │  │
│  │  │  api/ (UniFFI エクスポート層)         │  │  │
│  │  │  async_worker (候補非同期)            │  │  │
│  │  ├──────────────────────────────────────┤  │  │
│  │  │  lex-session (セッション状態機械)     │  │  │
│  │  ├──────────────────────────────────────┤  │  │
│  │  │  lex-core (計算エンジン)              │  │  │
│  │  │  romaji / candidates / converter /   │  │  │
│  │  │  dict / user_history / user_dict /   │  │  │
│  │  │  neural (feature-gated) / settings    │  │  │
│  │  └──────────────────────────────────────┘  │  │
│  └────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────┘
```

## コンポーネント詳細

### Swift Frontend

| ファイル | 役割 |
|---|---|
| `main.swift` | エントリポイント。AppContext 初期化、IMKServer 起動 |
| `AppContext.swift` | シングルトン: 辞書・接続行列・学習データ・ユーザー辞書の読み込み、LexEngine 管理 |
| `LeximeInputController.swift` | IMKInputController サブクラス。LexSession 保持、ポールタイマー管理、イベント実行、設定メニュー |
| `MarkedTextManager.swift` | インライン表示（未確定文字列、点線下線） |
| `CandidateManager.swift` | 候補リスト状態管理（surfaces, selectedIndex, generation counter） |
| `CandidatePanel.swift` | 候補ウィンドウ（NSPanel、ページネーション、VoiceOver） |
| `SettingsWindowController.swift` | 設定ウィンドウ管理（singleton、NSHostingView、activation policy 切替） |
| `SettingsView.swift` | SwiftUI 設定ルートビュー（TabView、developerMode 分岐、TOML エディタ） |
| `UserDictionaryView.swift` | ユーザ辞書 CRUD（List + Add/Remove、LexEngine FFI 呼び出し） |

Swift は純粋なイベント実行レイヤー。Rust から返る `LexEvent` の列を `applyEvents` ループで逐次適用する。

### Rust Engine（ワークスペース構成）

依存グラフ: `lex-engine → lex-session → lex-core`、`lex-cli → lex-core`

#### lex_engine (engine/src/) — UniFFI ラッパー

| モジュール | 内容 |
|---|---|
| `api/` | UniFFI エクスポート関数・型定義（engine, session, resources, types, user_dict） |
| `async_worker.rs` | 候補の非同期ワーカースレッド（mpsc, AtomicU64 staleness） |
| `lib.rs` | `pub use lex_core::*; pub use lex_session as session;` + `uniffi::setup_scaffolding!()` |

#### lex-core (engine/crates/lex-core/) — 計算エンジン

| モジュール | 内容 |
|---|---|
| `romaji/` | ローマ字→かな変換。Trie + TOML 設定対応（`default_romaji.toml`, 306 エントリ） |
| `candidates/` | 統一候補生成（Standard / Predictive）。Neural は feature-gated で research 用 |
| `converter/` | Lattice 構築、Viterbi N-best、Reranker、Rewriter、CostFunction trait |
| `dict/` | `Dictionary` trait、`TrieDictionary`、`CompositeDictionary`、`ConnectionMatrix` |
| `user_history/` | ユニグラム・バイグラム学習、WAL、LXUD 形式 |
| `user_dict/` | ユーザー辞書、LXUW 形式 |
| `neural/` | GPT-2 (Zenzai) ニューラルスコアリング（feature gate: `--features neural`） |
| `settings.rs` | 設定管理（`default_settings.toml`, OnceLock パターン） |
| `unicode.rs` | Unicode ユーティリティ（ひらがな・カタカナ判定、変換） |
| `numeric.rs` | 日本語数詞→数字変換（にじゅうさん → 23） |

#### lex-session (engine/crates/lex-session/) — セッション状態機械

| モジュール | 内容 |
|---|---|
| `key_handlers.rs` | キー入力処理（idle / composing 分岐） |
| `composing.rs` | 入力中状態管理（Composition の操作） |
| `commit.rs` | 確定操作 |
| `auto_commit.rs` | 自動確定ロジック（安定度トラッカー、ASCII グルーピング） |
| `candidate_gen.rs` | 候補生成ディスパッチ |
| `response.rs` | レスポンスビルダー（free functions） |
| `types/` | セッション型定義（SessionConfig）、Composition |

#### lex-cli (engine/crates/lex-cli/) — CLI ツール

| バイナリ | 内容 |
|---|---|
| `dictool` | 辞書操作 CLI（fetch / compile / compile-conn / merge / diff / info / user-dict / romaji-export / romaji-validate / settings-export / settings-validate / neural-score (`--features neural`)） |
| `lextool` | 変換テスト CLI |

### 辞書データ

Mozc 辞書のみを使用。ファイル名は `lexime.dict` / `lexime.conn`。

- **辞書**: Mozc TSV → `TrieDictionary`（bincode シリアライズ、マジック `LXDC`、約 49MB）
- **接続行列**: バイナリ行列（マジック `LXCX`、i16 配列）。V3 フォーマットでは POS ロールメタデータ（`ContentWord` / `FunctionWord` / `Suffix` / `Prefix`）を埋め込み、文節グルーピングに使用
- POS ID ペアの遷移コストを O(1) で参照

### UniFFI バインディング

UniFFI proc-macro で Swift バインディングを自動生成。`generated/lex_engine.swift` + `lex_engineFFI.modulemap`。

**エクスポート型**:

| 型 | 種類 | 説明 |
|---|---|---|
| `LexEngine` | Object | 変換エンジン本体。セッション生成、ユーザー辞書操作 |
| `LexSession` | Object | 入力セッション。handle_key / commit / poll |
| `LexDictionary` | Object | 辞書リソース（open / open_with_user_dict） |
| `LexConnection` | Object | 接続行列 |
| `LexUserHistory` | Object | 学習履歴（WAL 付き、`clear()` で全消去） |
| `LexUserDictionary` | Object | ユーザー辞書 |
| `LexKeyResponse` | Record | キー入力レスポンス（consumed + events） |
| `LexEvent` | Enum | イベント（下記参照） |
| `LexDictEntry` | Record | 辞書エントリ |
| `LexUserWord` | Record | ユーザー辞書ワード |

**LexEvent enum**:

| バリアント | 説明 |
|---|---|
| `Commit { text }` | テキスト確定 |
| `SetMarkedText { text }` | マークドテキスト設定（空文字列でクリア） |
| `ShowCandidates { surfaces, selected }` | 候補パネル表示 |
| `HideCandidates` | 候補パネル非表示 |
| `SwitchToAbc` | システム ABC 入力ソースに切替 |

**トップレベル関数**:

| 関数 | 説明 |
|---|---|
| `engine_version()` | バージョン文字列 |
| `romaji_lookup(romaji)` | ローマ字 Trie 照合（None / Prefix / Exact / ExactAndPrefix） |
| `romaji_convert(kana, pending, force)` | ローマ字→かな変換 |
| `romaji_load_config(path)` | カスタムローマ字設定読み込み |
| `romaji_default_config()` | 埋め込みデフォルトローマ字 TOML 取得 |
| `settings_load_config(path)` | カスタム設定読み込み |
| `settings_default_config()` | 埋め込みデフォルト設定 TOML 取得 |
| `keymap_get(key_code, has_shift)` | キーリマップ照合（`Option<String>`） |
| `trace_init(log_dir)` | 構造化ログ初期化 |

**LexSession メソッド**:

| メソッド | 説明 |
|---|---|
| `handle_key(event)` | キー入力処理（`LexKeyEvent`）→ `LexKeyResponse` |
| `commit()` | 現在の入力を確定（選択候補に解決 + 学習記録）→ `LexKeyResponse` |
| `settle_unconfirmed(displayed)` | 非自発的な composition の終了（フォーカス喪失等）。**呼び出し側が渡した表示中テキストを確定し、学習しない** → `LexKeyResponse`（§不変条件） |
| `is_composing()` | 入力中かどうか |
| `set_defer_candidates(enabled)` | 非同期候補生成の有効化 |
| `set_conversion_mode(mode)` | 変換モード切替（LexConversionMode enum） |
| `set_abc_passthrough(enabled)` | ABC パススルー設定 |
| `shutdown()` | AsyncWorker スレッドを即時停止 |

**LexSessionEvents** (foreign callback interface):

| メソッド | 説明 |
|---|---|
| `on_async_response(response)` | 非同期候補結果を `LexKeyResponse` として受信（Worker スレッドから呼ばれる） |

## 入力モデル

### 状態遷移

```
idle ──(ローマ字入力/句読点)──→ composing ──(Enter/Tab=確定)──→ idle
  │                               │
  └──────(トリガーキー)──────────┬─┘  ※ composing からは先に確定する
                                 ↓
                              snippet ──(Enter/Space=展開, Escape=取消)──→ idle
```

`is_composing()` は composing と snippet の**和**を返す（どちらもインライン表示を持つ状態）。

Escape は composing に**留まる**（IMKit が後続で `commitComposition` を呼ぶ確定経路のため）。
snippet 状態の Escape だけはピッカーを畳んで idle に戻る。この 2 つの違いは
proptest の invariant 3 / 3b で固定している。

### 各状態でのキー操作

**idle**

| キー | 動作 |
|---|---|
| ローマ字 | composing へ遷移 |
| Shift+英字 | 大文字のまま composing へ遷移（ローマ字変換しない） |
| 句読点（`,` `.` 等） | 全角句読点で composing へ遷移 |
| Tab | パススルー（消費しない） |
| 英数キー | ABC パススルーモードに入る |

**composing**

| キー | 動作 |
|---|---|
| ローマ字 | かな追加、候補更新（ひらがなをインライン表示） |
| Shift+英字 | 大文字のまま composedKana に追加（auto-commit 抑制、連続英字は一塊） |
| z + `h/j/k/l/.,/-/[/]` | Mozc 互換 z-sequence（矢印・記号）を入力 |
| Space / ↓ | 次の候補を選択（初回 Space は index 1 から開始） |
| ↑ | 前の候補を選択 |
| Enter | 表示中の候補を確定（変換結果 + 学習記録） |
| Tab | 確定 |
| Backspace | 1 文字削除（空になれば idle へ） |
| Fn+Delete | 選択中の候補の学習履歴を削除し、候補リストから除外 |
| Escape | ひらがなで確定（IMKit が commitComposition を呼ぶため） |
| 句読点 | 現在の変換を確定し、句読点を直接挿入 |
| その他の文字 | composedKana に追加（Backspace で削除可能） |

**snippet**

トリガーキー（`settings.toml` `[snippets] trigger`、デフォルト `ctrl+shift+/`）で入る。composing 中なら先に確定してから入る。トリガーは ABC パススルーの判定より**先に**ディスパッチされるので、英数モード中でもピッカーが開く。定義は `~/Library/Application Support/Lexime/snippets.toml`（`key = "body"` のフラット形式、Swift が I/O と TOML パースを担当し Rust が変数参照を検証する）。

提示できるスニペットが 1 件も無いときは snippet 状態に入らず、トリガーキーは**消費せずアプリに渡す**（IME にバインドが無いのと同じ扱い）。composing 中なら先に確定してから渡す（`ModifiedKey` と同じ形）。該当するのは: ファイル無し / エントリ 0 件 / 全エントリの body が空 / 起動時に設定エラーで読み込めなかった（空 key・重複 key・未定義変数）。

**再読み込みの失敗は挙動が違う**: 一度読めた後に `snippets.toml` が壊れた場合、`ConfigStore.reloadSnippets()` は代入前に throw するので **直前に読めた store がそのまま残り**、ピッカーは従来どおり開く。タイプミス 1 つで動いていた機能を止めないための意図的な非対称であり（起動時は「保持すべき正しい store」が存在しない、という違い）、エラーは呼び出し元に伝播する。

| キー | 動作 |
|---|---|
| 文字 | フィルタに追加（ローマ字変換しない）。前方一致で候補を絞る |
| Backspace | フィルタを 1 文字削除（空フィルタなら取消して idle へ） |
| Enter / Space | 選択中のスニペットを展開して確定（変数展開後の body を挿入） |
| ↓ / ↑ | 候補選択（巡回） |
| Escape | 取消して idle へ |
| 英数キー / かなキー | 取消してモード切替を適用 |
| その他 | 取消してキーをパススルー |

### 不変条件（marked text と session の同期）

`fix/snippet-enter-leak` (#293) 由来。Chromium/Electron は marked text の有無で `KeyboardEvent.isComposing` を決めるため、**session が composing のまま host の marked text が消えると、確定キーが IME に消費されると同時に web ページにも届く**（チャットアプリなら送信されてしまう）。したがって:

- **composing を続ける response は、host の marked text を残さなければならない**。host が marked text を失う経路は 3 つあり、いずれも session 全体の不変条件（呼び出し箇所ごとの約束ではない）なので、`InputSession::debug_assert_response_contract` が response を返す 3 つの公開入口（`handle_key` / `commit` / `receive_candidates`）でまとめて検査する: ①空の marked text を明示的に出す ②`commit` を出して marked text を出し直さない（`insertText` も marked セッションを終わらせ、`marked: None` は「そのまま」の意味なので誰も開き直さない）③空の `commit`（②の何も挿入しない版）
- **検査の範囲**: 上記は per-response で見える形だけ。「数 response 後にまだ composing か」は `marked: None` が前の値を引き継ぐため累積的で、proptest 側の `HostMarked` モデルが担当する。また `debug_assert` なので出荷ビルド（`--release`）では落ちる — 構造的保証は下の各条項が担い、これは回帰検出器
- snippet 状態のインライン表示は**常に非空**。フィルタが空のときは選択中スニペットの key を表示する。key が空になり得ないことは `SnippetStore::new` が構造的に保証し、body が空に展開されるエントリは `prefix_search` が落とす（`$date` 系は時刻依存なので、構築時の判定は陳腐化しうる。落とされた key は `LexSnippetStore::unusable_keys()` で取得でき、Swift 側が再読み込み時に報告する。engine の `tracing` 出力は出荷ビルドに乗らないので、診断はログではなく FFI 経由で返す）。提示できるエントリが 0 件のときは snippet 状態に入らない
- ブラウズは開始時の store スナップショットに対するトランザクション。ブラウズ中の `snippetsDidReload` は進行中のピッカーに影響しない。ただし空になった store でトリガーを再度押すとブラウズは畳まれる（提示できるものが無いため）
- **フォーカス喪失時の settle**（#298。以前は「未モデル」として保留していたが、実測の結果**到達可能**だった）: `deactivateServer` はホストが marked セッションを畳むことを意味するので、session を composing のまま残してはならない。IMKit は先に `commitComposition` を送るとは限らない（2026-08-01 実測: 未確定のままフォーカスを移す操作で `deactivateServer` が composing のまま届き、その大半で `commitComposition` は最後まで来なかった。指標は `activateServer` 時点で composing が残っていた回数で、同等の操作列で修正前 4 回 → 修正後 0 回）。したがって `SessionCoordinator.deactivate(client:)` が**表示を消すのと同じ呼び出しの中で** session を確定させる — 分かれていたから乖離した。
  - **settle は無条件、delivery は best-effort**。テキストを挿入する先（`lastClient`、無ければ IMKit が渡す sender）が無くても、session を composing のまま残す理由にはならない。`session.commit()` は挿入先の有無に関係なく状態を Idle にする
  - **settle は `commit()` ではなく `settle_unconfirmed()`**。フォーカス喪失は受容ではないので、自発的 commit の 2 つの副作用を持ち込んではならない: ①選択候補への解決 — 入力中のマークドテキストは navigate するまで**読み**なので、`commit()` で確定するとアプリを切り替えただけでユーザーが見ていない変換が文書に入る ②学習の記録 — 非自発的な操作を受容として扱うと top-1 が非シグナルで訓練され、1発目精度が劣化する。`settle_unconfirmed()` は**表示していたものをそのまま確定**し、履歴を書かない
  - **「表示していたもの」は engine が持たない。呼び出し側が渡す**。engine は marked text を *emit* するが、それが画面に届いたかは FFI 境界の向こう・別スレッドで決まる（`receive_candidates` は worker で走り、その response は UI スレッドにキューされてそこで落ち得る）。engine 内にコピーを置くと**観測できない状態の影**になる。実際 2 回失敗した: ①選択状態から推論 → 再描画で陳腐化 ②emit 時点で記録 → 配送前に先走る。`SessionCoordinator.currentDisplay` は `setMarkedText` 呼び出しと同じ場所で書かれるので、これが正解。engine は session の意味論（学習せず確定、Idle 復帰）だけを持ち、表示は入力として受け取る
  - settle は渡された文字列を**そのまま**確定する — prefix の再結合も `flush()` も行わない（pending の `n` を `ん` に変えると画面に無かった文字を入れることになる）。proptest invariant 10 が `HostMarked` モデルの値をそのまま渡して「表示していたものと確定されたものが一致する」ことを全生成列で固定する
  - **再入するホストへの防御**: client コールバック（`insertText`）が同期的に `deactivateServer` を再入し得る。epoch watermark は*別途キューされた* response しか弾けないので、実行中の `applyEvents` ループは止まらず、残りの `.setMarkedText` / `.showCandidates` が Idle な session に対して適用されてしまう。`SessionCoordinator.lifecycleGeneration` を teardown ごとに進め、`applyEvents` が世代の変化を見たら残りを捨てる
  - `Snippet` ブラウズは `commit()` と同じくキャンセル（確定テキスト無し）。確定されたものが無いので保持する対象が無い
  - `resetDisplay()` 側では確定しない: そこで確定すると**次にフォーカスを得た別クライアント**に前の文書のテキストを差し込むことになる。代わりに、表示の out-of-band クリアを単一の `clearDisplay()` に集約して debug assert を置く
  - **未モデルのまま残る半分**（`activateServer` 側）: 上の settle は `deactivateServer` が届く前提に立つ。IMKit がそれを飛ばす場合があることは `LeximeInputController.activateServer` のコメントが記録している（クライアントのクラッシュ等）。その経路では session が composing のまま `resetDisplay()` に到達し、表示だけが消える — #293 と同じ形。`clearDisplay()` の assert は**出荷ビルド（`-O`）では消える**ので、この経路には構造も検出も無い。session を破棄する FFI が無い（`LexSession` は `commit` のみ）ため、現状は既知ギャップとして記録するに留める。Rust 側 `debug_assert_response_contract` が「構造が防ぎ assert は回帰検出器」なのは response 経路の話で、この out-of-band 経路には当てはまらない
- **表示と確定の不一致（未解決、#309 で追跡）**: マークドテキストは読みを表示する一方、`commit` は選択サーフェスに解決する（§候補操作: Space/↑↓ で navigate するまで表示は かな のまま）。この不一致は**両系統のホストで見えるが、見え方が違う**:
  - ネイティブホスト（TextEdit 等）: こちらの `insertText` が効くので、画面で かな を見ていたユーザーの文書に**選んでいないサーフェス**が入る
  - Chromium / Electron 系: blur で**自前の composition を確定する**ため、残るのは表示していた読みで、こちらの `insertText` は視覚的に効かない

  2026-08-01 実測（本 settle 適用後、`deactivateServer` 経路で計測）: こちらは 8/8 で選択サーフェスを commit していたが、Slack の文書に残ったのは読みだった。`insertText` が視覚的に効かない以上、settle の追加でこれらのホストの見え方は変わっていない（settle 以前は commit 自体を発行していなかった）。なお `settle_unconfirmed()` が表示していた読みを確定するようになったことで、**この経路に限れば両系統のホストの結果は一致する**

**キーリマップ（settings.toml `[keymap]`）**

| key_code | 通常 | Shift |
|---|---|---|
| 10 | `]` | `}` |
| 93 | `\` | `\|` |

keymap に登録されたキーはリマップ後のテキストとして処理される。値が空文字のとき、その側は**リマップ無し**として扱う（空文字を挿入すると host の marked セッションだけ終わってしまうため。§不変条件の ③）。ファイルは有効なまま読み込まれ、`dictool settings-validate` が該当エントリを WARN として表示する。

TOML テーブルは**文字列**キーなので `10` / `010` / `"+10"` は同じ key_code を指す別エントリになる。1 つの key_code が解決するエントリは 1 つだけで、**キー文字列の辞書順で最小のものが勝つ**（起動ごとに変わらない）。負けたエントリの値は照合に使われない — 勝ったエントリの側が空文字なら、その側はリマップ無しのままで、負けたエントリが代わりを供給することはない（ただし妥当性検査は全エントリに適用される。key_code として解釈できない / 要素数が 2 でないエントリは、勝敗に関わらずファイル全体を reject する。空文字を許すのは「値が空」であって「壊れたファイル」ではない）。この解決は `parse_keymap` の 1 パスで行われ、`keymap_get` が読む map と WARN の両方をそこで作る（照合と診断が別々に導出して食い違わないようにするため）。

WARN は `settings_keymap_warnings` で FFI を渡り、設定読み込み成功後に frontend が報告する（`dictool settings-validate` でも同じ内容が出る）。engine の `tracing` は出荷ビルドに届かないため、ログのみの診断はサイレント失敗になる。
かなモードではリマップ後のテキストがローマ字 trie・通常入力パスを経由する（例: `]` → `」`）。
trie にマッチしない文字（例: `\`）は直接確定。ABC モードでは常に直接確定。
`settings.toml` の `[keymap]` セクションで追加・変更可能。

### ローマ字変換

Rust engine 内の Trie（HashMap ベース）で 306 のマッピングをサポート（`default_romaji.toml`、`include_str!` で埋め込み）:

- 基本五十音、濁音・半濁音、拗音
- 小書き（`xa`/`la` 系）
- 拡張（`fa`, `va`, `tsa` 等）
- 特殊（`wi`→ゐ、`we`→ゑ、`nn`/`n'`/`xn`→ん、`-`→ー）
- z-sequences（Mozc 互換）: `zh`→←、`zj`→↓、`zk`→↑、`zl`→→、`z.`→…、`z,`→‥、`z/`→・、`z-`→〜、`z[`→『、`z]`→』
- 促音: 子音連打を自動検出（`kk`→っ+k）
- 撥音: `n` + 非母音・非 n・非 y → ん
- ラテン子音＋かな母音のコラプス: composedKana 内の `[latin][あいうえお]` パターンを trie で再検索して合成（例: `kあ`→`か`、`shあ`→`しゃ`）

カスタムローマ字テーブル: `~/Library/Application Support/Lexime/romaji.toml`（完全置換、マージなし）。`mise run romaji-export` でデフォルトをエクスポート可能。

### 候補生成

#### Standard モード

composing 中、キーストロークごとに候補を生成し、以下の順序で統合する:

1. **Viterbi N-best** — N-best 変換候補（#1 はリアルタイム表示用）
2. **学習済みサーフェス** — ユーザーが過去に確定した変換をブースト降順で注入（N-best に含まれない場合のみ）
3. **ひらがな** — 元のかな（学習ブーストがあれば上位に移動）
4. **予測候補** — `predict_ranked` による prefix search
5. **辞書 lookup** — 全読み候補（学習履歴で並び替え）

重複は engine 内で排除する。句読点入力時は代替候補（`。`→`．`/`.` 等）を生成する。
マークドテキストにはひらがな（入力中のかな + pending romaji）をリアルタイム表示し、Space / ↑↓ で候補に切り替えると選択サーフェスを表示する。

#### Predictive モード

Viterbi N-best をベースに、学習バイグラムを連鎖させた予測候補を生成する:

1. Viterbi N-best で変換候補を取得
2. 各候補の末尾セグメントから `bigram_successors` でバイグラム後続を探索
3. サイクル検出（`HashSet` で訪問済みサーフェスを追跡）付きで最大チェーン長まで連鎖
4. 重複排除後に統合

非同期候補生成（`defer_candidates`）と組み合わせて使用する。

### 変換モード

`ConversionMode` enum で Standard / Predictive を切り替える。

| | Standard | Predictive |
|---|---|---|
| 候補生成 | standard | predictive (bigram chaining) |
| Tab の動作 | 確定 | 確定 |
| 自動確定 | 有効 | 無効 |

- 設定 UI（開発者タブ）で切替。変更後は Lexime の再起動が必要
- UserDefaults `conversionMode` で永続化（0=Standard, 1=Predictive）

## 変換パイプライン

```
ローマ字入力
  → ひらがな (lex-core/romaji)
  → 統一候補生成 (lex-core/candidates)
    → ラティス構築 (common_prefix_search + 1文字フォールバック)
    → Viterbi N-best 探索
    → Reranker (structure cost + 学習ブースト)
    → Rewriters (カタカナ / ひらがな / 数字)
    → 文節グルーピング (自立語 + 付属語)
    → 学習済みサーフェス注入 + 予測候補 + 辞書 lookup の統合・重複排除
  → 候補表示 (CandidatePanel)
```

### ラティス構築

- `Dictionary::common_prefix_search` で辞書の Trie を効率的に走査
- 各位置から始まる全てのエントリをノードとして追加
- **接続性保証**: 1 文字マッチがない位置にはコスト 10,000 の未知語フォールバックを追加

### Viterbi N-best 探索 + 後処理

- 累積コストに i64 を使用（i16 オーバーフロー回避）
- 前方パス: ノードごとに top-K コスト/バックポインタを保持
- N-best: 同一サーフェスの重複排除後、上位 N パスを出力
- **Reranker**: Viterbi で over-generate（1-best: 10 候補、N-best: 3x）し、structure cost（累積遷移コスト）で再ランキング。セグメント数が少なく長いパスを優先
- **Rewriters**: N-best パスに対して追加候補を生成
  - `KatakanaRewriter` — カタカナ候補追加
  - `HiraganaVariantRewriter` — 漢字セグメントをひらがなに置換した候補追加
  - `NumericRewriter` — 日本語数詞の半角・全角数字候補追加
- **文節グルーピング**: 接続行列 V3 に埋め込まれた POS ロール（`ContentWord` / `FunctionWord` / `Suffix` / `Prefix`）に基づき、形態素列を自立語 + 付属語のフレーズ単位にマージ

### CostFunction trait

```
CostFunction
├── word_cost(node) → i64
├── transition_cost(prev, next) → i64
├── bos_cost(node) → i64
└── eos_cost(node) → i64
```

| 実装 | 用途 |
|---|---|
| `DefaultCostFunction` | 辞書コスト + 接続行列コストをそのまま使用 |

学習ブーストは Viterbi のコスト関数ではなく、Reranker で適用する（コスト関数を汚染しない設計）。

## 自動確定

Standard モードでのみ有効（`try_auto_commit` 内で `auto_commit_enabled` をガード）。長い入力を途中で区切って確定することで、composedKana の肥大化を防ぐ。

### 安定度トラッカー

`StabilityTracker` が Viterbi 結果の先頭セグメントを監視し、同一リーディングが連続したカウントを記録する。

- **安定度閾値**: `count ≥ 3`（3 回連続で先頭セグメントが同じ）
- **セグメント閾値**: `segments ≥ 4`（パス全体のセグメント数が 4 以上）
- 両条件を満たすと、安定したセグメントを自動確定

### 連続 ASCII グルーピング

english サブモードで入力された連続 ASCII セグメントは、1 文字ずつではなく単語単位でまとめて自動確定する。

## 非同期候補生成

候補生成は Rust 側の `AsyncWorker` でバックグラウンド実行し、完了時に `LexSessionEvents` コールバックで Swift に結果を push する。

### アーキテクチャ

1. キー入力 → `LexSession::handle_key()` → セッションが `async_request` を返す
2. `handle_key` 内で自動的に `AsyncWorker` にサブミット
3. `AsyncWorker` のワーカースレッドが候補を生成
4. 完了時に `LexSessionEvents::on_async_response` を呼び、セッションを更新した上で `LexKeyResponse` を Swift に渡す
5. Swift 側は main thread に dispatch して IMKit / 候補パネルに反映
6. 結果が stale（generation counter 不一致）なら破棄

### AsyncWorker

| スレッド | 優先度 | 内容 |
|---|---|---|
| Candidate | `.userInitiated` | 候補生成（Standard / Predictive） |

- `AtomicU64` generation counter で staleness を管理
- mpsc チャネルの drain-to-latest で最新リクエストのみ処理
- ワーカースレッドは `LexSession` Drop または `shutdown()` で join される
- foreign callback 呼び出しは `catch_unwind` で保護

## 学習機能

### データ構造

- **ユニグラム**: `reading → surface → HistoryEntry`（最大 10,000 件）
- **バイグラム**: `prev_surface → next_reading → next_surface → HistoryEntry`（最大 10,000 件）
- **HistoryEntry**: `frequency: u32`, `last_used: u64`（Unix epoch）

### ブースト計算

```
boost = min(frequency × 3000, 15000) × decay(last_used)
decay = 1.0 / (1.0 + hours_elapsed / 168.0)
```

- 半減期: 1 週間（168 時間）
- 最大ブースト: 15,000（frequency ≥ 5 で到達）
- Reranker が Viterbi 後のパスに対してブーストを適用し、学習した変換を優先する
- 学習済みサーフェスを候補上位に注入（N-best 直後、boost 降順）

### バイグラム後続探索

`bigram_successors(prev_surface)` は、指定サーフェスに続くバイグラムエントリを検索し、`(reading, surface, boost)` のリストをブースト降順で返す。Predictive モードの bigram chaining で使用される。

### LearningRecord

| バリアント | 説明 |
|---|---|
| `Committed { reading, surface, segments }` | 確定時に生成。FFI 層が `UserHistory::record_at()` で whole-reading + sub-segments の 2 段階記録 |
| `Deletion { segments }` | ForwardDelete 時に生成。FFI 層が WAL に `Tombstone` frame を append（毎回 `F_FULLFSYNC`）し、`UserHistory::remove_entries()` でメモリから削除、非同期スクラブ compaction で checkpoint から物理消去。no-op 判定の条件は §個別削除 |

### 個別削除

候補選択中に Fn+Delete を押すと、選択中の候補に対応する学習エントリ（ユニグラム + バイグラム）を削除する。削除は WAL に `Tombstone` frame を書き込んで（書き込み時に `F_FULLFSYNC`）耐久化し、replay 時に削除が再適用されるため WAL リプレイで復活しない。物理スクラブ（旧 checkpoint と過去 Committed frame に残る文字列の消去）は直後に非同期 compaction をスケジュールして行い、`scrub_pending` + `compact_gate`（Mutex）で直列化する。

学習されていない候補の削除（no-op）には Tombstone を書かず `F_FULLFSYNC` を払わない。**ただし「メモリに無い」は「ディスクに無い」を意味しない** — 退避（§退避）で落ちたエントリや、耐久化が確立しなかった削除の対象は、メモリから消えていても直近の checkpoint には残っている。これらのキーは `DurableResidue` として記録され、no-op 判定から除外して必ず Tombstone を書く（そうしないと削除が黙って無効になり、次回起動で復活する）。キー単位で追跡するため、退避が起きても**それ以外の未学習候補の削除は fast path のまま**。この状態は次の compaction 成功で解ける（追跡キーが上限に達した場合のみ、キー単位の追跡を諦めて「常に Tombstone を書く」保守動作に退避する。上限は 1 compaction 区間では到達しないよう設定してあるので、これは checkpoint が失敗し続けている状態の症状）。

Tombstone の WAL 耐久化が失敗した場合（`Io` / `SyncFailed`）は、フォールバックとして checkpoint を**同期的に**書き出して削除を永続化する（`durability_failed` → `run_gated_compact`、design §5.4）。

### 保存（WAL + Checkpoint、LXUD v2）

- **Checkpoint**: LXUD v2（32 バイト固定ヘッダ: マジック `LXUD` + version 2 + `applied_seq` + created_at + body_len + CRC32、body は bincode。CRC はヘッダ先頭 28 バイト + body を保護 — `applied_seq` は replay filter を駆動するため無防備な bit flip を許さない）。v1（マジック + version 1 + bincode）の reader を保持し、起動時に一回で v2 へ migration（元 v1 は `.v1.bak` へ best-effort 退避）
- **WAL**: `user_history.lxud.wal`（8 バイトファイルヘッダ `LXWL` + version 2。フレーム形式: payload_len + CRC32(seq+payload) + seq + bincode(`WalRecord::Committed|Tombstone`)）
- **seq**: WAL フレームは単調増加の連番を持ち、checkpoint ヘッダの `applied_seq` が「効果を含む最後の seq」を記録。replay は `seq > applied_seq` のフレームのみ適用するため、checkpoint 書き込みと WAL truncate の間でクラッシュしても二重適用が構造的に起きない
- **書き込み**: 確定時に WAL append（Committed は 50 frame ごとに write barrier = `fcntl(F_BARRIERFSYNC)`）、閾値到達で background compaction（checkpoint を tmp + `sync_all` + rename + 親 dir fsync（best-effort・log-only）で書き出し + 条件付き WAL truncate）。compaction の排他は `compact_gate` (Mutex)、削除・障害後の即時要求は `scrub_pending` で直列化。削除は `Tombstone` frame（削除の WAL 表現、書き込み時に毎回 `F_FULLFSYNC`）を append し、直後に非同期スクラブ compaction をスケジュールして物理消去する。全消去（`clear`）は空 checkpoint（`applied_seq` = 現 WAL 最大 seq）を先行書き込みしてコミットポイントとし、以後どのクラッシュ点でも空履歴に収束する（旧 WAL frame は全て skip される）
- **場所**: `~/Library/Application Support/Lexime/user_history.lxud`
- **起動時（エンジン経路）**: `recovery::open_recovering` — checkpoint ロード → WAL replay（evict なし + 事後 1 回）→ in-memory 復元。破損は `.corrupt-<epoch>` へ隔離（直近 3 個保持）して空で継続、WAL 末尾破損は last-good オフセットで物理修復。どのファイル状態でも起動は成功し学習は継続する（`OpenReport` に結果を記録。Err は EACCES 等の環境障害のみ）。`OpenReport` は v1→v2 migration の commit 失敗（`migration_failed`。v1 ファイルは温存する。再試行のタイミングは経路による — legacy WAL を消費していた場合は WAL が frozen になるため `appends_frozen` 由来の起動時 compaction が副作用として v2 checkpoint を書き変換を完了させ、そうでなければ次回起動が再試行する。**compaction は migration ではない**（`.v1.bak` 退避も `Migrated` 状態設定も行わない）ため、失敗した migration の再試行に compaction を使うことはしない — commit が失敗している経路でそれを走らせると v1 ファイルを潰す。`.v1.bak` は design 決定 #13 どおり best-effort のままで、正しさの前提条件ではない）と append 凍結（`appends_frozen`。このセッションの学習は compaction が heal するまでメモリのみ）も持つ。`migration_failed` は `checkpoint_state` / `wal_state` が健全値のまま真になりうるので独立フィールドが要る。`appends_frozen` は逆に `RepairFailed` / `Quarantined` と同時に立つ場合もあり（凍結の 5 経路のうち健全値のままなのは legacy WAL つき migration 失敗のみ）、どちらの向きにも畳めない — どちらも `checkpoint_state` / `wal_state` は健全な値のままなので、それらだけでは正常起動と区別できない
- **オフラインツール経路**: `UserHistory::open` / `open_with_wal` は無副作用・厳格エラーのまま（監査ツールが稼働中 IME のファイルを rename しない）

### 退避

容量超過時、`frequency × decay(last_used)` のスコアが低いエントリから削除。退避はメモリからのみ消す操作であり、落としたエントリは次の compaction が checkpoint を書き直すまでディスクに残る。そのため退避したキーは `DurableResidue` に記録され、削除の no-op 判定に反映される（§個別削除）。

### コミットログ（診断用）

変換確定イベントを JSONL で checkpoint と同じディレクトリ（`commit-log.jsonl`）に追記する（全セッション共有の Mutex で直列化）。identity な auto-commit（surface == reading）は学習はしないがログには載る（受容率の分母を欠かさないため）。対象は候補リストからの変換確定のみで、変換判断を伴わない確定（生かなの overflow commit、ABC パススルー、snippet 展開）は含まない。1 行 = 1 変換確定: `t`（epoch 秒）/ `reading` / `surface` / `rank`（確定時の選択候補 index。0 = top-1 受容、>0 = 手動選択 = 変換ミスの一次signal）/ `top1`（rank>0 のときのみ、その時の top-1 surface）/ `auto`（auto-commit 由来のときのみ true）。ローカル専用の診断データで、lextool によるオフライン集計（実使用 top-1 受容率の推移、ミス頻度）に使う。履歴 `clear()` で一緒に削除される。書き込み失敗は警告ログのみ（確定経路を壊さない）。

## ユーザー辞書

ユーザーが手動登録する単語辞書。`Dictionary` trait を実装し、`CompositeDictionary` のレイヤーとして統合。

- **データ構造**: `RwLock<HashMap<String, Vec<UserEntry>>>`（reading → entries）
- **POS ID**: 1852（名詞,一般）、cost: -1（システム辞書より常に優先）
- **形式**: LXUW（マジック `LXUW` + version 1 + bincode）。LXUD と共有する persistence primitive で耐久アトミック書き込み（tmp + `sync_all` + rename + 親 dir fsync（best-effort・log-only））。読み込みは alloc 上限（`with_limit`）つきで巨大 alloc を防ぐ。フォーマットは CRC なし・単一ファイル・WAL なしのまま（数十エントリ・明示 `save` 駆動のため最小ハードニングに留める、design §9）
- **場所**: `~/Library/Application Support/Lexime/user_dict.lxuw`
- **操作**: `register` / `unregister` は write lock、`Dictionary` trait（lookup / predict 等）は read lock
- **起動時（エンジン経路）**: `UserDictionary::open_recovering` — 破損（マジック / version / bincode / 長さ）は `.corrupt-<epoch>` へ隔離（直近 3 個保持）して空辞書で継続 + report 記録。破損ファイルが残存して毎起動 throw し登録語が永久に失われる経路を塞ぐ。Err は EACCES 等の環境障害のみ
- **オフラインツール経路**: `UserDictionary::open` は無副作用・厳格エラーのまま（CLI の監査ツールが稼働中 IME のファイルを rename しない）
- **CLI**: `dictool user-dict add/remove/list`

## 設定の外部化

### settings.toml

`default_settings.toml`（`include_str!`）+ OnceLock パターン。カスタム: `~/Library/Application Support/Lexime/settings.toml`（完全置換）。

| セクション | パラメータ |
|---|---|
| `[cost]` | segment_penalty, mixed_script_bonus, katakana_penalty, pure_kanji_bonus, latin_penalty, unknown_word_cost |
| `[reranker]` | length_variance_weight, structure_cost_filter |
| `[history]` | boost_per_use, max_boost, half_life_hours, max_unigrams, max_bigrams |
| `[candidates]` | nbest, max_results |
| `[snippets]` | trigger（スニペットモードのトリガーキー）, variables（`$name` 展開用のユーザー定義変数） |
| `[keymap]` | key_code = ["normal", "shifted"]（オプショナル、デフォルト: 10→]/}, 93→\\/\|） |

`mise run settings-export` でデフォルトをエクスポート。`dictool settings-validate` で検証。

### romaji.toml

`default_romaji.toml`（306 エントリ、`include_str!`）+ OnceLock パターン。カスタム: `~/Library/Application Support/Lexime/romaji.toml`（完全置換、マージなし）。

`mise run romaji-export` でデフォルトをエクスポート。`dictool romaji-validate` で検証。

## アクセシビリティ

### VoiceOver 候補読み上げ

`CandidatePanel` が候補選択時に VoiceOver アナウンスを発行する。

- `NSWorkspace.shared.isVoiceOverEnabled` で VoiceOver の有効/無効を確認
- 有効時、`NSAccessibility.post(notification: .announcementRequested)` で「候補テキスト index/total」形式を読み上げ
- 優先度は `high` に設定し、他のアナウンスに割り込み

## 開発フェーズ

### Phase 1: MVP — **完了**

macOS で動作する最小限の IME を構築。

- InputMethodKit スケルトン IME
- ローマ字→かな変換（Trie ベース）
- Rust エンジン + FFI ブリッジ
- Mozc 辞書による基本検索
- 結合: ローマ字→かな→辞書検索→候補表示→確定

### Phase 2: 予測変換 — **完了**

リアルタイム予測入力と高精度変換。

- 予測候補のリアルタイム表示（prefix search）
- ラティス構築 + Viterbi 最小コスト探索
- 候補確定の操作体系（Space / Enter / Tab / 数字キー）

### Phase 3: 学習機能 — **完了**

ユーザーの入力パターンに基づく適応的なランキング。

- ユニグラム + バイグラム学習（時間減衰付き）
- Reranker による学習ブースト適用
- 候補リストの並び替え（学習済みエントリ優先）
- 学習済みサーフェスの候補上位注入
- ローカル保存（LXUD + WAL 形式、アトミック書き込み）

### Phase 4: Speed of Thought — **完了**

思考の速度で日本語を書ける開発者向け IME を目指す。

**1発目精度の向上**

- 学習収束の高速化（`BOOST_PER_USE` を 3000 に引き上げ、frequency 5 で最大ブースト到達）
- バイグラム活用の強化（直前の文脈を変換精度に反映）

**リアルタイム変換表示 + 句読点自動確定**

- マークドテキストにひらがなをリアルタイム表示（候補選択時のみ変換結果を表示）
- 句読点入力で直前の変換を自動コミット＋句読点を直接挿入
- Enter で index 0（Viterbi #1）も学習付きで確定
- Escape はひらがなで確定（IMKit の制約: Escape 後に `commitComposition` が呼ばれる）

**キーリマップ**

- `settings.toml` の `[keymap]` セクションで keyCode → 文字のリマップを定義
- デフォルト: keyCode 10 → `]`/`}`、keyCode 93 → `\`/`|`（JIS キーボード対応）
- かなモード: リマップ後のテキストをローマ字 trie・通常入力パスに通す（trie マッチしない文字は直接確定）
- ABC モード: 直接確定

**候補パネルのカーソル追従**

- 候補パネルをマークドテキスト末尾（入力カーソル位置）に追従させる
- composedKana を長く保持する方針と整合させ、視線移動を最小化

**Predictive モード**

- Viterbi base + bigram chaining による予測変換
- `ConversionMode` enum（Standard / Predictive）で切替可能
- 設定 UI（開発者タブ）で切替（再起動必要）
- Tab キーで予測候補を確定

**アーキテクチャ改善**

- UniFFI proc-macro バインディング（手動 C FFI 全削除）
- ワークスペース分割（lex-core / lex-session / lex-cli）
- 非同期内部化（AsyncWorker: 候補生成を Rust ワーカースレッドで実行）
- イベント駆動 FFI（LexKeyResponse + LexEvent enum）
- セッション責務分離（composing / commit / auto_commit / response）
- ローマ字・設定の TOML 外部化
- Dictionary trait 統一 + CompositeDictionary
- ユーザー辞書（LXUW 形式、CompositeDictionary レイヤー）
- WAL 付き学習履歴
- Rewriters（カタカナ / ひらがな / 数字候補追加）

### Phase 5: 設定 UI — **完了**

ユーザーが設定を変更できる SwiftUI ベースの UI を追加。

- メニューバーの Lexime アイコン右クリック → 「設定...」でアクセス
- **ユーザ辞書タブ**: 単語の一覧・追加・削除（LexEngine FFI 経由）
- **開発者タブ**（`UserDefaults` `developerMode` フラグで表示制御）: 変換モード切替、romaji.toml / settings.toml テキストエディタ（保存・再読み込み・デフォルトに戻す）
- `NSHostingView` + activation policy 切替（`.accessory` on open / `.prohibited` on close）で Dock アイコンなし
- 「Lexime を再起動」ボタンで設定変更を即座に反映（`exit(0)` → macOS 自動再起動）

### Phase 6+ (今後)

- ニューラルリスコアリング: GGUF ニューラルモデル（azooKey/Zenzai 方式）で Viterbi N-best をリスコアし変換精度を向上（lex-core に実験モジュールあり、IME 統合は速度課題のため未定）

## ビルド・CI

### mise.toml タスク

| タスク | 内容 |
|---|---|
| `engine-lib` | universal static library ビルド（x86_64 + aarch64、lipo） |
| `uniffi-gen` | UniFFI Swift バインディング自動生成 |
| `build` | Lexime.app ユニバーサルバイナリのビルド |
| `install` | `~/Library/Input Methods` へコピー |
| `reload` | Lexime プロセスを再起動 |
| `fetch-dict-mozc` | Mozc 辞書データのダウンロード |
| `fetch-dict-symbols` | バンドル記号 TSV の生成（ギリシャ文字・数学記号） |
| `fetch-dict-extras` | curated ドメイン TSV の生成（IT / 食 / 地理 ほか） |
| `dict-mozc` | Mozc 辞書バイナリのコンパイル |
| `dict` | 辞書のコピー |
| `dict-clean` | コンパイル済み辞書の削除（次回ビルドで再コンパイル） |
| `conn` | 接続行列のコンパイル |
| `test-swift` | Swift UniFFI ラウンドトリップテスト |
| `test` | lint + `cargo test --workspace --all-features` |
| `lint` | `cargo fmt --check` + `cargo clippy --all-targets` |
| `audit` | quarantine / build.rs スクリーニング + `--locked` 検証 + cargo-deny（脆弱性・ライセンス）+ cargo-vet + cargo-machete（未使用 deps） |
| `log` | ログストリーミング |
| `trace-log` | トレース JSONL ストリーミング |
| `icon` | アイコンアセット生成 |
| `clean` | ビルド成果物の削除 |
| `explain` | 変換パイプラインの説明出力 |
| `snapshot` | 変換スナップショット生成 |
| `diff-snapshot` | スナップショット差分比較 |
| `accuracy` | 変換精度テスト（accuracy-corpus.toml） |
| `accuracy-history` | 履歴込み変換精度テスト（accuracy-corpus-history.toml） |
| `history-audit` | 学習履歴を素のエンジン top-1 と突き合わせて監査 |
| `bench` | criterion ベンチマーク |
| `fetch-model` | Zenzai GGUF モデルダウンロード |
| `neural-score` | ニューラルスコアリングベンチマーク |
| `romaji-export` | デフォルトローマ字テーブルを `~/Library/Application Support/Lexime/romaji.toml` にエクスポート |
| `settings-export` | デフォルト設定を `~/Library/Application Support/Lexime/settings.toml` にエクスポート |

### CI

`.github/workflows/ci.yml`:

- **トリガー**: push to main + pull_request
- **パスフィルタ**: `dorny/paths-filter` で変更コンポーネントを検出し、不要なジョブをスキップ

| ジョブ | 環境 | 条件 | 内容 |
|---|---|---|---|
| `changes` | ubuntu-latest | 常時 | パスフィルタ検出（core / session / ffi / cli / corpus / swift） |
| `screen` | ubuntu-latest | 常時 | quarantine + build.rs ベースライン検査。cargo を呼ぶ全ジョブをこれが gate する |
| `lint` | ubuntu-latest | Rust 変更時 | `cargo fmt --check` + `cargo clippy --all-targets` |
| `msrv` | ubuntu-latest | Rust 変更時 | `cargo check --workspace --locked`（宣言 MSRV。デフォルト features / targets） |
| `test-core` | ubuntu-latest | core 変更時 | `cargo test -p lex-core --features trace,neural` |
| `test-session` | ubuntu-latest | session/core 変更時 | `cargo test -p lex-session --features trace` |
| `test-engine` | ubuntu-latest | core/session/ffi 変更時 | `cargo test -p lex_engine --features trace` |
| `test-cli` | ubuntu-latest | core/cli 変更時 | `cargo test -p lex-cli` |
| `accuracy` | ubuntu-latest | core/cli/corpus 変更時 | `mise run accuracy` + `mise run accuracy-history`（Mozc スナップショットは `mozc-pin.txt` で固定） |
| `audit` | ubuntu-latest | core 変更時 | `--locked` 検証 + `cargo-deny` + `cargo-vet` + `cargo-machete` |
| `swift` | macos-latest | engine または Swift 変更時 | `mise run test-swift` |

Rust ジョブは `Swatinem/rust-cache@v2` を使い、多くは `shared-key: engine` を共有する（`msrv` は toolchain 固定、`accuracy` は release プロファイルのため専用キー、`screen` は意図的にキャッシュなし）。理由は各ジョブのコメント参照。

> この表と上の mise タスク表は**概観**であり正準ではない。正準は `.github/workflows/ci.yml` と `mise.toml`（`mise tasks` で一覧できる）。齟齬があれば向こうが正 — 書き写しは drift するので、ジョブやタスクの増減をここへ反映し忘れても壊れない前提で読むこと。

## 未決事項

- リリースワークフロー（パブリック化後のタグプッシュによる自動ビルド）
