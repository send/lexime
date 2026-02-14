# Lexime 仕様書 (v1.1)

## 概要

Lexime は macOS 向けの日本語予測入力システム。
PRIME にインスパイアされた予測変換型の入力体験を、軽量・高速に提供する。

## アーキテクチャ

```
┌──────────────────────────────────────────────┐
│  macOS (InputMethodKit)                      │
│  ┌────────────────────────────────────────┐  │
│  │  Swift: IME Frontend                   │  │
│  │  - LeximeInputController (状態管理)     │  │
│  │  - KeyHandlers (キー入力処理)           │  │
│  │  - MarkedTextManager (インライン表示)   │  │
│  │  - CandidatePanel (候補ウィンドウ)      │  │
│  │  - DictBridge (FFI ラッパー)            │  │
│  └─────────────┬──────────────────────────┘  │
│                │ FFI (C ABI)                  │
│  ┌─────────────▼──────────────────────────┐  │
│  │  Rust: 変換エンジン (liblex_engine)     │  │
│  │  - romaji (ローマ字→かな変換)          │  │
│  │  - candidates (統一候補生成)            │  │
│  │  - dict (辞書検索・予測)                │  │
│  │  - converter (ラティス構築・Viterbi)    │  │
│  │  - user_history (学習・ランキング)      │  │
│  │  - lib.rs (FFI 関数)                   │  │
│  └────────────────────────────────────────┘  │
└──────────────────────────────────────────────┘
```

## コンポーネント詳細

### Swift Frontend

| ファイル | 役割 |
|---|---|
| `main.swift` | エントリポイント。辞書・接続行列・学習データの読み込み、IMKServer 起動 |
| `LeximeInputController.swift` | IMKInputController サブクラス。状態管理、学習記録 |
| `KeyHandlers.swift` | idle / composing 各状態のキー入力ハンドラ |
| `MarkedTextManager.swift` | インライン表示（未確定文字列） |
| `CandidatePanel.swift` | 候補ウィンドウ（NSPanel、ページネーション） |
| `DictBridge.swift` | Rust FFI のラッパー関数（romaji lookup / convert / generate candidates） |
| `InputState.swift` | `InputState` enum（idle / composing）、`InputSubmode` enum（japanese / english） |

### Rust Engine (`engine/src/`)

| モジュール | 内容 |
|---|---|
| `lib.rs` | FFI 関数 (39 関数)。C 互換構造体、メモリ管理（`OwnedVec` パターン） |
| `romaji/` | ローマ字→かな変換。Trie（HashMap ベース）、141+ マッピング、促音・撥音・コラプス |
| `candidates.rs` | 統一候補生成。句読点代替、予測 + Viterbi N-best + 辞書 lookup の統合・重複排除。予測モード（bigram chaining） |
| `dict/` | `Dictionary` trait、`TrieDictionary`（bincode）、`ConnectionMatrix`（LXCX） |
| `dict/source/` | `DictSource` trait、`MozcSource`、`SudachiSource`、`pos_map`（POS ID リマップ） |
| `converter/` | `Lattice` 構築、`Viterbi` N-best 探索、`Reranker`、`CostFunction` trait |
| `user_history/` | ユニグラム・バイグラム学習、LXUD 形式 |
| `bin/dictool.rs` | 辞書操作 CLI（fetch / compile / compile-conn / merge / diff / info） |

### 辞書データ

Mozc 辞書のみを使用。ファイル名は `lexime.dict` / `lexime.conn`。

- **辞書**: Mozc TSV → `TrieDictionary`（bincode シリアライズ、マジック `LXDC`、約 49MB）
- **接続行列**: バイナリ行列（マジック `LXCX`、i16 配列）。V3 フォーマットでは POS ロールメタデータ（`ContentWord` / `FunctionWord` / `Suffix` / `Prefix`）を埋め込み、文節グルーピングに使用
- POS ID ペアの遷移コストを O(1) で参照

### FFI (C ABI)

`engine/include/engine.h` で公開する 39 関数:

