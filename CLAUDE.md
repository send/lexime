# CLAUDE.md

## 設計哲学

コンセプト: **思考の速度で日本語を書ける開発者向け IME**(Speed of Thought — SPEC.md)。
「速さ」= 思考の非中断: hot path 即答 / 1発目精度 (top-1 受容率) / 操作・視線の中断最小化。
設計判断で迷ったら「ユーザーの思考を止めるか?」で判定する。

- **辞書がソースオブトゥルース**: 言語知識は辞書 + POS + 接続コストに宿る。個別語のコード分岐は禁止。構造問題 (分節・Viterbi) をコスト調整で偽装しない
- **狭く厳選**: 辞書拡充は curated + 必要性駆動 (user_dict seed)。bulk merge 禁止
- **学習データはユーザーの資産**: 破損・削除・復元をサイレントにしない。個人の入力内容 (history / commit-log / audit 出力) を PR・issue に貼らない — 集計値のみ
- **正しさは構造で守る**: 世代整合は epoch、並行性は単一所有 + snapshot、キャッシュはトランザクション + 原子 swap。lock guard を callback 越しに持たない
- **UniFFI 境界**: Rust が単一真実源。panic 越境・main thread ブロック・サイレント失敗禁止

## 設計規律

- 場当たり的 reactive fix 禁止。新しい型・rewriter・特例の前に既存の抽象 (辞書層 / POS / コスト / 既存 rewriter) で解決できないか考える
- 同種処理は単一の正準形に収束させる。dead code・後方互換 shim は削除。ただしオンディスク形式 (履歴 / user_dict / 設定) の変更は migration 必須
- コスト・重み変更は「直る側」だけでなく「壊れる側」も実測してから採用する (§変換精度テスト)
- 不変条件が交差する大きめの work は multi-PR に分割し、実装前に `/lexime-plan-review` を通す
- 大型 refactor の見送りは発動条件つきで記録する。コンセプトに効かない理想化はしない
- 並行 Claude セッションあり: コミットするブランチは専用 worktree で隔離

## ビルド・テスト

```bash
# Rust lint + test
cd engine && cargo fmt --all --check && cargo clippy --workspace --all-features -- -D warnings && cargo test --workspace --all-features

# アプリビルド・インストール
mise run build && mise run install && mise run reload
```

## ワークフロー

main に直接コミットしない。必ず以下の流れで作業する:

1. `git worktree add -b <type>/<topic> <dir> origin/main` で専用 worktree にブランチを切る（§設計規律: 並行セッションと tree を共有しない）
2. 変更をコミットする（Conventional Commits: `feat`, `fix`, `refactor`, `docs`, `chore`）
3. push 前に `/pre-push` を通す（fmt → verify → /simplify → /code-review → /review → /lexime-review の 6 段ゲート。post-push レビューは single-shot なので深さはここで確保する）。ゲートが編集を生んだら（fmt / /simplify auto-apply / fix 適用）追加コミットしてから次へ
4. `git push -u origin <branch>` で push する
5. `gh pr create` で PR を作成する。未チェックのテストプランがある場合は先に済ますこと
6. コードの変更を含む PR はレビュー対応後にマージする（後述）
7. `gh pr merge --merge --delete-branch` でマージする

### PR レビュー対応フロー

外部レビュー (OpenAI Codex) は routine PR は `/external-review` スキル (single-pass triage)、高 stakes / 高 blast-radius PR は `/external-converge` スキル (収束ループ) で対応する。reviewer の詳細 (bot 名、trigger、fetch の罠、latency) は `.claude/skills/external-review/project.md` / `.claude/skills/external-converge/project.md` の overlay に記載。Codex へのレビュー観点の供給は `AGENTS.md` の `## Review guidelines` 節 (pointer-only、SSoT は本ファイル)。

このリポジトリ固有の運用ルール:

- **Codex automatic review が全 PR に自動で付く** (chatgpt.com/codex 設定)。再レビューは PR コメント `@codex review` で依頼する。`review` 以外の `@codex <指示>` は Codex cloud task として実行される (同じ Pro 課金)。応答者の identity 確認は overlay の Identity caveat 参照
- **CI 確認**: `gh pr checks {number}` で全チェック pass を確認
- **マージ前にユーザー確認**: CI pass + レビュー対応完了後でも、`gh pr merge --merge --delete-branch` の前に必ずユーザーに確認を取る (`gh pr merge --auto` 禁止)

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
