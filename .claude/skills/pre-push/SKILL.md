---
name: pre-push
description: Run the full lexime pre-push gate in one shot — fmt → verify (cargo + conditional accuracy/swift) → /simplify → /code-review → /review → /lexime-review. Invoke BEFORE git push / gh pr create. Post-push Codex is a single-shot second opinion, so this gate carries the depth; skipping stages here is what turns into 37-commit review tails (PR #281).
user-invocable: true
---

# pre-push — one-shot pre-push gate

6 段を固定順で 1 invocation に束ねる。「どの段を忘れたか」を決定面から消すのが目的。post-push の Codex は single-shot なので、正しさ・設計の取り切りはこのゲートが主担 (判例: PR #281 — pre-push ゲート無しで push し、レビュー修正 37 コミットの tail を払った)。

## Hard rules

- **No skipping** — 全段 invoke する。唯一の例外は純 inert doc PR (typo / wording のみ、Stage 3-6 不要)。**review・enforcement tooling の編集 (`.claude/skills/**`, hooks) は inert ではない** — full gate を通す
- **No substitution** — `/lexime-review` は `/simplify` + `/code-review` + `/review` の代替ではない。4 つとも走らせる
- **Fix → re-verify** — どの段でも編集が発生したら (コードに限らず corpus・辞書 TSV 等のデータ編集も) Stage 1 → 2 を**条件付きゲート込みで**再実行してから続行 (`/simplify` は auto-apply なので特に。データ編集は該当する accuracy ゲートの再実行が本体)
- 本 skill は **push の手前で止まる**。push / PR 作成は別途の授権アクション

## Stages (fixed order)

### Stage 1 — Format

```sh
cd engine && cargo fmt --all
```

### Stage 2 — Verify

```sh
cd engine && cargo fmt --all --check && cargo clippy --workspace --all-features -- -D warnings && cargo test --workspace --all-features
```

条件付き追加 (diff に該当パスがあれば必須):

条件付き追加 — **SSoT は `.github/workflows/ci.yml`**。diff の変更パスを `changes` filter に当てて「CI でどの job が走るか」を判定し、**走る job 全部**のローカル等価をここで先に走らせる。filter のパス内容も job 一覧もこの doc を正としない (書き写しは drift する — 本 PR の R2〜R4 レビューがその実証)。既知の job → ローカル等価対応 (**非網羅** — ci.yml にここに無い job を見つけたら実行 + この表に追補):

| CI job | ローカル等価 |
|---|---|
| lint / test-* | 上の基本 verify でカバー済み |
| accuracy | `mise run accuracy && mise run accuracy-history`。accuracy に影響する変更 (コスト・重み・reranker・辞書ソース・変換パス) なら before/after を記録し PR に貼る (CLAUDE.md §変換精度テスト) |
| swift | `mise run test-swift` |
| audit | `mise run audit` (CI audit job と同一ステップ、locked check 込み) |
| msrv | `cd engine && cargo +<rust-version> check --workspace --locked` (`<rust-version>` は `engine/Cargo.toml` の `rust-version` を読む。toolchain 未導入なら `rustup toolchain install <rust-version>`) |
| CodeQL / Analyze | ローカル等価なし — CI に委ねる |

### Stage 3 — `/simplify`

Quality-only pass (reuse / simplification / altitude)。auto-apply されるので、直後に Stage 1-2 を再実行。

### Stage 4 — `/code-review`

**Effort は blast radius 連動**:

- routine PR → `/code-review high` (broad coverage が floor — post-push loop はもう無い)
- 高 blast-radius (persistence / epoch・世代 / FFI 境界 / edge-dense subsystem / 大 diff) → `/code-review ultra`

fix する価値のある findings を適用。

### Stage 5 — `/review`

一般 PR-level review。

### Stage 6 — `/lexime-review`

プロジェクト固有 5 軸 design review (最終 design gate)。skip 判断はあちらの Skip-OK 表に従う。

## On completion

全段 green / addressed で push-ready。landing report (各段の結果 + skip した段とその理由) をまとめ、push / PR 作成の提案を user に出す (自律 push しない)。
