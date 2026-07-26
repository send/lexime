# AGENTS.md

This repo's engineering rules, build commands, and workflow are in the
**`CLAUDE.md`** at the repo root — read it first; it is the single source of
truth. Do not restate or fork its rules here.

## Review guidelines

Codex reads this section for PR code review (it keys on the literal
`## Review guidelines` heading). The local pre-push gate (Claude-based)
already covers generic correctness and style, so a generic review adds
little. Add value by applying lexime's *project-specific* lenses and flagging
what a generic reviewer misses:

- **Conversion-accuracy impact**: changes to dictionary sources
  (`engine/data/`, `engine/crates/lex-cli/src/dict_source/`), connection
  costs, feature weights (`engine/crates/lex-core/src/settings.rs`),
  rerankers, or the Viterbi path must show before/after results of both
  accuracy corpora (`mise run accuracy` / `mise run accuracy-history`) in the
  PR description (CLAUDE.md § 変換精度テスト). Flag cost/weight changes that
  lack that evidence.
- **Corpus discipline**: an accuracy-corpus `skip` without an issue link is a
  violation (理由なし skip 禁止); a new history-corpus case without a
  `baseline` (expected result with no history) is a real defect, not a nit.
- **FFI boundary (UniFFI)**: the Rust engine (`engine/`) reaches the Swift
  frontend (`Sources/`) via UniFFI. Watch for panics that could cross the
  boundary, blocking calls on the IMKit main thread, and callback
  re-entrancy.
- **Concurrency invariants**: history/learning state sits behind locks with a
  known self-deadlock history (PR #237); lattice generation consistency is
  epoch-based, including the watermark across FFI (PR #258-260 series). Flag
  lock-order changes and epoch/generation regressions.
- **Performance scope**: only the IME hot path (conversion, key handling)
  is performance-sensitive. Dev-tool (`dictool`) performance nits are
  acceptable-by-policy — do not raise them.
- **Durability stance (settled)**: the user-history checkpoint's parent-dir
  fsync after rename is deliberately best-effort and log-only (design §6:
  APFS journals renames; the worst case of an unsynced rename is rolling
  back to the previous checkpoint, never corruption). Findings that couple
  rename-durability confirmation to WAL truncation or other behavior
  re-litigate a settled design decision — do not raise them.
- **User-dictionary recovery visibility (settled)**: a corrupt user_dict is
  quarantined and started empty (open no longer throws on corruption), and
  Swift records it as a dedicated `.userDictionaryDataLoss` case distinct from
  `.userDictionary` (which now means an environmental read failure → dict
  unavailable). This mirrors the merged `.history` / `.historyDataLoss` split:
  "registration keeps working but the registered words were lost" is a
  different state with different remediation than "dictionary unavailable". It
  supersedes design §10's earlier "reuse the existing `.userDictionary` case"
  note, which predates the history split. Findings proposing to collapse the
  two cases re-litigate a settled decision — do not raise them.
- **Non-empty inline text while composing (settled)**: a session that stays
  composing while the host's marked text goes away leaks the confirming key to
  the web page (PR #293). The rule, its enforcement, and what is deliberately
  *not* covered are specified in SPEC.md § 不変条件（marked text と session の同期）
  — read it before raising anything in this area. One decision is settled: the
  session proptest models the host's marked text cumulatively, because `commit`
  ends the marked session too, so a per-response check cannot see that shape;
  findings proposing to replace the cumulative model with a per-response check
  re-litigate it — do not raise them. Everything else is open, including the
  unmodelled `currentDisplay` writers SPEC names.
- Prioritize conversion correctness and boundary safety over naming/style
  nits.

CLAUDE.md is the SSoT; this section deliberately does not duplicate its
content.
