# Lexime

macOS 向けの軽量な日本語予測入力システム（IME）。
[PRIME](http://taiyaki.org/prime/) にインスパイアされた予測変換型の入力体験を提供する。

## 特徴

- リアルタイム予測候補表示
- Viterbi アルゴリズムによる高精度変換
- ユーザーの入力パターンに適応する学習機能
- Rust エンジンによる高速な辞書検索・変換処理
- プログラマモード（JIS キーボードの ¥ キーで `\` を入力）

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

インストール後、ログアウト→ログインし、**システム環境設定 → キーボード → 入力ソース** で Lexime を追加する。

## 使い方

### 基本操作

| キー | 動作 |
|---|---|
| ローマ字入力 | ひらがなに変換、予測候補を表示 |
| Space | 漢字変換 |
| Enter | 確定 |
| Tab | 予測候補を確定 |
| ↑↓ | 候補を選択 |
| F7 | カタカナに変換 |
| Backspace | 1 文字削除 |
| Escape | キャンセル |
| 英数キー | システム ABC に切替 |

### z-sequences（Mozc 互換）

`z` + キーで特殊文字を入力:

| 入力 | 出力 | 入力 | 出力 |
|---|---|---|---|
| `zh` | ← | `zl` | → |
| `zj` | ↓ | `zk` | ↑ |
| `z.` | … | `z,` | ‥ |
| `z/` | ・ | `z-` | 〜 |
| `z[` | 『 | `z]` | 』 |

### プログラマモード

JIS キーボードの ¥ キーでバックスラッシュ `\` を入力するモード。

```sh
# 有効化
defaults write sh.send.inputmethod.Lexime programmerMode -bool true

# 無効化
defaults write sh.send.inputmethod.Lexime programmerMode -bool false
```

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
- [SudachiDict](https://github.com/WorksApplications/SudachiDict) — Apache License 2.0 (Works Applications)
