# Lexime 仕様書 (v2.0)

## 概要

Lexime は macOS 向けの軽量・高速な日本語予測入力システム。

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
| `LexUserHistory` | Object | 学習履歴（WAL 付き） |
| `LexUserDictionary` | Object | ユーザー辞書 |
| `LexKeyResponse` | Record | キー入力レスポンス（consumed + events） |
| `LexEvent` | Enum | イベント（下記参照） |
| `LexCandidateResult` | Record | 候補生成結果（surfaces + paths） |
| `LexSegment` | Record | 変換セグメント（reading + surface） |
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
| `SchedulePoll` | ポールタイマー開始要求 |

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
| `trace_init(log_dir)` | 構造化ログ初期化 |

**LexSession メソッド**:

| メソッド | 説明 |
|---|---|
| `handle_key(key_code, text, flags)` | キー入力処理 → `LexKeyResponse` |
| `commit()` | 現在の入力を確定 → `LexKeyResponse` |
| `poll()` | 非同期結果をチェック → `Option<LexKeyResponse>` |
| `is_composing()` | 入力中かどうか |
| `set_defer_candidates(enabled)` | 非同期候補生成の有効化 |
| `set_conversion_mode(mode)` | 変換モード切替（0=Standard, 1=Predictive） |
| `set_abc_passthrough(enabled)` | ABC パススルー設定 |
| `committed_context()` | 確定済みコンテキスト取得 |

## 入力モデル

### 状態遷移

```
idle ──(ローマ字入力/句読点)──→ composing ──(Enter/Escape/Tab)──→ idle
```

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
| Escape | ひらがなで確定（IMKit が commitComposition を呼ぶため） |
| 句読点 | 現在の変換を確定し、句読点を直接挿入 |
| その他の文字 | composedKana に追加（Backspace で削除可能） |

**キーリマップ（settings.toml `[keymap]`）**

| key_code | 通常 | Shift |
|---|---|---|
| 10 | `]` | `}` |
| 93 | `\` | `\|` |

keymap に登録されたキーはリマップ後のテキストとして処理される。
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

候補生成は Rust 側の `AsyncWorker` でバックグラウンド実行する。Swift はポーリングで結果を受け取る。

### アーキテクチャ

1. キー入力 → `LexSession::handle_key()` → セッションが `async_request` を返す
2. `handle_key` 内で自動的に `AsyncWorker` にサブミット
3. レスポンスに `SchedulePoll` イベントを含めて返す
4. Swift 側の 50ms ポールタイマーが `LexSession::poll()` を呼び出し
5. `poll()` が `AsyncWorker` のチャネルから結果を取得し、セッションに配信
6. 結果が stale（generation counter 不一致）なら破棄

### AsyncWorker

| スレッド | 優先度 | 内容 |
|---|---|---|
| Candidate | `.userInitiated` | 候補生成（Standard / Predictive） |

- `AtomicU64` generation counter で staleness を管理
- mpsc チャネルの drain-to-latest で最新リクエストのみ処理
- ポールタイマーは 5 秒アイドルタイムアウトで自動停止

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

`LearningRecord::Committed { reading, surface, segments }` — セッション側で確定時に生成。FFI 層（`LexSession::record_history`）が enum を解釈して `UserHistory::record_at()` を呼び出す。whole-reading + sub-segments の 2 段階記録。

### 保存（WAL + Checkpoint）

- **Checkpoint**: LXUD（マジック `LXUD` + version 1 + bincode）
- **WAL**: `history.lxud.wal`（フレーム形式: length + CRC32 + bincode）
- **場所**: `~/Library/Application Support/Lexime/user_history.lxud`
- **書き込み**: 確定時に WAL append（同期）、閾値到達で background compaction（checkpoint 書き出し + WAL truncate）
- **起動時**: checkpoint ロード → WAL replay → in-memory 復元

### 退避

容量超過時、`frequency × decay(last_used)` のスコアが低いエントリから削除。

## ユーザー辞書

ユーザーが手動登録する単語辞書。`Dictionary` trait を実装し、`CompositeDictionary` のレイヤーとして統合。

- **データ構造**: `RwLock<HashMap<String, Vec<UserEntry>>>`（reading → entries）
- **POS ID**: 1852（名詞,一般）、cost: -1（システム辞書より常に優先）
- **形式**: LXUW（マジック `LXUW` + version 1 + bincode）、アトミック書き込み
- **場所**: `~/Library/Application Support/Lexime/user_dict.lxuw`
- **操作**: `register` / `unregister` は write lock、`Dictionary` trait（lookup / predict 等）は read lock
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
| `dict-mozc` | Mozc 辞書バイナリのコンパイル |
| `dict` | 辞書のコピー |
| `dict-clean` | コンパイル済み辞書の削除（次回ビルドで再コンパイル） |
| `conn` | 接続行列のコンパイル |
| `test-swift` | Swift UniFFI ラウンドトリップテスト |
| `test` | lint + `cargo test --workspace --all-features` |
| `lint` | `cargo fmt --check` + `cargo clippy` |
| `audit` | cargo-audit（脆弱性）+ cargo-machete（未使用 deps） |
| `log` | ログストリーミング |
| `trace-log` | トレース JSONL ストリーミング |
| `icon` | アイコンアセット生成 |
| `clean` | ビルド成果物の削除 |
| `explain` | 変換パイプラインの説明出力 |
| `snapshot` | 変換スナップショット生成 |
| `diff-snapshot` | スナップショット差分比較 |
| `accuracy` | 変換精度テスト（accuracy-corpus.toml） |
| `accuracy-history` | 履歴込み変換精度テスト（accuracy-corpus-history.toml） |
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
| `changes` | ubuntu-latest | 常時 | パスフィルタ検出（core / session / ffi / cli / swift） |
| `lint` | ubuntu-latest | Rust 変更時 | `cargo fmt --check` + `cargo clippy` |
| `test-core` | ubuntu-latest | core 変更時 | `cargo test -p lex-core --features trace,neural` |
| `test-session` | ubuntu-latest | session/core 変更時 | `cargo test -p lex-session --features trace` |
| `test-engine` | ubuntu-latest | core/session/ffi 変更時 | `cargo test -p lex_engine --features trace` |
| `test-cli` | ubuntu-latest | core/cli 変更時 | `cargo test -p lex-cli` |
| `audit` | ubuntu-latest | core 変更時 | `cargo-audit` + `cargo-machete` |
| `swift` | macos-latest | engine + Swift 両方変更時 | `mise run test-swift` |

全 Rust ジョブは `Swatinem/rust-cache@v2` で `shared-key: engine` キャッシュを共有。

## 未決事項

- リリースワークフロー（パブリック化後のタグプッシュによる自動ビルド）
