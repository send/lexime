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
  settled decision — do not raise them.
- **A failed migration is reported, not healed in-session (settled)**: a
  `migration_failed` startup deliberately schedules no compaction. A
  compaction is *not* the migration — `run_compact_impl` writes a v2
  checkpoint over the v1 file with none of the commit's steps (no `.v1.bak`,
  no `Migrated` state) — so using one as the retry destroys the v1 bytes on
  exactly the path where the commit is already failing. The next launch
  re-attempts properly. `.v1.bak` stays **best-effort** per LXUD v2 decision
  #13: it is a manual rescue hatch, not a correctness dependency, and
  migration proceeds whether or not it lands. Making it a precondition was
  tried and reverted — turning it into a dependency meant every write, GC and
  wipe site had to join a rescue protocol, and three review rounds each found
  another site that had not. Findings proposing to gate writes on the backup,
  or to retry a failed migration with a compaction, re-litigate this — do not
  raise them. (The legacy-WAL variant does still complete in-session, but for
  an unrelated reason: that path freezes the WAL, and the compaction
  `appends_frozen` schedules to thaw it writes the v2 checkpoint as a side
  effect. The retry timing is therefore not a property of `migration_failed`,
  which is why the shipped log line states the fact and not a schedule.)
- **The runtime durability channel (settled)**: `durability_issues()` reports
  what holds *now*, and five sub-decisions are settled. (a) It is a **list**,
  not one enum or a bool: on a failing volume an unpersisted deletion (#295)
  and memory-only learning (#288) hold simultaneously — the steady state, not
  a corner — so collapsing hides one behind the other. (b) There is **no
  commit-side ledger**. `append_record` returns `Io` only from its frozen
  guard or from an append that freezes, so "a commit is memory-only" is
  exactly "the WAL is frozen"; a second ledger would be a duplicate book free
  to disagree. (c) The deletion raise **does not branch on the error variant**
  — `SyncFailed` and `Io` both raise. §8 ※1's silent power-loss window is the
  *Committed* window; the Tombstone window is zero by §6, so a failed flush is
  a real breach. (d) The cover is tied to a **durable checkpoint save**, not
  to the WAL truncation that follows: truncation is the physical scrub of
  superseded frames, and gating on it leaves a permanent warning whenever
  frames land mid-run (`FollowUp`) or the truncate fails on an otherwise
  durable write. `clear`'s empty checkpoint is the second cover point —
  without it a full wipe leaves a standing privacy warning on a history that
  provably holds nothing. (e) Swift keeps it **separate from
  `EngineInitFailure`** on lifetime, not on snapshot-ness (`recordFailure`
  appends after load, so init failures are not a snapshot): init failures
  latch, runtime issues clear when the disk recovers. The rows sit outside the
  `isDegraded` gate because the main #295 scenario is a clean launch followed
  by a later failure. Findings proposing to collapse the list, add a commit
  ledger, treat `SyncFailed` as benign, gate the cover on truncation, or merge
  the issues into `initFailures` re-litigate these — do not raise them.
  Deliberately **out of scope**: a persistent `spawn_compact` thread-spawn
  failure leaves `scrub_pending` unconsumed, so deleted strings can linger in
  the old checkpoint. The deletion itself is durable, startup
  `replayed_deletion` scrubs, and there is no action for the user to take.
- **The deletion ledger stays on `LexUserHistory`, not in `UserHistory`
  (settled)**: it looks like a duplicate of `DurableResidue` — same raise
  event, same cover moment, and the residue gets its ordering by construction
  (the snapshot clone carries the epochs) where the ledger needs an explicit
  read-before-clone. Reviewers reliably propose merging them. Two things block
  it. (1) **The read must take no lock.** The consumer is a UI poll;
  `DurableResidue` lives behind the `inner` RwLock, which the key thread holds
  for writing inside the wal critical section on every commit, so a merged
  ledger would put the menu behind exactly the stall `appends_frozen` is
  shaped to avoid. (2) **lex-core cannot see the `SyncFailed` raise.**
  `apply_batch`'s witness is `(WalRecord, Option<u64>)` and a `SyncFailed`
  tombstone carries `Some(seq)`, indistinguishable from a healthy one — by
  design, since the residue deliberately excludes `SyncFailed` (its frame is
  replayable). Merging would mean widening that witness to a three-state
  durability value across a settled PR1 surface to serve a reporting concern.
  The two ledgers answer different questions: the residue asks "may the
  durable set still hold this key" (gating the no-op skip), the ledger asks
  "did this deletion reach disk at all" (reporting). Findings proposing to
  move or merge them re-litigate this — do not raise them.
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