| カテゴリ | 関数 |
|---|---|
| ユーティリティ | `lex_engine_version`, `lex_engine_echo`, `lex_trace_init` |
| ローマ字 | `lex_romaji_lookup`, `lex_romaji_lookup_free`, `lex_romaji_convert`, `lex_romaji_convert_free` |
| 辞書 | `lex_dict_open`, `lex_dict_close`, `lex_dict_lookup`, `lex_dict_predict`, `lex_dict_predict_ranked`, `lex_candidates_free` |
| 接続行列 | `lex_conn_open`, `lex_conn_close` |
| 変換 | `lex_convert`, `lex_conversion_free`, `lex_convert_nbest`, `lex_convert_nbest_with_history`, `lex_conversion_result_list_free` |
| 候補生成 | `lex_generate_candidates`, `lex_generate_prediction_candidates`, `lex_candidate_response_free` |
| セッション | `lex_session_new`, `lex_session_free`, `lex_session_set_programmer_mode`, `lex_session_set_defer_candidates`, `lex_session_set_conversion_mode`, `lex_session_handle_key`, `lex_session_commit`, `lex_session_is_composing`, `lex_session_receive_candidates` |
| 学習 | `lex_history_open`, `lex_history_close`, `lex_history_record`, `lex_history_save`, `lex_convert_with_history`, `lex_dict_lookup_with_history`, `lex_key_response_record_history` |
| レスポンス | `lex_key_response_free` |

メモリ管理: Rust 側が `OwnedVec` / `OwnedCandidateResponse` で文字列を所有し、呼び出し元が `*_free()` で解放する。

## 入力モデル

### 状態遷移

```
idle ──(ローマ字入力/句読点)──→ composing ──(Enter/Escape)──→ idle
                                    │
                                    ├──(Tab)──→ サブモード切替（japanese ↔ english）
                                    └──(Option+Tab)──→ 変換モード切替（Standard ↔ Predictive）
```

### 各状態でのキー操作

**idle**

| キー | 動作 |
|---|---|
| ローマ字 | composing へ遷移 |
| 句読点（`,` `.` 等） | 全角句読点で composing へ遷移 |
| Tab | 日英サブモードをトグル |
| 英数キー | システム ABC 入力ソースに切替 |

**composing（japanese サブモード）**

| キー | 動作 |
|---|---|
| ローマ字 | かな追加、候補更新（Viterbi #1 をインライン表示） |
| z + `h/j/k/l/.,/-/[/]` | Mozc 互換 z-sequence（矢印・記号）を入力 |
| Space / ↓ | 次の候補を選択（初回 Space は index 1 から開始） |
| ↑ | 前の候補を選択 |
| Enter | 表示中の候補を確定（変換結果 + 学習記録） |
| Tab | Standard: english サブモードへ切替 / Predictive: 確定 |
| Option+Tab | 変換モード切替（Standard ↔ Predictive） |
| Backspace | 1 文字削除（空になれば idle へ） |
| Escape | ひらがなで確定（IMKit が commitComposition を呼ぶため） |
| 句読点 | 現在の変換を確定し、句読点を直接挿入 |
| その他の文字 | composedKana に追加（Backspace で削除可能） |

**composing（english サブモード）**

| キー | 動作 |
|---|---|
| 印字可能 ASCII | composedKana にそのまま追加（大文字小文字保持、Viterbi スキップ） |
| Space | スペース文字を composedKana に追加 |
| Enter | composedKana をそのまま確定（学習なし） |
| Tab | japanese サブモードへ切替 |
| Backspace | 1 文字削除（空になれば idle へ） |
| Escape | composedKana をそのまま確定 |

**全状態共通（programmerMode）**

| キー | 動作 |
|---|---|
| ¥（ON） | 入力中のテキストを確定し `\` を挿入 |
| ¥（OFF） | 通常の ¥ をパススルー（デフォルト） |
| Shift+¥ | `|`（パイプ）をパススルー（モード無関係） |

`programmerMode` は UserDefaults で永続化（`defaults write sh.send.inputmethod.Lexime programmerMode -bool true/false`）。
JIS キーボードの ¥ キー（keyCode 93）のみ対象。US キーボードには影響なし。

### ローマ字変換

Rust engine 内の Trie（HashMap ベース）で 141+ のマッピングをサポート:

- 基本五十音、濁音・半濁音、拗音
- 小書き（`xa`/`la` 系）
- 拡張（`fa`, `va`, `tsa` 等）
- 特殊（`wi`→ゐ、`we`→ゑ、`nn`/`n'`/`xn`→ん、`-`→ー）
- z-sequences（Mozc 互換）: `zh`→←、`zj`→↓、`zk`→↑、`zl`→→、`z.`→…、`z,`→‥、`z/`→・、`z-`→〜、`z[`→『、`z]`→』
- 促音: 子音連打を自動検出（`kk`→っ+k）
- 撥音: `n` + 非母音・非 n・非 y → ん
- ラテン子音＋かな母音のコラプス: composedKana 内の `[latin][あいうえお]` パターンを trie で再検索して合成（例: `kあ`→`か`、`shあ`→`しゃ`）

