# lemma 設計書

## 概要

lemma は lexime 向けの汎用 Double-Array Trie ライブラリ。
`trie-rs` + `bincode` を置き換え、辞書とローマ字の両方の Trie を統一的に扱う。

## 動機

現在の `TrieDictionary` は `trie-rs::map::Trie<u8, Vec<DictEntry>>` を bincode でシリアライズしている。

| 項目 | 現状 | lemma 導入後 |
|------|------|-------------|
| 辞書ファイルサイズ | ~49MB (bincode) | 実測で確認 |
| ロード時間 | 数百ms (bincode deserialize) | ~5ms (memcpy) |
| ノード表現 | trie-rs 内部構造 (不透明) | `#[repr(C)]` 8B/node |
| 値の格納 | Trie 内部に `Vec<DictEntry>` を保持 | 外部配列 (value_id で参照) |
| ラベル方式 | byte-wise (UTF-8 バイト単位) | **char-wise** (文字単位) |
| 依存クレート | trie-rs, serde, bincode | なし (zero deps) |

char-wise により日本語の common_prefix_search が byte-wise 比で
**1.5-2x 高速** (crawdad ベンチマーク実証済み)。

ローマ字 Trie (`RomajiTrie`) も現在は `HashMap<u8, Node>` ベースだが、
lemma の `DoubleArray<u8>` で置き換えることで統一できる。

## 先行実装

| クレート | ラベル | ノードサイズ | predictive_search | 備考 |
|---------|--------|------------|-------------------|------|
| yada | byte-wise | 8B | なし | darts-clone Rust 移植 |
| crawdad | char-wise | 8B | なし | vibrato (MeCab 2x 速) で採用 |
| trie-rs | byte-wise | LOUDS | あり | 現在 lexime が使用 |
| **lemma** | **char-wise** | **8B (+4B sibling)** | **あり** | crawdad の手法 + predictive_search |

crawdad ベンチマーク (ipadic-neologd, 5.5M keys):

| 操作 | crawdad (char-wise) | yada (byte-wise) | 差 |
|------|-------------------|-----------------|-----|
| exact_match | 9-28 ns | 22-97 ns | 2-3x 速い |
| common_prefix_search | 2.0-2.6 us/line | 3.7-5.3 us/line | 1.5-2x 速い |
| ビルド時間 | 1.93 sec | 34.74 sec | 18x 速い |
| メモリ | 121 MiB | 153 MiB | 20% 小さい |

lemma は crawdad の char-wise + CodeMapper アプローチを採用しつつ、
crawdad にない **predictive_search** (sibling chain) と **probe** を追加する。

## データ構造

### Node

```rust
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Node {
    /// BASE — XOR ベースの子ノードオフセット (31 bit) | IS_LEAF (1 bit)
    base: u32,
    /// CHECK — 親ノードのインデックス (31 bit) | flag (1 bit)
    check: u32,
}
```

- **8 bytes/node**。キャッシュライン (64B) に 8 ノード収まる
- ノード `n` のラベル `c` の子: `index = base(n) XOR code_map(c)`、`check(index) == n` で検証
- IS_LEAF: base の最上位ビット。立っているとき base の残り 31 bit が value_id
- 子ノード探索は O(1): `base XOR label` で直接インデックス計算

### Sibling 配列 (並列・SoA レイアウト)

```rust
siblings: Vec<u32>   // nodes と同じ長さの並列配列
```

- `siblings[i]` — ノード `i` と同じ親を持つ次の兄弟ノードのインデックス (0 = なし)
- **Node 構造体に含めない** — Structure of Arrays (SoA) レイアウト
- `common_prefix_search` / `exact_match` / `probe` は `nodes` のみアクセス (**8B/node**)
- `predictive_search` のみ `siblings` も参照 (実効 12B/node)

| 操作 | アクセスする配列 | 実効ノードサイズ |
|------|-----------------|----------------|
| `exact_match` | `nodes` のみ | **8B** |
| `common_prefix_search` | `nodes` のみ | **8B** |
| `probe` | `nodes` のみ | **8B** |
| `predictive_search` | `nodes` + `siblings` | 12B |

