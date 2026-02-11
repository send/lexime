# リファクタリング提案

レポジトリ全体のコードレビュー結果に基づくリファクタリング提案です。
優先度（高・中・低）と影響範囲を示しています。

---

## 1. [高] `lib.rs` FFI層の重複パターンの統一 (engine/src/lib.rs)

### 現状の問題
FFI層（1065行）に3つの「Owned パターン」構造体（`CandidateListOwned`、`ConversionResultOwned`、`ConversionResultListOwned`）が存在し、それぞれが同じメモリ管理パターン（`Box::into_raw` → ポインタ公開 → `Box::from_raw` で解放）を繰り返しています。

- `LexCandidateList::pack()` (L154-178)
- `pack_conversion_result()` (L344-384)
- `pack_conversion_result_list()` (L436-459)

### 提案
ジェネリックな `OwnedList<T>` 型を導入し、ポインタ管理を一箇所にまとめる。

```rust
struct OwnedList<T> {
    items: Vec<T>,
    _strings: Vec<CString>,
}

impl<T> OwnedList<T> {
    fn pack(items: Vec<T>, strings: Vec<CString>) -> (*const T, u32, *mut Self) { ... }
}
```

### 期待効果
- unsafe コードのレビュー範囲の削減
- メモリリークバグの発生箇所の限定

---

## 2. [高] FFI関数のボイラープレート削減 (engine/src/lib.rs)

### 現状の問題
各FFI関数で同じ nullチェック → `cptr_to_str` → ポインタ参照のパターンが繰り返されています。例:

```rust
// lex_dict_lookup, lex_dict_predict, lex_dict_predict_ranked,
// lex_convert, lex_convert_nbest, lex_convert_with_history, ...
// 全て同じパターン
if dict.is_null() { return Empty; }
let Some(str) = (unsafe { cptr_to_str(ptr) }) else { return Empty; };
let dict = unsafe { &*dict };
```

### 提案
マクロまたはヘルパー関数で共通パターンを抽出する。

```rust
macro_rules! ffi_guard {
    ($ptr:expr, $default:expr) => {
        if $ptr.is_null() { return $default; }
        unsafe { &*$ptr }
    };
}
```

### 期待効果
- FFI関数の定義が簡潔になる
- nullチェック漏れの防止

---

## 3. [高] `DictError` / `ConnectionError` / `DictSourceError` エラー型の統一

### 現状の問題
3つの異なるエラー型が存在し、それぞれ `Io`、`Parse`、ヘッダー検証などの共通バリアントを持っています:

- `dict::trie_dict::DictError` (L152-160): Io, InvalidHeader, InvalidMagic, UnsupportedVersion, Serialize, Deserialize
- `dict::connection::ConnectionError` (L196-202): Io, InvalidHeader, InvalidMagic, UnsupportedVersion, Parse
- `dict::source::DictSourceError` (L24-29): Io, Parse, Http

### 提案
共通の `EngineError` 列挙型を導入し、`From` トレイトで変換を提供する。あるいは `thiserror` クレートの導入も検討。

### 期待効果
- エラーハンドリングの一貫性向上
- `?` 演算子の利用がスムーズになる

---

## 4. [高] `user_history/mod.rs` のネストされた HashMap の型エイリアス化

### 現状の問題
bigrams のデータ構造が3段ネストの HashMap になっており、型が冗長:

```rust
// L26
bigrams: HashMap<String, HashMap<String, HashMap<String, HistoryEntry>>>
```

これが `bigram_count()` (L98-104)、`evict()` (L322-349)、`to_data()` (L248-263)、`from_data()` (L277-295) の各所で3段のネストイテレーションを強制しています。

### 提案
複合キー `(prev_surface, next_reading, next_surface)` を使った単層 HashMap に変更するか、最低限型エイリアスを導入する:

```rust
type BigramKey = (String, String, String); // (prev_surface, next_reading, next_surface)
type BigramMap = HashMap<BigramKey, HistoryEntry>;
```

### 期待効果
- evict、to_data、from_data のコードが大幅に簡潔になる
- 3段ネストの空エントリ判定（empty inner map の除去）が不要になる

---

## 5. [中] `dict/source/mozc.rs` と `dict/source/sudachi.rs` のパーサー重複

### 現状の問題
両ファイルの `parse_dir` メソッド（mozc.rs:L74-158、sudachi.rs:L121-208）が以下の同一パターンを持ちます:

