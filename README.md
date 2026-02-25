<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/icon/export/dark/png/icon_128x128.png">
    <source media="(prefers-color-scheme: light)" srcset="assets/icon/export/light/png/icon_128x128.png">
    <img alt="Lexime" src="assets/icon/export/light/png/icon_128x128.png" width="128">
  </picture>
</p>

# Lexime

Lexime /lɛksɪm/ — 軽量な日本語入力システム（IME）

## 特徴

- Viterbi アルゴリズムによる高精度変換
- ユーザーの入力パターンに適応する学習機能
- ユーザ辞書
- Rust エンジンによる高速な辞書検索・変換処理
- JIS キーボード向けキーリマップ（`settings.toml` でカスタマイズ可能）

## 必要環境

- macOS 13.0 (Ventura) 以降
- [Rust](https://rustup.rs/)（エンジンのビルドに必要）
- [mise](https://mise.jdx.dev/)（タスクランナー）

## インストール

```sh
# 辞書データの取得・コンパイル + アプリのビルド
mise run build

# ~/Library/Input Methods にインストール
mise run install
```

インストール後、以下の手順で入力ソースを追加する:

1. **ログアウト → ログイン**（macOS に入力ソースを認識させる）
2. **システム設定 → キーボード → 入力ソース → 編集** を開く
3. 一覧で日本語・英語**以外**の言語を一度選択してから戻す（macOS の UI キャッシュにより、これをしないと新しい入力ソースが表示されないことがある）
4. **ひらがな (Lexime)**（日本語入力）と **英数 (Lexime)**（英字入力）を追加する

## 使い方

### 基本操作

| キー | 動作 |
|---|---|
| ローマ字入力 | ひらがなに変換 |
| Space | 変換候補を切替 |
| Enter | 変換を確定 |
| Tab | 確定 |
| ↑↓ | 候補を選択 |
| Backspace | 1 文字削除 |
| Fn+Delete | 選択中の候補の学習履歴を削除 |
| Escape | ひらがなで確定 |
| 英数キー | 英数 (Lexime) に切替 |
| かなキー | ひらがな (Lexime) に切替 |

### z-sequences（Mozc 互換）

`z` + キーで特殊文字を入力:

| 入力 | 出力 | 入力 | 出力 |
|---|---|---|---|
| `zh` | ← | `zl` | → |
| `zj` | ↓ | `zk` | ↑ |
| `z.` | … | `z,` | ‥ |
| `z/` | ・ | `z-` | 〜 |
| `z[` | 『 | `z]` | 』 |

### 設定

メニューバーのアイコンを右クリック → **設定...** で設定ウィンドウを開く。

- **ユーザ辞書**: 単語の追加・削除

以下のファイルを直接編集することでカスタマイズも可能:

- `~/Library/Application Support/Lexime/settings.toml` — キーリマップ、変換パラメータ等
- `~/Library/Application Support/Lexime/romaji.toml` — ローマ字テーブル

## 開発

```sh
# Rust エンジンの lint + テスト
mise run test

# Swift テスト
mise run test-swift

# ビルド・インストール・リロード
mise run build && mise run install && mise run reload

# ログ確認
mise run log
```

## ライセンス

MIT License。詳細は [LICENSE](LICENSE) を参照。

### 辞書データ

本プロジェクトは以下のオープンソース辞書データを利用している:

- [Mozc](https://github.com/google/mozc) — BSD 3-Clause License (Google)