### 候補生成

#### Standard モード

composing 中、キーストロークごとに `lex_generate_candidates` を 1 回呼び出し、以下の候補を engine 内で統合する:

1. **Viterbi #1** — N-best 変換の最良候補（先頭、リアルタイム表示用）
2. **ひらがな** — 元のかな（Viterbi #1 と同一なら重複排除でスキップ）
3. **予測候補** — `predict_ranked` による prefix search
4. **Viterbi #2+** — N-best 変換の 2 位以降
5. **辞書 lookup** — 全読み候補

重複は engine 内で排除する。句読点入力時は代替候補（`。`→`．`/`.` 等）を生成する。
マークドテキストには Viterbi #1（変換結果）をリアルタイム表示し、Space / ↑↓ で他の候補に切り替える。

#### Predictive モード

`lex_generate_prediction_candidates` を使用。Viterbi N-best をベースに、学習バイグラムを連鎖させた予測候補を生成する:

1. Viterbi N-best で変換候補を取得
2. 各候補の末尾セグメントから `bigram_successors` でバイグラム後続を探索
3. サイクル検出（`HashSet` で訪問済みサーフェスを追跡）付きで最大チェーン長まで連鎖
4. 重複排除後に統合

非同期候補生成（`defer_candidates`）と組み合わせて使用する（詳細は後述）。

### 変換モード

`ConversionMode` enum で Standard / Predictive を切り替える。

| | Standard | Predictive |
|---|---|---|
| 候補生成 | `lex_generate_candidates` | `lex_generate_prediction_candidates` |
| Tab の動作 | サブモード切替（japanese ↔ english） | 確定 |
| 自動確定 | 有効 | 無効 |
| `candidate_dispatch` | `0` | `1` |

- Option+Tab で切替（composing 中は現在の変換を確定してから切替）
- UserDefaults `predictiveMode` で永続化

## 変換パイプライン

