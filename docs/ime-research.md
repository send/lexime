# IME Conversion Algorithm Research (2026-02-11)

## Mozc Key Constants
- `kMaxCost = 32767` (unknown 1-char fallback)
- `kDefaultNumberCost = 3000` (single digit)
- `kStructureCostOffset = 3453` (≈1000:1 prob ratio, for filtering fragmented candidates)
- `kWeakConnectedPenalty = 3453` (boundary mismatch)
- `kCostOffset = 6907` (≈1:1M ratio, candidate filter)
- Cost formula: `-500 * log(prob)`
- Connection matrix: sparse row storage with per-row defaults, ~3000 lexicalized POS classes
- BOS/EOS: POS ID 0, no special-casing

## Mozc Architecture (vs Lexime)
- Viterbi → POS-based Segmenter → Rewriter pipeline (20+)
- structure_cost: accumulated transition costs within a segment (separate from total cost)
- N-best via A* search (not just 1-best)
- Constrained Viterbi for user-adjusted boundaries
- No segment penalty or script heuristics in core Viterbi

## Historical IME Approaches
- Left longest match → 2-bunsetsu longest (ATOK) → min cost (MS-IME/Kotoeri) → probabilistic (Anthy) → neural (azooKey)
- Anthy lesson: "poor conversion not from dictionary but algorithm design" (original developer)

## Modern Directions
- azooKey/Zenzai: N-gram + GGUF neural (~70MB), Swift macOS IME. 投機的デコーディング (Viterbi draft → neural verify)
- karukan: 純ニューラル (Viterbi なし), jinen-v1 GPT-2 (26M/90M GGUF), Adaptive Strategy, Linux fcitx5. 詳細は下の karukan 分析セクション
- MS-IME: 100-best Viterbi + reranking
- Discriminative training (SSVM): 1-4% improvement over generative
- libkkc: trigram ARPA LM (more context than bigram)

## Sources
- Mozc: github.com/google/mozc, converter/ directory
- azooKey: github.com/azooKey/azooKey-Desktop
- vibrato: github.com/daac-tools/vibrato (Rust Viterbi reference)
- Papers: ACL W11-3502, ANLP 2011 C4-3
- Tutorial: phontron.com/slides/nlp-programming-en-bonus-01-kkc.pdf

---

# Lexime vs Mozc 類似性分析 (2026-02-12)

## 結論: コード類似性はほぼゼロ、データ依存が最大の結合点

### IME 標準技法 (Mozc 固有ではない)
以下は MeCab/JUMAN/ATOK/macOS IME など全形態素解析ベース IME に共通:
- Viterbi + ラティス構築
- 接続行列 (left_id × right_id → cost)
- 辞書 (reading → surface + cost + POS ID)
- BOS/EOS ノード
- 未知語フォールバック (1-char ノード)
- ユーザー履歴 (unigram/bigram)

### Mozc 固有の類似点
- **辞書データ**: Mozc の TSV を直接使用 (BSD-3 ライセンスで再利用許可)
- **接続行列**: connection_single_column.txt を使用
- **POS ID 体系**: Mozc の id.def に依存
- **structure_cost**: reranker で Mozc に着想を得た遷移コスト集約 (ただし適用方法は異なる)

### Lexime の独自設計
| 項目 | Mozc | Lexime |
|------|------|--------|
| N-best | A* 逆方向探索 (nbest_generator.cc) | Top-K Viterbi (前向きパスで K 個保持) |
| スクリプトコスト | Viterbi 外 (rewriter) | Viterbi 内 (word_cost に統合) |
| リライタ | 25+ 個のパイプライン | 2 特徴量の簡易 reranker |
| 文節分割 | POS ベース segmenter.def | なし (Viterbi 出力をそのまま) |
| 学習注入点 | post-Viterbi (rewriter rerank) | Viterbi 内 (LearnedCostFunction で cost 減算) |
| 辞書形式 | LOUDS trie + succinct bit vector (10.4B/word) | trie_rs + bincode (~50MB) |
| アーキテクチャ | クライアント-サーバ (IPC, C++) | スタティックライブラリ (FFI, Rust+Swift) |
| 辞書ソース | Mozc 独自のみ | Mozc + SudachiDict マージ |
| 長さ分散ペナルティ | なし | 独自 (LENGTH_VARIANCE_WEIGHT) |
| セグメント学習 | UserBoundaryHistoryRewriter (境界位置) | N-best パスからサブフレーズ unigram/bigram |