### CodeMapper (頻度順ラベル再マッピング)

char-wise Double-Array では Unicode 文字をそのままラベルに使うと配列が疎になる。
**CodeMapper** で文字を頻度順の連番にリマップし、密な配列を維持する。

```rust
pub struct CodeMapper {
    /// char → remapped code (0 = 未登録)
    table: Vec<u32>,
}
```

- ビルド時に全キーの文字頻度を集計 → 高頻度文字ほど小さい code を割り当て
- 例: ひらがな ~80 種 + カタカナ ~80 種 + 漢字 ~3000 種 → 実効 ALPHABET_SIZE ≈ 4000
- code 0 はターミナルシンボル用に予約
- crawdad の Mapped scheme (Kanda et al. 2023) と同一手法
- `DoubleArray<u8>` (ローマ字 Trie) では CodeMapper は identity 変換（リマップ不要）

### 値の格納 (ターミナルシンボル方式)

Trie は値そのものを持たない。**ターミナルシンボル (code = 0)** を使って値を格納する。

キー "きょう" を value_id=42 で登録するとき、
内部的には `[code('き'), code('ょ'), code('う'), 0]` を挿入する。
ターミナルノードの BASE フィールドに value_id を格納する。

```
通常ノード:
  base  = XOR オフセット (31 bit) | IS_LEAF=0
  check = 親ノードインデックス (31 bit)

ターミナルノード (IS_LEAF = 1):
  value_id = base & 0x7FFF_FFFF  — 31 bit, 最大 ~2G 個
  check    = 親ノードインデックス
```

この方式により **値を持ちつつ子も持つノード** (ExactAndPrefix) を自然に表現できる。
例えばローマ字 Trie で "n" → "ん" かつ "na" → "な" の場合:

```
root --'n'--> N --[0]--> [value_id for "ん"]   (Exact)
                  --'a'--> A --[0]--> [value_id for "な"]
```

ノード N は子 (terminal, 'a') を持つので BASE は子配列を指し、
value_id はターミナル子ノードに格納される。ビット分割の競合が発生しない。

**容量**: value_id は 31 bit で最大 ~2G 値。十分。

**サイズオーバーヘッド**: 各 value 付きキーにターミナルノード 8 bytes が追加される。
ビルド後に実測してサイズ推定を更新する。

lexime での対応:

| 用途 | キー型 | value_id の指す先 |
|------|--------|------------------|
| 辞書 | `&str` (reading のひらがな) | オフセットテーブル経由で `&[DictEntry]` スライスを参照 |
| ローマ字 | `&[u8]` (ASCII romaji) | かな文字列テーブルのインデックス |

## API

### Label trait

```rust
pub trait Label: Copy + Ord + Into<u32> + TryFrom<u32> {
    /// ラベルの最大値 + 1 (配列確保に使用)
    const ALPHABET_SIZE: u32;
}

impl Label for u8 {
    const ALPHABET_SIZE: u32 = 256;
}

impl Label for char {
    const ALPHABET_SIZE: u32 = 0x11_0000;
}
```

辞書 Trie は `DoubleArray<char>` + CodeMapper、ローマ字 Trie は `DoubleArray<u8>` を使用。
CodeMapper によりラベル空間は実効 ~4000 に圧縮されるため、
`char::ALPHABET_SIZE` の大きさは配列サイズに影響しない。

### DoubleArray

```rust
pub struct DoubleArray<L: Label> {
    nodes: Vec<Node>,
    siblings: Vec<u32>,       // 並列配列 (predictive_search 用)
    code_map: CodeMapper,     // ラベル → 内部コード変換
    _phantom: PhantomData<L>,
}
```

### ビルド

```rust
impl<L: Label> DoubleArray<L> {
    /// ソート済みキーから構築する。
    /// 各キーに 0-indexed の value_id が自動付与される。
    ///
    /// # Panics
    /// - キーがソートされていない場合
    pub fn build(keys: &[impl AsRef<[L]>]) -> Self;
}
```