1. ディレクトリ内のファイル一覧取得 + ソート
2. ファイルが空の場合のエラー
3. `total_lines` / `skipped` カウンタ
4. 行ごとのパース（空行・コメント行のスキップ）
5. フィールド分割 + パース失敗時のスキップ
6. `is_hiragana` チェック

### 提案
共通のイテレータアダプタまたはヘルパー関数を `source/mod.rs` に抽出する:

```rust
fn iter_dict_files(dir: &Path, extension: &str)
    -> Result<Vec<DirEntry>, DictSourceError>

fn parse_lines_with_stats<F>(path: &Path, parser: F)
    -> Result<Vec<DictEntry>, DictSourceError>
```

### 期待効果
- 新しい辞書ソース追加時のボイラープレート削減
- パース統計ログの統一

---

## 6. [中] Swift: `LeximeInputController` のグローバル状態依存

### 現状の問題
`main.swift` でトップレベルに宣言されたグローバル変数:

```swift
let sharedDict: OpaquePointer?       // L24
let sharedConn: OpaquePointer?       // L41
let sharedHistory: OpaquePointer?    // L57
let sharedCandidatePanel = CandidatePanel()  // L73
let userHistoryPath: String          // L52
```

これらが `LeximeInputController`、`DictBridge`、`KeyHandlers` の各所から直接参照されています。

### 提案
`AppContext` 構造体（またはクラス）に集約し、依存関係を明示的にする:

```swift
class AppContext {
    let dict: OpaquePointer?
    let conn: OpaquePointer?
    let history: OpaquePointer?
    let historyPath: String
    let candidatePanel: CandidatePanel
}
```

### 期待効果
- テスタビリティの向上（モック注入が可能に）
- リソースのライフサイクル管理の明確化

---

## 7. [中] Swift: `KeyHandlers.swift` の handleComposing 関数の長さ

### 現状の問題
`handleComposing` メソッド (KeyHandlers.swift:L57-232) が176行あり、以下の責務が混在:
- キーイベントのディスパッチ（switch文）
- 変換候補リストの構築（L111-155、Space キー処理内部）
- 状態遷移の管理

特にSpace キー処理のブロック（L95-156）は62行あり、候補リスト構築ロジックが入り組んでいます。

### 提案
Space キー処理の候補リスト構築部分を `buildConversionCandidates(kana:predictions:) -> [String]` のような独立メソッドに抽出する。

### 期待効果
- handleComposing の見通しが改善
- 候補リスト構築ロジックの単体テストが可能になる

---

## 8. [中] Rust: `trie_dict.rs` の不要な Box ラップ

### 現状の問題
`predict`、`predict_ranked`、`common_prefix_search` の各メソッドで、イテレータを `Box<dyn Iterator>` に変換しています:

```rust
// trie_dict.rs:L88, L131, L142
let iter: Box<dyn Iterator<Item = (String, &Vec<DictEntry>)>> =
    Box::new(self.data.trie.predictive_search(prefix.as_bytes()));
```

これは不要な動的ディスパッチとヒープアロケーションです。

### 提案
`Box` を除去し、`trie-rs` が返す具体型をそのまま使う。型が長い場合は `let` 束縛のみで十分:

```rust
let iter = self.data.trie.predictive_search(prefix.as_bytes());
```

### 期待効果
- 不要なヒープアロケーションの排除
- コードの簡潔化

---

## 9. [中] Rust: `converter/cost.rs` の `is_kanji` / `is_hiragana` / `is_katakana` と `dict/source/mod.rs` の `is_hiragana` の重複

### 現状の問題
Unicode 文字種判定関数が2箇所に存在:
- `converter/cost.rs`: `is_kanji`, `is_hiragana`, `is_katakana`, `is_latin` (L45-61)
- `dict/source/mod.rs`: `is_hiragana` (L44-48)

両者の `is_hiragana` 実装は微妙に異なります:
- `cost.rs` 版: `'\u{3040}'..='\u{309F}'` (全ひらがなブロック)
- `source/mod.rs` 版: 同上 + `'ー'` (長音記号を含む)

### 提案
Unicode 文字判定を共通モジュール（例: `unicode.rs` またはクレートルートの `util` モジュール）に統一する。

### 期待効果
- ひらがな判定の一貫性確保
- 長音記号の扱いの明確化

---

## 10. [低] Swift: `RomajiTable.swift` のデータ駆動化

