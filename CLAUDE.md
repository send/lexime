# CLAUDE.md

## ビルド・テスト

```bash
# Rust lint + test
cd engine && cargo fmt --all --check && cargo clippy --workspace --all-features -- -D warnings && cargo test --workspace --all-features

# アプリビルド・インストール
mise run build && mise run install && mise run reload
```

## ワークフロー

main に直接コミットしない。必ず以下の流れで作業する:

1. `git checkout -b <type>/<topic>` でブランチを切る
2. 変更をコミットする（Conventional Commits: `feat`, `fix`, `refactor`, `docs`, `chore`）
3. `git push -u origin <branch>` で push する
4. `gh pr create` で PR を作成する
5. `gh pr merge --merge --delete-branch` でマージする

## コミット規約

- Conventional Commits を使用する
- amend + force push しない。レビュー修正は新しいコミットを積む

## 変換精度テスト

`engine/data/accuracy-corpus.toml` に構造化テストケースを管理し、`mise run accuracy` で実行する。

### 運用ルール

- **skip 以外は全 pass を維持する**。fail があれば修正するか skip にする
- **skip には issue リンク必須**（理由なし skip 禁止）
- skip ケースは定期的にレビューし、修正済みなら skip を外す
- **コスト調整・reranker 変更時**: 事前に `mise run accuracy` で現状確認し、PR に before/after の結果を貼る
- **変換精度バグの修正時**: regression カテゴリのケース追加を推奨
- ユーザ報告の変換ミスは積極的に追加。対応困難なものは skip + issue で管理
