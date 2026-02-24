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
4. `gh pr create` で PR を作成する。未チェックのテストプランがある場合は先に済ますこと
5. コードの変更を含む PR はレビュー対応後にマージする（後述）
6. `gh pr merge --merge --delete-branch` でマージする

### PR レビュー対応フロー

コードの変更を含む PR は以下の手順で対応する（Copilot レビューはユーザーが手動でリクエスト）:

1. **レビューコメント確認**: `gh api repos/{owner}/{repo}/pulls/{number}/reviews` と `gh api repos/{owner}/{repo}/pulls/{number}/comments` でコメントを取得
2. **対応**: 指摘に対し修正コミットを積むか、対応不要ならコメントで理由を返信
3. **resolve**: 対応済みスレッドは `gh api graphql` で resolve する
4. **CI 確認**: `gh pr checks {number}` で全チェック pass を確認
5. **マージ**: CI pass を確認後 `gh pr merge --merge --delete-branch` でマージ。マージ前にユーザーに確認を取る

## コミット規約

- Conventional Commits を使用する
- amend + force push しない。レビュー修正は新しいコミットを積む

## 変換精度テスト

2 つのコーパスで管理し、それぞれ `mise run accuracy` / `mise run accuracy-history` で実行する。

| コーパス | 目的 | コマンド |
|---|---|---|
| `engine/testcorpus/accuracy-corpus.toml` | 辞書 + Viterbi の素の変換品質 | `mise run accuracy` |
| `engine/testcorpus/accuracy-corpus-history.toml` | 学習履歴による改善の検証 | `mise run accuracy-history` |

### 運用ルール

- **skip 以外は全 pass を維持する**。fail があれば修正するか skip にする
- **skip には issue リンク必須**（理由なし skip 禁止）
- skip ケースは定期的にレビューし、修正済みなら skip を外す
- **コスト調整・reranker 変更時**: 事前に両方の accuracy テストで現状確認し、PR に before/after の結果を貼る
- **変換精度バグの修正時**: regression カテゴリのケース追加を推奨
- ユーザ報告の変換ミスは積極的に追加。対応困難なものは skip + issue で管理
- **history コーパスの新規ケースには `baseline`（履歴なしの期待結果）を必ず付ける**
- baseline がずれた場合は辞書・コスト変更を確認し baseline 値を更新する
