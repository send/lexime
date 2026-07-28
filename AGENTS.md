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
- **Deletion durability vs. the durable set (settled)**: the no-op-deletion
  skip is gated on `UserHistory::deletion_has_durable_target`, not on memory
  alone — an entry evicted for capacity is gone from the maps while the
  checkpoint still holds it, and skipping there made the deletion a silent
  no-op that a restart undid (#286). That predicate is deliberately one
  method rather than two the caller ORs together: answering only the
  in-memory half is the bug itself. Two sub-decisions are settled. (a) The
  residue tracks *keys*, not a single "something was evicted" bit: a bit is
  permanently set once a heavy user reaches `max_unigrams`, which would make
  every ForwardDelete of a never-learned candidate pay a key-thread
  F_FULLFSYNC (measured 5.9ms p50 / 11.7ms max) plus a full checkpoint.
  (b) Each residue key carries the epoch it was raised at, and the covering
  pass retires a key only if that epoch has not moved. A plain set is
  idempotent, so a key re-raised while a checkpoint is being written looks
  identical to one raised before it, and covering would retire a key the
  written checkpoint still contains — reopening #286. Findings proposing to
  collapse the residue to a boolean, or to drop the epoch stamps for a plain
  set difference, re-litigate these — do not raise them. Past a memory cap
  the residue drops key tracking and answers conservatively for everything;
  the cap is a constant, not a function of `max_unigrams`/`max_bigrams`,
  because those are user-settable while the compaction that clears the
  residue fires on a fixed frame threshold — deriving it would let a
  small-capacity configuration saturate during ordinary typing.
- **`wal_state` does not carry freeze or migration failure (settled)**:
  `OpenReport` reports appends-frozen and migration-commit-failure as their own
  fields rather than folding them into `wal_state`. `WalState::Quarantined`
  does *not* imply frozen (a quarantine that succeeds leaves a fresh,
  appendable WAL), so folding them conflates data loss with degraded
  persistence; and `wal_state` is string-interpolated into the one diagnostic
  line that survives the shipped build, so overloading a variant makes that
  line wrong. Relatedly, `migration_failed` is derived from the migration's
  own outcome (`migrate && !migrated_from_v1`), not from `is_frozen()`: the
  `Err` branch only freezes when a legacy WAL was consumed, so a v1 checkpoint
  beside a fresh WAL fails the commit without freezing anything and would
  report a clean startup. A dedicated `CheckpointState::MigrationFailed` (the
  other option #296 floated) is rejected for the same reason as the
  `wal_state` fold: the two facts are independent — a migration can fail with
  appends healthy, and appends can freeze with no migration in sight — so one
  enum cannot carry both without a state per combination. Findings proposing
  either fold, or a migration-specific `CheckpointState`, re-litigate a
  settled decision — do not raise them. `migration_failed` drives
  `compaction_recommended` only when the `.v1.bak` rescue copy exists: that
  compaction is not the migration — `run_compact_impl` writes a v2 checkpoint
  over the v1 file without the commit's preconditions (no backup, no
  `Migrated` state) — so without a rescue copy it would destroy the only copy
  of the v1 bytes.
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
- **Unusable config entries are dropped, not rejected (settled)**: an empty
  side in `[keymap]` and a snippet body that expands to nothing are ignored,
  and the file still loads. Rejecting was tried and reversed: `init_custom` is
  all-or-nothing, so one bad entry would revert every other custom value
  (costs, history limits, snippet trigger, the working key mappings) to the
  defaults. A *malformed* entry — unparseable key_code, wrong arity — is a
  different case and still rejects the file. Both drops are reported, over the
  FFI, because engine `tracing` does not reach the shipped build
  (`settings_keymap_warnings`, `LexSnippetStore::unusable_keys`); the rules are
  in SPEC.md § キーリマップ. Findings proposing to reject on an empty value, or
  to log the drop instead of returning it, re-litigate this — do not raise them.
- Prioritize conversion correctness and boundary safety over naming/style
  nits.

CLAUDE.md is the SSoT; this section deliberately does not duplicate its
content.