### 現状の問題
222 件のローマ字→かなマッピングが `init()` メソッド内でハードコードされています（L19-316、298行）。

### 提案
マッピングを `[(String, String)]` 配列として宣言し、ループで `insert` する:

```swift
private static let mappings: [(String, String)] = [
    ("a", "あ"), ("i", "い"), ("u", "う"), ...
]
private init() {
    for (romaji, kana) in Self.mappings { insert(romaji, kana) }
}
```

### 期待効果
- 新しいマッピング追加・削除が容易になる
- テストでマッピング数の検証が可能になる

---

## 11. [低] Swift: `MarkedTextManager.swift` の `updateMarkedText` と `updateMarkedTextWithCandidate` の統合

### 現状の問題
2つのメソッドがほぼ同じ処理（NSAttributedString 生成 → setMarkedText）を行っています:

```swift
func updateMarkedText(client:)              // L6-17: composedKana + pendingRomaji を表示
func updateMarkedTextWithCandidate(_, client:) // L19-28: 候補文字列を表示
```

### 提案
引数を統一し、1つのメソッドに統合する:

```swift
func updateMarkedText(_ text: String, client: IMKTextInput) {
    let len = text.utf16.count
    let attrs: [NSAttributedString.Key: Any] = [.markedClauseSegment: 0]
    let attrStr = NSAttributedString(string: text, attributes: attrs)
    client.setMarkedText(attrStr, selectionRange: ..., replacementRange: ...)
}
```

### 期待効果
- コード重複の除去

---

## 12. [低] Rust: `user_history/mod.rs` の `decay()` 関数のテスタビリティ

### 現状の問題
`decay()` 関数 (L74-78) が内部で `now_epoch()` を呼んでおり、現在時刻に依存するため確定的なテストが困難です。テスト（L452-490）は許容誤差を設けて対処していますが脆弱です。

### 提案
`decay` に `now` パラメータを渡すように変更し、テスト時に固定時刻を指定できるようにする:

```rust
fn decay(last_used: u64, now: u64) -> f64 {
    let hours = (now.saturating_sub(last_used)) as f64 / 3600.0;
    1.0 / (1.0 + hours / HALF_LIFE_HOURS)
}
```

`HistoryEntry::boost()` も `now` を受け取るように変更する。

### 期待効果
- テストの確定性・安定性の向上

---

## 13. [低] `dictool.rs` のサブコマンド引数パースの `clap` 化

### 現状の問題
`dictool.rs` (502行) で手動の引数パース（`parse_source_args`、`parse_merge` など）が行われています。`--source`、`--full`、`--remap-ids`、`--max-cost`、`--max-reading-len` の各フラグを手動で処理しています。

### 提案
`clap` クレート（derive マクロ）の導入を検討する。ただし、依存を増やしたくない場合は現状維持も妥当。

### 期待効果
- ヘルプ自動生成
- 引数バリデーションの自動化
- エラーメッセージの統一

---

## まとめ

| # | 優先度 | 対象 | 概要 |
|---|--------|------|------|
| 1 | 高 | engine/src/lib.rs | Owned パターンのジェネリック化 |
| 2 | 高 | engine/src/lib.rs | FFI null チェックのマクロ化 |
| 3 | 高 | engine/src/dict/ | エラー型の統一 |
| 4 | 高 | engine/src/user_history/ | 3段ネスト HashMap のフラット化 |
| 5 | 中 | engine/src/dict/source/ | Mozc/Sudachi パーサーの共通化 |
| 6 | 中 | Sources/*.swift | グローバル変数の AppContext 化 |
| 7 | 中 | Sources/KeyHandlers.swift | handleComposing の分割 |
| 8 | 中 | engine/src/dict/trie_dict.rs | 不要な Box dyn Iterator の除去 |
| 9 | 中 | engine/ | Unicode 文字判定の統一 |
| 10 | 低 | Sources/RomajiTable.swift | マッピングのデータ駆動化 |
| 11 | 低 | Sources/MarkedTextManager.swift | marked text メソッドの統合 |
| 12 | 低 | engine/src/user_history/ | decay() のテスタビリティ改善 |
| 13 | 低 | engine/src/bin/dictool.rs | clap による引数パース |

**全体的な印象:** コードベースは v0.1.0 としてはよく構造化されており、テストカバレッジも適切です。上記の提案は主にコード重複の削減と保守性の向上に焦点を当てています。機能的なバグやセキュリティ上の問題は発見されませんでした。