- 入力: ソート済みキー配列。`keys[i]` の value_id は `i`
- ビルド手順:
  1. 全キーの文字頻度を集計 → CodeMapper 構築
  2. キーをリマップ済みコード列に変換 + ターミナルシンボル付与
  3. Doubly-linked free list で BASE を貪欲に配置
  4. Sibling chain を構築
- ビルドは辞書コンパイル時 (`dictool compile`) に 1 回だけ実行

### 検索操作

```rust
impl<L: Label> DoubleArray<L> {
    /// 完全一致検索。キーが存在すれば value_id を返す。
    pub fn exact_match(&self, key: &[L]) -> Option<u32>;

    /// 共通接頭辞検索。query の各接頭辞に一致するキーを返す。
    /// ラティス構築 (Viterbi) で使用。
    pub fn common_prefix_search<'a>(&'a self, query: &'a [L])
        -> impl Iterator<Item = PrefixMatch> + 'a;

    /// 予測検索。prefix で始まる全キーを sibling chain による DFS で返す。
    /// 辞書の predict / predict_ranked で使用。
    pub fn predictive_search<'a>(&'a self, prefix: &'a [L])
        -> impl Iterator<Item = SearchMatch> + 'a;

    /// ノード探査。キーを辿り、値の有無と子の有無を返す。
    /// ローマ字 Trie の lookup (None/Prefix/Exact/ExactAndPrefix) で使用。
    ///
    /// ターミナルシンボル方式により O(1) で判定可能:
    /// 1. キーを辿って到達失敗 → None
    /// 2. ノード N に到達 → base(N) XOR 0 でターミナル子を確認
    ///    - ターミナル子あり → value = Some(value_id),
    ///      has_children = (siblings[terminal] != 0)
    ///    - ターミナル子なし → value = None, has_children = true
    ///      (N が存在する以上、子経由で到達するキーが必ず存在)
    pub fn probe(&self, key: &[L]) -> ProbeResult;
}

pub struct PrefixMatch {
    pub len: usize,      // 一致した接頭辞の長さ
    pub value_id: u32,
}

pub struct SearchMatch {
    pub key: Vec<L>,     // 一致したキー全体 (DFS 中に構築、マッチごとにアロケーション)
    pub value_id: u32,
}

pub struct ProbeResult {
    pub value: Option<u32>,  // 値があれば value_id
    pub has_children: bool,  // 子ノードが存在するか (ターミナル子を除く)
}
```

### シリアライズ

```rust
impl<L: Label> DoubleArray<L> {
    /// 内部データの生バイト表現を返す。
    pub fn as_bytes(&self) -> Vec<u8>;

    /// 生バイト列から DoubleArray を復元する (コピー)。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LemmaError>;
}
```

- lemma 独自ヘッダ: magic + version + 各セクションの長さ
- セクション: `nodes`, `siblings`, `code_map` の 3 つ
- バイト列は `#[repr(C)]` の生データ
- コピーロード: ~5ms。アプリ起動時 1 回のみ
- 内部の検索ロジックは `&[Node]` に対して実装するため、
  将来 zero-copy (`DoubleArrayRef<'a>`) の追加は後方互換で可能
- **エンディアン**: `#[repr(C)]` はバイトオーダーがプラットフォーム依存。
  lexime は同一マシンでビルド・ロードするため問題ないが、
  将来クロスコンパイルが必要になった場合は little-endian で正規化する

## lexime との統合

### 辞書ファイルフォーマット (LXDX v2)

```
Offset      Size  内容
──────────  ────  ──────────────────────────
0           4     magic: "LXDX"
4           1     version: 2
5           4     nodes_len: u32
9           4     siblings_len: u32
13          4     code_map_len: u32
17          4     offsets_len: u32
21          4     entries_len: u32
25          N     [Node; K]              ← lemma: base+check
25+N        S     [u32; K]               ← lemma: siblings
25+N+S      C     CodeMapper             ← lemma: ラベル変換テーブル
25+N+S+C    O     [u32; V+1]             ← オフセットテーブル
25+N+S+C+O  E     [FlatDictEntry; M]     ← lexime: エントリ本体
```

