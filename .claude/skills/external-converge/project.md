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

Observed on elidex (user-confirmed 2026-07-18): a Codex review commonly
takes **~30 minutes** to land, range ~15-30 min and sometimes longer (the
earlier "~15 min normal", #390 on 2026-06-21, was the low end).
Manually-triggered rounds have been far faster (under 4 min in three samples
on 2026-07-28), too few to retune on. So:

- **~30 min is NORMAL, not stuck** — after a trigger, do not re-trigger or
  surface-as-slow inside that window; the review is almost certainly still
  running, and re-triggering interrupts/duplicates it.
- **Surface / re-trigger threshold = `wakeup_surface_threshold` below** —
  measured from the trigger, with no Codex review, comment or inline thread
  on head since.

## wakeup_surface_threshold

`~45min` — how long one `@codex review` trigger is given, counted from the
trigger comment, before it is treated as dropped (SKILL.md Step 5.5 (2)).
Set well above the ~30 min latency in `wakeup_poll`, so a normal-but-slow
review is not cut off (elidex uses the same value for the same reviewer); it
overrides the generic SKILL.md `~15 min` default.

## reviewer

- `bot_login`: `chatgpt-codex-connector[bot]` (REST form). **GraphQL
  `reviewThreads` author.login is the BARE `chatgpt-codex-connector`** (no
  `[bot]`) — the Step-1 fetch must normalize (strip `[bot]`) for GraphQL
  comparisons or it false-negatives every inline finding (elidex
  `#316`/`#337`).
- `name`: Codex (OpenAI Codex Cloud, ChatGPT **Pro**)
- `trigger`: `@codex review`
  - The literal comment body, posted to re-trigger each round. Write only
    the body here; Step 1 posts and searches for exactly this value.
- `error_verdict_match`: `Codex Review: Something went wrong` — the start of
  Codex's error comment, which goes on to ask for a new trigger: no marker,
  no badge, no blob link. The trigger was
  consumed and failed; re-post at once (SKILL.md Step 1 error shape).
- `status_comment_match`: `codex-pull-request-review-summary` — the
  HTML-comment tag of Codex's "Codex Review Summary" status table (`Review |
  Status | Commit | Review trigger`), created on the push and edited in
  place. Neither a finding nor a verdict (SKILL.md Step 1 status-widget
  shape; elidex `#510` R12).
- `body_finding_match`: `shields\.io/badge/P[0-9]` — Codex renders each
  line-anchored body finding with a P0-P3 severity badge image. The global ORs this with
  its always-on generic `/blob/<sha>/…#L…` detector, so it can only add
  matches.
- `non_verdict_signals`: the 👍 reaction and the status table's `Completed`
  row — an auto-review can end with only these and no comment, which is not
  an assessment (`gh-pr-merge-head-guard.sh` counts a `Completed` row on
  head as reviewed for the merge check only; it is still no verdict here).
  Surface-to-user shape: your manual trigger for the current head reaches
  `wakeup_surface_threshold` with no Codex comment or review since, but a
  bot 👍 dated after it (do not re-post; tell the user).
- `auto_review_trusted`: `false` — trigger yourself even though Codex
  automatic review is ON (it has failed to arm: `#301`).
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
