---
name: lexime-review
description: lexime project-specific pre-push design review (5-agent parallel). Checks 辞書ソースオブトゥルース / 測定・コーパス規律 / 構造的正しさ / UniFFI 境界 / プロジェクト文脈 beyond what generic /code-review + /review cover. Run BEFORE git push as /pre-push Stage 6 — the final design gate; post-push Codex is a single-shot second opinion, not a safety loop.
user-invocable: true
---

# lexime-review — pre-push diff design review

`/code-review` (correctness bugs) と `/review` (一般 PR review) **の上に重ねる** lexime 専門 design review。`/pre-push` の最終段。post-push の Codex は single-shot second opinion なので、設計の取り切りはここが主担。

- **Axis SSoT**: `./axes.md` (5 軸定義 + severity calibration、`/lexime-plan-review` と共有)
- 対象 diff = `git diff origin/main...HEAD` (best-effort fetch 後。PR の見え方と同じ 3-dot)

## Skip OK

Default = run。skip は **軸ごとの expected-yield=0 を正直に言える時のみ** (per-PR 判断、形状の事前表引きではない):

| Axis | Fires (→ run) when… (非網羅) |
|---|---|
| 1 辞書 SSoT | rewriter / postprocess / cost / dict_source / pos_map を触った |
| 2 測定・コーパス | settings / コスト定数 / reranker / converter (viterbi) / corpus TOML を触った |
| 3 構造的正しさ | lex-session / lex_engine / persistence / AsyncWorker / lock・epoch 系を触った |
| 4 UniFFI 境界 | `engine/src/` / `Sources/` を触った |
| 5 文脈整合 | 上記いずれか + 挙動変更 / 新機構の追加 |

**Review・enforcement tooling の編集 (`.claude/skills/**`, hooks) は inert 扱いにしない** — ゲートの挙動を変えるので full gate を通す。純 inert doc (typo / wording のみ) だけが skip 対象。

## Workflow

### Step 1 — diff 収集

```bash
git fetch --quiet origin main 2>/dev/null || echo "⚠ fetch 失敗 — base が stale の可能性" >&2
DIFF=$(mktemp "${TMPDIR:-/tmp}/lexime-review-diff.XXXXXX")   # 並列セッションと衝突しない per-run パス
git diff origin/main...HEAD > "$DIFF"
git diff --stat origin/main...HEAD
```

### Step 2 — 5-agent 並列レビュー

Agent tool で 5 軸を並列起動 (1 agent = 1 軸)。プロンプト template:

> Read `<repo>/.claude/skills/lexime-review/axes.md` の Common + Axis N と `<repo>/CLAUDE.md` の設計哲学・設計規律。次の diff (`$DIFF` の実パスを埋め込む) を Axis N の Detect 観点で精査し、findings を `path:line / severity (CRIT|IMP|MIN) / 何が壊れるか一文 / 根拠` で返せ。必要なら repo の該当ファイルを読んで文脈確認してよい。findings ゼロならその旨と「何を確認したか」を返せ。

### Step 3 — 集約と裁定

- 重複統合 → axes.md Common で severity 再校正 (agent の申告を鵜呑みにしない)
- **Fix discipline (哲学レンズ)**: 各 real finding に対し symptom vs root を CLAUDE.md 設計哲学で判定 — 明白な patch (guard / sort / 特例) より、辞書・データ根治 (Axis 1) や構造で不変条件が成立する形 (Axis 3) を優先
- **Disposition**: real → 完全に fix (real MIN 含む、"edge だから defer" 禁止) / FP → 根拠つき reject。pragmatic な妥協を推奨形で user に丸投げしない

### Step 4 — fix 後の再検証

fix でコードが変わったら `/pre-push` Stage 1-2 (fmt + verify) を再実行して green を確認。findings と disposition を landing report にまとめて終了 (push はしない — push は別途授権)。