- `FlatDictEntry`: `DictEntry` から `String` を排除したフラット表現
  (surface は別途文字列テーブルに配置し、オフセットで参照)
- **オフセットテーブル**: 1 つの reading が複数の DictEntry を持つ場合のマッピング。
  value_id `i` に対応するエントリは `entries[offsets[i]..offsets[i+1]]`

### TrieDictionary の置き換え

| 現在の API | lemma 導入後 |
|-----------|-------------|
| `Trie<u8, Vec<DictEntry>>` | `DoubleArray<char>` + `Vec<DictEntry>` |
| `trie.exact_match(key)` → `Option<&Vec<DictEntry>>` | `da.exact_match(key)` → `Option<u32>` → `entries[range]` |
| `trie.common_prefix_search(query)` → iter | `da.common_prefix_search(query)` → iter |
| `trie.predictive_search(prefix)` → iter | `da.predictive_search(prefix)` → iter |
| `bincode::serialize/deserialize` | `as_bytes()` / `from_bytes()` |

`Dictionary` trait の実装は変わらない。内部のデータ構造だけが置き換わる。

### RomajiTrie の置き換え

| 現在 | lemma 導入後 |
|------|-------------|
| `HashMap<u8, Node>` ツリー | `DoubleArray<u8>` |
| `lookup() → TrieLookupResult` | `probe() → ProbeResult` → `TrieLookupResult` に変換 |
| 動的に `insert` | ビルド時に `DoubleArray::build()` で構築 (static) |

```rust
// RomajiTrie::lookup の実装イメージ
pub fn lookup(&self, romaji: &str) -> TrieLookupResult {
    let result = self.da.probe(romaji.as_bytes());
    match (result.value, result.has_children) {
        (None, false) => TrieLookupResult::None,
        (None, true) => TrieLookupResult::Prefix,
        (Some(id), false) => TrieLookupResult::Exact(self.kana[id as usize].clone()),
        (Some(id), true) => TrieLookupResult::ExactAndPrefix(self.kana[id as usize].clone()),
    }
}
```

ローマ字 Trie は ASCII のみなので byte-wise (`DoubleArray<u8>`)。
CodeMapper は identity 変換（リマップ不要）。

## クレート構成

```
lexime/
├── lemma/              ← 新規クレート
│   ├── Cargo.toml      [dependencies] なし
│   └── src/
│       ├── lib.rs       pub mod
│       ├── label.rs     Label trait + u8/char impl
│       ├── node.rs      Node (base + check, 8B)
│       ├── code_map.rs  CodeMapper (頻度順ラベル再マッピング)
│       ├── build.rs     DoubleArray::build() + sibling chain 構築
│       ├── search.rs    exact_match, common_prefix_search, predictive_search, probe
│       └── serial.rs    as_bytes, from_bytes
├── engine/             ← 既存クレート (lemma に依存)
│   └── Cargo.toml      trie-rs, serde, bincode を削除 → lemma を追加
└── Cargo.toml          ← workspace 化
```

## 制約・非目標

- **挿入・削除の動的操作はサポートしない**。ビルド済みの不変 Trie のみ
- **圧縮 (TAIL 圧縮、MpTrie 等) は初期実装に含めない**。必要になったら追加
- **mmap zero-copy は初期実装に含めない**。コピーロード (~5ms) で十分高速。
  内部を `&[Node]` で書いておくことで、後から `DoubleArrayRef<'a>` を追加可能

## 実装順序

1. **Node + Label + CodeMapper** — 基本型の定義とラベル再マッピング
2. **build** — ソート済みキーから Double-Array を構築 (free list + sibling chain)
3. **exact_match** — 最も単純な検索
4. **common_prefix_search** — ラティス構築に必要
5. **predictive_search** — 予測候補に必要 (sibling chain 利用)
6. **probe** — ローマ字 Trie に必要
7. **as_bytes / from_bytes** — シリアライズ
8. **lexime 統合** — TrieDictionary と RomajiTrie の内部を差し替え
