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
- **Fix → re-verify** — どの段でもコード編集が発生したら Stage 1 → 2 を再実行してから続行 (`/simplify` は auto-apply なので特に)
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

条件付き追加 — **SSoT は `.github/workflows/ci.yml` の `changes` filter**。diff の変更パスを filter に当てて「CI でどの job が走るか」を判定し、走る job のローカル等価をここで先に走らせる。**filter のパス内容をこの doc に書き写さない** (書き写しは必ず drift する — 本 PR の R2/R3 レビューがその実証):

- CI **accuracy** job が走る変更 (`core ∨ cli ∨ corpus` filter) → `mise run accuracy && mise run accuracy-history`。コスト・重み・辞書ソース・変換パスの変更なら before/after を記録し PR に貼る (CLAUDE.md §変換精度テスト)
- CI **swift** job が走る変更 (`core ∨ session ∨ ffi ∨ swift` filter) → `mise run test-swift`
- CI **audit** job が走る変更 (`core` filter — Cargo.toml/lock・deny.toml・supply-chain 等を含む) → `mise run audit`

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
