# /external-converge project overlay — lexime

Loaded by `~/.claude/skills/external-converge/SKILL.md` (the full multi-round
convergence loop) when invoked from this repo. Reviewer = **OpenAI Codex on
ChatGPT Pro** (loop-affordable, no per-credit cost). For the routine
single-pass, see `external-review/project.md` (same reviewer; `repo`,
`build_verify`, `fix_discipline`, `classification_calibration` are defined
there and apply unchanged in the loop — not restated here).

## failure_modes

The loop's defensive rules (Step 1 pitfall gate, Step 4 TERMINAL stop,
request-staleness gate, generator-layer / altitude check, pre-Ask
written-lens attestation) were calibrated on elidex incidents — see
`../../../../elidex/.claude/skills/external-converge/project.md`
`failure_modes` for that log. lexime-local incidents:

- **PR #281 (2026-07-02, LXUD v2 PR1)** — 1 feature commit + **37 review-fix
  commits over ~7.5h**. Amplifiers: dual reviewers (Copilot auto-review
  still ON per-push + Codex rounds; Copilot is OFF since 2026-07-03),
  same-mechanism corner-hopping (WAL truncate durability / rename
  confirmation / compaction queue→drain→re-pass→cap patched from a new
  corner each round), and a branch that predated the AGENTS.md/overlay
  merge so no lens or loop-defense was visible. Operative rules:
  **(a)** fire the cumulative fix-delta design re-gate (`/lexime-review`
  over `git diff <feature-commit>..HEAD`) EARLY on ≥2-round same-mechanism
  patching, not only at TERMINAL — a diverging loop never reaches TERMINAL;
  **(b)** at loop start, merge main into long-running branches so
  AGENTS.md + overlays are present; **(c)** depth belongs in `/pre-push`
  BEFORE the first push — the loop is not the primary net.

Inherited from the Copilot era (reviewer-agnostic, still applies):

- Severity miscalibration — reviewers inflate polish to IMPORTANT; apply the
  one-sentence "what concretely breaks?" test strictly. Doc imprecision that
  doesn't misdirect is MINOR.
- Merge-on-stale is mechanically blocked by
  `~/.claude/hooks/gh-pr-merge-head-guard.sh` (PreToolUse on `gh pr merge`;
  override sentinel `# merge-stale-ok`) — it activates for any repo where
  Codex has assessed the PR at least once, including this one.

## wakeup_poll

`300s` — poll cadence while waiting for Codex's review to land, **NOT a
latency prediction**.

Observed on elidex (user-confirmed 2026-06-21, #390): a single Codex review
normally takes **~15 minutes** to land. So:

- **~15 min is NORMAL, not stuck** — do not re-trigger or surface-as-slow at
  ~14-15 min; the review is almost certainly still running, and re-triggering
  interrupts/duplicates it.
- **Surface / re-trigger threshold = ~25-30 min** of zero Codex activity on
  head (no formal review, no marker-bearing issue-comment, no inline thread).
  The generic SKILL.md "~15 min" sanity cap is too aggressive for this
  reviewer.

## reviewer

- `bot_login`: `chatgpt-codex-connector[bot]` (REST form). **GraphQL
  `reviewThreads` author.login is the BARE `chatgpt-codex-connector`** (no
  `[bot]`) — the Step-1 fetch must normalize (strip `[bot]`) for GraphQL
  comparisons or it false-negatives every inline finding (elidex
  `#316`/`#337`).
- `name`: Codex (OpenAI Codex Cloud, ChatGPT **Pro**)
- `trigger`: `@codex review` (posted as a PR comment to re-trigger each round)
- `assessed_commit_marker`: `Reviewed commit:` — appears in BOTH
  formal-review bodies AND Codex's dry-verdict issue-comment, followed by
  `` `<sha>` ``. Step 1 reads the latest assessed commit from this marker
  across reviews + issue-comments (NOT the reviews API alone).
- `dry_verdict_match`: `Didn't find any major issues` — posted as a **plain
  PR issue-comment** (`Codex Review: Didn't find any major issues`), **not**
  a formal review. A dry-verdict comment on the current head IS a dry round;
  keying head-staleness on `pulls/{n}/reviews` alone false-stalls every dry
  round (elidex `#322`/`#337`).

The genuine Pro Codex responds as `chatgpt-codex-connector[bot]`; `@codex`
with a non-`review` instruction starts a Pro-billed Codex cloud task (fine —
just not a review). The Copilot-billed `codex[agent]` SWE agent is a
different product — if a responder is not `chatgpt-codex-connector[bot]`,
stop and check billing (full caveat: `external-review/project.md`). Lenses
reach Codex via `AGENTS.md` (`## Review guidelines`).