```
ローマ字入力
  → ひらがな (engine/romaji)
  → 統一候補生成 (engine/candidates)
    → ラティス構築 (common_prefix_search + 1文字フォールバック)
    → Viterbi N-best 探索
    → Reranker (structure cost + 学習ブースト)
    → 文節グルーピング (自立語 + 付属語)
    → 予測候補 + 辞書 lookup の統合・重複排除
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

### Deferred モード

`defer_candidates` 有効時は、候補生成を非同期に行い、メインスレッドのキー入力処理をブロックしない。provisional candidates（暫定候補）を表示し、非同期結果が到着次第更新する。

## 非同期候補生成

`defer_candidates` モードでは、候補生成をバックグラウンドで実行する。

### アーキテクチャ

1. キー入力 → セッションが `AsyncCandidateRequest { reading, candidate_dispatch }` を返す
2. Swift 側の `candidateQueue`（DispatchQueue）でバックグラウンド生成を実行
3. `candidateGeneration` カウンタ（UInt64）でリクエストの鮮度を管理
4. 生成完了後、`candidateGeneration` が一致する場合のみ `lex_session_receive_candidates` でメインスレッドに配信

### candidate_dispatch

| 値 | モード | 使用する FFI |
|---|---|---|
| `0` | Standard | `lex_generate_candidates` |
| `1` | Predictive | `lex_generate_prediction_candidates` |

stale な候補（生成開始時と完了時で `candidateGeneration` が異なる）は破棄される。

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

### バイグラム後続探索

`bigram_successors(prev_surface)` は、指定サーフェスに続くバイグラムエントリを検索し、`(reading, surface, boost)` のリストをブースト降順で返す。Predictive モードの bigram chaining で使用される。

### 保存

- **形式**: LXUD（マジック `LXUD` + version 1 + bincode）
- **場所**: `~/Library/Application Support/Lexime/user_history.lxud`
- **書き込み**: アトミック（`.tmp` に書いてリネーム）
- **タイミング**: 確定時に記録（同期）、ファイル保存はバックグラウンドキュー

### 退避

容量超過時、`frequency × decay(last_used)` のスコアが低いエントリから削除。

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
- ローカル保存（LXUD 形式、アトミック書き込み）

### Phase 4: Speed of Thought

思考の速度で日本語を書ける開発者向け IME を目指す。

**1発目精度の向上** — **完了**

- 学習収束の高速化（`BOOST_PER_USE` を 3000 に引き上げ、frequency 5 で最大ブースト到達）
- バイグラム活用の強化（直前の文脈を変換精度に反映）

**リアルタイム変換表示 + 句読点自動確定** — **完了**

- マークドテキストに Viterbi #1 をリアルタイム表示（かなではなく変換結果）
- 句読点入力で直前の変換を自動コミット＋句読点を直接挿入
- Enter で index 0（Viterbi #1）も学習付きで確定
- Escape はひらがなで確定（IMKit の制約: Escape 後に `commitComposition` が呼ばれる）
- `currentDisplay` トラッキングで `composedString` とマークドテキストを同期

**Tab インライン英字** — **完了**

- Tab キーで japanese / english サブモードをトグル
- english モード中はローマ字変換をバイパスし、入力をそのまま composedKana に追加（大文字小文字保持）
- programmerMode 時は日英境界に自動スペース挿入（未使用時は再トグルで取消）
  - 例: `今日 React のコンポーネントを commit した`
- english モードはマークドテキストに点線下線（patternDash）で表示
- 自動確定は連続 ASCII セグメントを単語単位でまとめて確定

**候補パネルのカーソル追従** — **完了**

- 候補パネルをマークドテキスト末尾（入力カーソル位置）に追従させる
- composedKana を長く保持する方針と整合させ、視線移動を最小化

**Predictive モード** — **完了**

- Viterbi base + bigram chaining による予測変換
- `ConversionMode` enum（Standard / Predictive）で切替可能
- Option+Tab で変換モードをトグル（UserDefaults `predictiveMode` で永続化）
- Tab キーで予測候補を確定（Standard モードのサブモード切替と差別化）

### Phase 5+ (今後)

- ユーザー辞書
- 設定 UI
- ゴーストテキスト: GGUF ニューラルモデル（azooKey/Zenzai 方式）による AI 予測候補を薄く表示し Tab で受け入れ（Copilot 的 UX）。長文（3 文節〜）で Viterbi N-best をニューラルリスコアする方向

## ビルド・CI

### mise.toml タスク

| タスク | 内容 |
|---|---|
| `engine-lib` | universal static library ビルド（x86_64 + aarch64、lipo） |
| `fetch-dict-sudachi` | SudachiDict データのダウンロード |
| `fetch-dict-sudachi-full` | SudachiDict Full データのダウンロード（core + notcore） |
| `fetch-dict-mozc` | Mozc 辞書データのダウンロード |
| `dict-sudachi-full` | SudachiDict Full 辞書のコンパイル（Mozc POS ID にリマップ） |
| `dict-mozc` | Mozc 辞書バイナリのコンパイル |
| `dict` | Mozc 辞書のコピー（`lexime-mozc.dict` → `lexime.dict`） |
| `conn` | Mozc 接続行列のコンパイル |
| `build` | Lexime.app ユニバーサルバイナリのビルド（depends: dict, conn） |
| `install` | `~/Library/Input Methods` へコピー |
| `reload` | Lexime プロセスを再起動 |
| `log` | ログストリーミング |
| `icon` | アイコンアセット生成 |
| `test-swift` | Swift FFI ラウンドトリップテスト（depends: engine-lib） |
| `lint` | `cargo fmt --check` + `cargo clippy` |
| `test` | lint + `cargo test` |
| `clean` | ビルド成果物の削除 |
| `explain` | 変換パイプラインの説明出力（指定リーディングのラティス・Viterbi 過程を表示） |
| `snapshot` | 変換スナップショット生成（テストリーディング一覧の変換結果を記録） |
| `diff-snapshot` | 現在の変換結果とベースラインスナップショットの差分比較 |
| `trace-log` | トレース JSONL 出力のストリーミング |

### CI

`.github/workflows/ci.yml`:

- **トリガー**: pull_request
- **パスフィルタ**: `engine/**` 変更時のみ Rust CI、`engine/**` かつ `Sources/**`/`Tests/**` 両方変更時のみ Swift CI を実行
- **engine ジョブ** (ubuntu-latest): `mise run test`（lint + cargo test）
- **swift ジョブ** (macos-latest): `mise run test-swift`（engine-lib ビルド + FFI テスト）。macOS クレジット節約のため両方変更時のみ実行

## 未決事項

- リリースワークフロー（パブリック化後のタグプッシュによる自動ビルド）