### Mozc の真の差別化要素 (Lexime にないもの)
1. 25+ rewriter パイプライン (数値変換、絵文字、計算機、コロケーション等)
2. A* ベース N-best (Viterbi 前向きコストをヒューリスティックに利用)
3. POS ベース文節分割器 (segmenter.def のルールテーブル)
4. LOUDS 圧縮辞書 (1.3M語で13.3MB)
5. CachingConnector (遷移コストのアトミックハッシュキャッシュ)
6. CandidateFilter (structure_cost による候補抑制)
7. AES256 暗号化ユーザー履歴

### 差別化の方向性メモ
- Lexime 独自路線: 学習を Viterbi に直接注入、SudachiDict マージ、簡易 reranker
- 今後の差別化候補: n-gram LM、ニューラルリランキング、独自コーパスからの辞書拡張

---

# karukan 技術分析 (2026-02-20)

github.com/togatoga/karukan — Linux (fcitx5) 向け日本語 IME、Rust 製、2026-02 v0.1.0

## アーキテクチャ: 純ニューラル（Viterbi なし）

- **Viterbi ラティスを一切使わない**。GPT-2 ベース LM で直接変換候補を生成
- 辞書は候補補完用（SudachiDict ベース Double-Array Trie, yada クレート）
- Zenzai の jinen format 踏襲: `U+EE02{context}U+EE00{katakana}U+EE01{output}`
  - U+EE02 はコンテキストトークン（karukan 独自拡張、Zenzai は U+EE00/U+EE01 の 2 トークン）

## Zenzai との違い

| | Zenzai (azooKey) | karukan |
|---|---|---|
| 変換 | Viterbi ドラフト → ニューラル検証（投機的デコーディング） | 純ニューラル生成、辞書は補助 |
| モデル | zenz-v3 (90M) | jinen-v1 (独自訓練 GPT-2, 26M/90M) |
| 推論 | greedy + 制約充足 | greedy / beam / depth-1 beam+greedy batch |

## モデル: jinen-v1

- GPT-2 ベース、GGUF フォーマット、Q5_K_M 量子化
- xsmall: 26M (31MB), small: 90M (88MB)
- おそらく ku-nlp/gpt2-small-japanese-char をファインチューン
- 外部 HuggingFace tokenizer.json（llama.cpp 内蔵トークナイザー不使用）
- CPU 推論のみ、コンテキスト長 256

## 速さの秘密: Adaptive Strategy

```
MainModelOnly  → 90M greedy
LightModelOnly → 26M greedy/beam
ParallelBeam   → 90M + 26M 並列 (thread::scope)
```

- 実測レイテンシでモデルを動的切替（閾値以下→Main、超→Light）
- Depth-1 Beam + Greedy Batch: 1st トークンだけ beam で top-k → 残り独立 greedy、KV キャッシュ共有

## その他

- 候補優先順: LearningCache > UserDict > Neural > SystemDict > ひらがな/カタカナ
- LearningCache: HashMap + TSV、`recency*10 + freq.ln_1p()` スコア、prefix lookup 対応
- NllScorer: 候補 NLL を文字数（トークン数ではなく）で正規化して再ランキング
- ライブ変換: preedit にニューラル結果をリアルタイム表示

## Lexime への示唆

- Adaptive Strategy（レイテンシベースのモデル切替）は将来のニューラル統合時に参考
- NllScorer の文字数正規化は N-best + ニューラルリランキングに有用
- U+EE02 コンテキストトークンで前文脈を渡す仕組み
- Viterbi なし全振りは大胆だが、辞書ベースの安定性では Lexime のアプローチが堅実

