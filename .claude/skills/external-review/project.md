# /external-review project overlay — lexime

Loaded by `~/.claude/skills/external-review/SKILL.md` (single-pass triage) when
invoked from this repo. The multi-round loop variant's calibration
(`wakeup_poll`, failure-mode inheritance) lives in
`external-converge/project.md` (same reviewer).

## repo

`send/lexime`

## build_verify

`cd engine && cargo fmt --all --check && cargo clippy --workspace --all-features -- -D warnings && cargo test --workspace --all-features`

Per CLAUDE.md「ビルド・テスト」. When a fix touches dictionary sources /
connection costs / feature weights / rerankers, additionally run both
accuracy gates and keep them green: `mise run accuracy && mise run
accuracy-history` (skip 以外全 pass — CLAUDE.md § 変換精度テスト).
Swift-side changes are gated by CI (`gh pr checks`), not locally.

## fix_discipline

No project-local review-axes skill (unlike elidex) — run SKILL.md's bare
symptom-vs-root check per fix. lexime-specific bias:

- Conversion-accuracy regressions are fixed at the *data/cost* root
  (dictionary entry, connection cost, feature weight), not by special-casing
  in conversion code.
- When a reviewer-confirmed conversion miss is real, add a regression
  corpus case (`engine/testcorpus/accuracy-corpus.toml`, regression
  category) alongside the fix — CLAUDE.md § 運用ルール recommends it.

## classification_calibration

- Dev-tool performance findings (`dictool` subcommands, corpus-mining paths)
  are **WONTFIX by policy** — resolve as "acceptable for runtime profile",
  do not auto-fix. Runtime (IME hot-path) perf findings are real.
- A new history-corpus case without `baseline` is IMPORTANT (CLAUDE.md rule),
  not a nit.
- An accuracy-corpus `skip` without an issue link is a real finding
  (理由なし skip 禁止).

## reviewer

- `bot_login`: `chatgpt-codex-connector[bot]` (REST form). **GraphQL
  `reviewThreads` author.login is the BARE `chatgpt-codex-connector`** (no
  `[bot]`) — normalize (strip `[bot]`) for GraphQL comparisons or every
  inline finding false-negatives (elidex `#316`/`#337`).
- `name`: Codex (genuine OpenAI Codex Cloud, ChatGPT **Pro** —
  loop-affordable, no per-credit cost; *not* GitHub Copilot credits)
- `trigger`: `@codex review` (or Codex automatic review, enabled at
  chatgpt.com/codex — ON for this repo since 2026-07-03)
- `assessed_commit_marker`: `Reviewed commit:` — in BOTH formal-review bodies
  AND the dry-verdict issue-comment, followed by `` `<sha>` ``; read the
  latest assessed commit from this marker across reviews + issue-comments.
- `dry_verdict_match`: `Didn't find any major issues` — Codex's no-findings
  verdict, posted as a **plain PR issue-comment**, not a formal review. A
  dry-verdict comment on head = clean pass; keying the head check on
  `pulls/{n}/reviews` alone false-reports "stale review" (elidex
  `#322`/`#337`).

**Identity caveat**: only the `chatgpt-codex-connector[bot]` review is the
genuine Pro Codex. A bare `@codex[agent]` mention routes to a GitHub-Copilot
SWE agent (Copilot credits) — do **not** use it. Lenses reach Codex via
`AGENTS.md` (`## Review guidelines`).
