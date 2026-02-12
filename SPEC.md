# Lexime 仕様書 (v1.0)

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
| `InputState.swift` | `InputState` enum（idle / composing） |

### Rust Engine (`engine/src/`)

| モジュール | 内容 |
|---|---|
| `lib.rs` | FFI 関数 (26 関数)。C 互換構造体、メモリ管理（`OwnedVec` パターン） |
| `romaji/` | ローマ字→かな変換。Trie（HashMap ベース）、141+ マッピング、促音・撥音・コラプス |
| `candidates.rs` | 統一候補生成。句読点代替、予測 + Viterbi N-best + 辞書 lookup の統合・重複排除 |
| `dict/` | `Dictionary` trait、`TrieDictionary`（bincode）、`ConnectionMatrix`（LXCX） |
| `dict/source/` | `DictSource` trait、`MozcSource`、`SudachiSource`、`pos_map`（POS ID リマップ） |
| `converter/` | `Lattice` 構築、`Viterbi` N-best 探索、`Reranker`、`CostFunction` trait |
| `user_history/` | ユニグラム・バイグラム学習、LXUD 形式 |
| `bin/dictool.rs` | 辞書操作 CLI（fetch / compile / compile-conn / merge / diff / info） |

### 辞書データ

Mozc 辞書のみを使用。ファイル名は `lexime.dict` / `lexime.conn`。

- **辞書**: Mozc TSV → `TrieDictionary`（bincode シリアライズ、マジック `LXDC`、約 49MB）
- **接続行列**: バイナリ行列（マジック `LXCX`、i16 配列）
- POS ID ペアの遷移コストを O(1) で参照

### FFI (C ABI)

`engine/include/engine.h` で公開する 26 関数:

| カテゴリ | 関数 |
|---|---|
| ユーティリティ | `lex_engine_version`, `lex_engine_echo` |
| ローマ字 | `lex_romaji_lookup`, `lex_romaji_lookup_free`, `lex_romaji_convert`, `lex_romaji_convert_free` |
| 辞書 | `lex_dict_open`, `lex_dict_close`, `lex_dict_lookup`, `lex_dict_predict`, `lex_dict_predict_ranked`, `lex_candidates_free` |
| 接続行列 | `lex_conn_open`, `lex_conn_close` |
| 変換 | `lex_convert`, `lex_conversion_free`, `lex_convert_nbest`, `lex_convert_nbest_with_history`, `lex_conversion_result_list_free` |
| 候補生成 | `lex_generate_candidates`, `lex_candidate_response_free` |
| 学習 | `lex_history_open`, `lex_history_close`, `lex_history_record`, `lex_history_save`, `lex_convert_with_history`, `lex_dict_lookup_with_history` |

メモリ管理: Rust 側が `OwnedVec` / `OwnedCandidateResponse` で文字列を所有し、呼び出し元が `*_free()` で解放する。

## 入力モデル

### 状態遷移

```
idle ──(ローマ字入力/句読点)──→ composing ──(Enter/Tab)──→ idle
```

### 各状態でのキー操作

**idle**

| キー | 動作 |
|---|---|
| ローマ字 | composing へ遷移 |
| 句読点（`,` `.` 等） | 全角句読点で composing へ遷移 |
| 英数キー | システム ABC 入力ソースに切替 |

**composing**

| キー | 動作 |
|---|---|
| ローマ字 | かな追加、候補更新（予測 + Viterbi） |
| z + `h/j/k/l/.,/-/[/]` | Mozc 互換 z-sequence（矢印・記号）を入力 |
| Space / ↓ | 次の候補を選択 |
| ↑ | 前の候補を選択 |
| Enter | 選択中の候補（未選択ならかな）を確定 |
| Tab | カタカナに変換して確定 |
| Backspace | 1 文字削除（空になれば idle へ） |
| Escape | 候補選択を解除 |
| 句読点 | 確定後、句読点入力を開始 |

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

composing 中、キーストロークごとに `lex_generate_candidates` を 1 回呼び出し、以下の候補を engine 内で統合する:

1. **ひらがな** — 元のかな（常に先頭）
2. **予測候補** — `predict_ranked` による prefix search
3. **Viterbi 結果** — N-best 変換（学習ブースト付き）
4. **辞書 lookup** — 全読み候補

重複は engine 内で排除する。句読点入力時は代替候補（`。`→`．`/`.` 等）を生成する。
マークドテキストにはかなを表示し、Space / ↑↓ で候補を選択すると選択中の候補に切り替わる。

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
- **文節グルーピング**: 形態素列を `is_function_word(left_id)` で判定し、自立語 + 付属語のフレーズ単位にマージ

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

## 学習機能

### データ構造

- **ユニグラム**: `reading → surface → HistoryEntry`（最大 10,000 件）
- **バイグラム**: `prev_surface → next_reading → next_surface → HistoryEntry`（最大 10,000 件）
- **HistoryEntry**: `frequency: u32`, `last_used: u64`（Unix epoch）

### ブースト計算

```
boost = min(frequency × 1500, 10000) × decay(last_used)
decay = 1.0 / (1.0 + hours_elapsed / 168.0)
```

- 半減期: 1 週間（168 時間）
- 最大ブースト: 10,000（frequency ≥ 7 で到達）
- Reranker が Viterbi 後のパスに対してブーストを適用し、学習した変換を優先する

### 保存

- **形式**: LXUD（マジック `LXUD` + version 1 + bincode）
- **場所**: `~/Library/Application Support/Lexime/user_history.lxud`
- **書き込み**: アトミック（`.tmp` に書いてリネーム）
- **タイミング**: 確定時に記録（同期）、ファイル保存はバックグラウンドキュー

### 退避

容量超過時、`frequency × decay(last_used)` のスコアが低いエントリから削除。

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

**1発目精度の向上**

- 学習収束の高速化（1-2 回の使用で十分なブースト）
- バイグラム活用の強化（直前の文脈を変換精度に反映）

**リアルタイム変換表示 + 句読点自動確定**

- マークドテキストにかなではなく 1 位変換結果をリアルタイム表示
- 句読点入力で直前の変換を自動コミット（Space/Enter 不要）
- composedKana は長く保持（Viterbi の文脈を最大化）

**Shift インライン英字**

- composing 中に Shift+文字で英字入力を開始（ローマ字変換をバイパス）
- 区切り文字（スペース、句読点等）で日本語モードに復帰
- programmerMode 時は英語と日本語の境界に自動スペース挿入
  - 例: `今日 React のコンポーネントを commit した`

**実験枠**

- ゴーストテキスト: 予測候補を薄く表示し Tab で受け入れ（Copilot 的 UX）

### Phase 5+ (今後)

- ユーザー辞書
- 設定 UI

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

### CI

`.github/workflows/ci.yml`:

- **トリガー**: pull_request
- **パスフィルタ**: `engine/**` 変更時のみ Rust CI、`Sources/**` or `Tests/**` 変更時のみ Swift CI を実行
- **engine ジョブ** (ubuntu-latest): `mise run test`（lint + cargo test）
- **swift ジョブ** (macos-latest): `mise run test-swift`（engine-lib ビルド + FFI テスト）

## 未決事項

- リリースワークフロー（パブリック化後のタグプッシュによる自動ビルド）