---

# Lexime ロードマップ (2026-02-12, updated 2026-03-03)

## 大前提: レイテンシ最優先
短文変換で体感遅延は許容しない。速度を犠牲にする改善は長文限定で検討。

## Phase 1: Rewriter パイプライン ✅ 完了
- NumericRewriter: ひらがな数字→漢数字/半角/全角 (最大 ~10^16)
- KatakanaRewriter: 全文カタカナ候補
- HiraganaVariantRewriter: 漢字セグメント→ひらがな置換
- PartialHiraganaRewriter: Top-5 パスの個別セグメント単位でひらがな置換
- KanjiVariantRewriter: ひらがなセグメント(2文字)→漢字代替案
- run_rewriters() で順次実行、重複排除

## Phase 1.5: 学習を reranker に移動 ✅ 完了
- Viterbi は `DefaultCostFunction` (辞書+接続行列のみ、boost なし)
- 学習は `history_rerank()` で N-best パスに post-hoc 適用
- `LearnedCostFunction` は削除済み

## Phase 2: POS 文節分割 + structure_cost フィルタ ✅ ほぼ完了
- structure_cost: 遷移コスト集約 + ハードフィルタ + ソフトペナルティ (Mozc インスパイア)
- group_segments(): POS role ベースの形態素→句グループ化 (接尾辞/関数語マージ、接頭辞処理)
- resegment(): ラティスノードを使った代替分割案生成 (最大10パス)
- per-segment penalties: 非自立漢字、代名詞ボーナス、て形漢字、人名、単漢字内容語
- length_variance: 3セグメント以上のパスで不均等分割にペナルティ
- 残: POS ルールテーブルによる分割点補正 (segmenter.def 相当) は未実装

## Phase 2.5: スニペット機能 ✅ 完了
- SnippetStore: HashMap ベース、prefix_search、TOML 設定
- VariableResolver: $varname / ${varname} 展開、未定義変数検証

## Phase 3: 辞書サブプロジェクト分離 + IT/業務辞書
- engine/ と dict/ を分離。関心事が異なる:
  - engine = アルゴリズム (Viterbi, reranker, FFI)
  - dict = データパイプライン (ソース取得, マージ, コスト推定)
- dictool を dict/ 側に移動
- engine は辞書バイナリの読み込みだけ持つ
- コーパスベースのコスト再推定はここで
- **IT/業務辞書の構築**: Mozc 辞書に欠けている語彙を体系的に収集・補完
  - 動機: 「突合」(とつごう) のような IT/業務用語が Mozc にない。場当たり的なユーザー辞書登録では限界がある
  - 候補ソース: IT 用語集、ビジネス文書コーパス、Wikipedia 技術記事、既存 OSS 辞書 (mozcdic-ut 等)
  - フォーマット: Mozc TSV 互換 → 既存の dictool compile パイプラインに乗せる
  - POS ID・コストの推定方法を要検討（既存 Mozc エントリの類似語から推定 etc.）

## Phase 4: 長文向けニューラルリランキング (条件付き発動)
- 短文: Viterbi のみ (即応答)
- 長文 (3文節〜): Viterbi N-best → GGUF モデルでリスコア
- 長文ほど bigram では文脈不足、ニューラルの恩恵大
- 50-100ms の追加レイテンシは長文なら許容範囲
- 参考: azooKey/Zenzai (~70MB GGUF, Swift macOS IME で実績)
- 参考: karukan の Adaptive Strategy (レイテンシベースのモデル切替)

## 学習強化の方向性 (Phase 1.5 の土台の上で)
- **訂正学習**: 候補変更 = 元候補への負のフィードバック
- **文脈拡張**: bigram → trigram 履歴 (直前2語で予測精度向上)
- **境界学習**: ユーザーの文節手動調整位置を記憶
- **個人辞書**: 辞書にない語の自動登録
- **アプリ別学習**: コードエディタ vs チャットで語彙切替 (高コスト、将来)
