# lemma 設計書

## 概要

lemma は lexime 向けの汎用 Double-Array Trie ライブラリ。
`trie-rs` + `bincode` を置き換え、辞書とローマ字の両方の Trie を統一的に扱う。

## 動機

現在の `TrieDictionary` は `trie-rs::map::Trie<u8, Vec<DictEntry>>` を bincode でシリアライズしている。

| 項目 | 現状 | lemma 導入後 |
|------|------|-------------|
| 辞書ファイルサイズ | ~49MB (bincode) | ~10-15MB (推定) |
| ロード時間 | 数百ms (bincode deserialize) | ~5ms (memcpy) |
| ノード表現 | trie-rs 内部構造 (不透明) | `#[repr(C)]` 8B/node |
| 値の格納 | Trie 内部に `Vec<DictEntry>` を保持 | 外部配列 (value_id で参照) |
| 依存クレート | trie-rs, serde, bincode | なし (zero deps) |

ローマ字 Trie (`RomajiTrie`) も現在は `HashMap<u8, Node>` ベースだが、
lemma の `DoubleArray<u8>` で置き換えることで統一できる。

## データ構造

### Node

```rust
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Node {
    /// BASE (上位 22 bit) | CHECK (下位 10 bit)
    base_check: u32,
    /// 兄弟ノードのインデックス (0 = 兄弟なし)
    sibling: u32,
}
```

- **8 bytes/node**。キャッシュライン (64B) に 8 ノード収まる
- `base_check` エンコーディング:
  - `BASE = base_check >> 10` — 子ノード配列の開始位置 (最大 4M ノード)
  - `CHECK = base_check & 0x3FF` — 親からの遷移ラベル (最大 1024 種)
  - u8 ラベル (256 種) でも char ラベル (Unicode) でも収まる
- `sibling` — 同じ親を持つ次の兄弟ノードのインデックス
  - predictive_search で子→兄弟の順に DFS するために必要
  - 親ポインタ不要で、ノードサイズを 8B に抑えられる

### 値の格納 (外部方式)

Trie は値そのものを持たない。リーフ（または中間ノード）に **value_id: u32** を埋め込み、
呼び出し側が value_id を外部配列のインデックスとして使う。

value_id の格納方法: BASE が特別な値（例: 最上位ビットが立っている）のとき、
残りのビットが value_id を表す。

```
has_value = (base_check >> 31) & 1
value_id  = (base_check >> 10) & 0x1FFFFF   // 21 bit, 最大 ~2M 個
check     = base_check & 0x3FF
```

lexime での対応:

| 用途 | キー型 | value_id の指す先 |
|------|--------|------------------|
| 辞書 | `&[u8]` (reading の UTF-8) | `&[DictEntry]` スライスのインデックス |
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
```

### DoubleArray

```rust
pub struct DoubleArray<L: Label> {
    nodes: Vec<Node>,
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
    /// キーがソートされていない場合
    pub fn build(keys: &[impl AsRef<[L]>]) -> Self;
}
```

- 入力: ソート済みキー配列。`keys[i]` の value_id は `i`
- アルゴリズム: 幅優先で BASE を貪欲に配置
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

    /// 予測検索。prefix で始まる全キーを DFS 順に返す。
    /// 辞書の predict / predict_ranked で使用。
    pub fn predictive_search<'a>(&'a self, prefix: &'a [L])
        -> impl Iterator<Item = SearchMatch> + 'a;

    /// ノード探査。キーを辿り、値の有無と子の有無を返す。
    /// ローマ字 Trie の lookup (None/Prefix/Exact/ExactAndPrefix) で使用。
    pub fn probe(&self, key: &[L]) -> ProbeResult;
}

pub struct PrefixMatch {
    pub len: usize,      // 一致した接頭辞の長さ
    pub value_id: u32,
}

pub struct SearchMatch {
    pub key: Vec<L>,     // 一致したキー全体
    pub value_id: u32,
}

pub struct ProbeResult {
    pub value: Option<u32>,  // 値があれば value_id
    pub has_children: bool,  // 子ノードが存在するか
}
```

### シリアライズ

```rust
impl<L: Label> DoubleArray<L> {
    /// 内部 Node 配列の生バイト表現を返す。
    /// そのままファイルに書き出せる。
    pub fn as_bytes(&self) -> &[u8];

    /// 生バイト列から DoubleArray を復元する (コピー)。
    /// バイト長が Node サイズの倍数でない場合はエラー。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LemmaError>;
}
```

- **ヘッダなし**。lemma はライブラリであり、ファイルフォーマットではない
- バイト列は `[Node]` の生データそのもの (`#[repr(C)]` なので移植可能)
- コピーロード: 49MB で ~5ms。アプリ起動時 1 回のみ
- 内部の検索ロジックは `&[Node]` に対して実装するため、
  将来 zero-copy (`DoubleArrayRef<'a>`) の追加は後方互換で可能

## lexime との統合

### 辞書ファイルフォーマット (LXDX v2)

```
Offset  Size  内容
──────  ────  ──────────────────────────
0       4     magic: "LXDX"
4       1     version: 2
5       4     trie_len: u32 (Node 配列のバイト数)
9       4     entries_len: u32 (DictEntry 配列のバイト数)
13      ...   [Node; N]           ← lemma が読む
13+T    ...   [FlatDictEntry; M]  ← lexime が読む
```

- `FlatDictEntry`: `DictEntry` から `String` を排除したフラット表現
  (surface は別途文字列テーブルに配置し、オフセットで参照)
- value_id `i` は entries 配列のインデックス範囲に対応

### TrieDictionary の置き換え

| 現在の API | lemma 導入後 |
|-----------|-------------|
| `Trie<u8, Vec<DictEntry>>` | `DoubleArray<u8>` + `Vec<DictEntry>` |
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

## クレート構成

```
lexime/
├── lemma/              ← 新規クレート
│   ├── Cargo.toml      [dependencies] なし
│   └── src/
│       ├── lib.rs       pub mod
│       ├── label.rs     Label trait + u8 impl
│       ├── node.rs      Node, エンコーディング
│       ├── build.rs     DoubleArray::build()
│       ├── search.rs    exact_match, common_prefix_search, predictive_search, probe
│       └── serial.rs    as_bytes, from_bytes
├── engine/             ← 既存クレート (lemma に依存)
│   └── Cargo.toml      trie-rs, serde, bincode を削除 → lemma を追加
└── Cargo.toml          ← workspace 化
```

## 制約・非目標

- **挿入・削除の動的操作はサポートしない**。ビルド済みの不変 Trie のみ
- **圧縮 (DARTS-clone の TAIL 圧縮等) は初期実装に含めない**。必要になったら追加
- **char ラベルは当面不要**。辞書もローマ字も `u8` で十分
  (`Label` trait は将来の拡張ポイントとして残す)
- **mmap zero-copy は初期実装に含めない**。コピーロード (~5ms) で十分高速。
  内部を `&[Node]` で書いておくことで、後から `DoubleArrayRef<'a>` を追加可能

## 実装順序

1. **Node + Label** — 基本型の定義
2. **build** — ソート済みキーから Double-Array を構築
3. **exact_match** — 最も単純な検索
4. **common_prefix_search** — ラティス構築に必要
5. **predictive_search** — 予測候補に必要
6. **probe** — ローマ字 Trie に必要
7. **as_bytes / from_bytes** — シリアライズ
8. **lexime 統合** — TrieDictionary と RomajiTrie の内部を差し替え
