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
  a corner — so collapsing hides one behind the other. (b) Both rows are **tracked facts, not
  derived ones**. `LearningMemoryOnly` used to be read off
  `HistoryWal::is_frozen()`, justified by "a commit is memory-only ⟺ the WAL
  is frozen". That equivalence is **false**, and the decision it justified is
  reversed: a commit refused by the frozen guard never reaches seq
  assignment, so `last_appended_seq` does not move, and an in-flight
  compaction whose snapshot predates it then satisfies `truncate_covered` and
  clears the freeze — leaving the commit in neither the checkpoint nor the WAL
  while the report reads clean (Codex R2 on #317, pinned by
  `test_a_stale_compaction_must_not_unfreeze_over_memory_only_commits`). One
  breach had already been patched (the encode guards) before the equivalence
  itself was questioned. The ledger now carries `raised_memory_only` beside
  `raised_deletion`, and the freeze went back to being what its name says — a
  property of the file, read only by `OpenReport`. Findings proposing to
  re-derive either row from `is_frozen()` re-litigate this. (c) The deletion raise **does not branch on the error variant**
  — `SyncFailed` and `Io` both raise. §8 ※1's silent power-loss window is the
  *Committed* window; the Tombstone window is zero by §6, so a failed flush is
  a real breach. The *marker* that carries the raise across a restart (#312)
  does distinguish them, and that is not a contradiction: within a session
  neither half is durable, so both are reported the same; across a restart
  `SyncFailed`'s frame is replayable, so reporting it after replay applied the
  deletion would be a latching alarm about data that is gone. Which is why the
  marker records a seq for that half and nothing for `Io`. (d) The cover is tied to a **durable checkpoint save**, not
  to the WAL truncation that follows: truncation is the physical scrub of
  superseded frames, and gating on it leaves a permanent warning whenever
  frames land mid-run (`FollowUp`) or the truncate fails on an otherwise
  durable write. `clear`'s empty checkpoint is the second cover point —
  without it a full wipe leaves a standing privacy warning on a history that
  provably holds nothing. (e) Swift keeps it **separate from
  `EngineInitFailure`** on lifetime, not on snapshot-ness (`recordFailure`
  appends after load, so init failures are not a snapshot): init failures
  latch, runtime issues are re-derived on every menu open. Retraction is
  **passive**, and its latency differs per half: the `Io` half clears on the
  first commit after the disk recovers (that append still fails against the
  frozen WAL, which posts a compaction), while the `SyncFailed` half never
  froze anything and waits for the 1000-frame / 1 MiB threshold. There is no periodic compaction and no
  quit-time flush, so a user who stops typing keeps the row. The runtime rows
  are also emitted whether or not any init failure exists — `menu()` gates on
  "are there rows", not on the engine being degraded — because the main #295
  scenario is a clean launch followed by a later failure. Findings proposing to
  collapse the list, treat `SyncFailed` as benign, gate the cover on
  truncation, or merge these two issues into `initFailures` re-litigate these
  — do not raise them. The dividing line is **retraction, not provenance**: a
  durability fact the *disk* can retract belongs on this list, and one no
  amount of recovery can retract belongs with the latching startup failures.
  `EngineInitFailure.historyDeletionLost` (#312) is the second kind — the
  deletion is already lost and only the user deleting again resolves it (two
  user actions retire the *row*: acknowledging it, and a wipe that makes the
  claim false; neither is the disk healing)
  — so it is not a breach of this entry, and proposals to move it onto the
  runtime list (or to fold the runtime rows into it) contradict the same rule
  from the other side. The "no commit-side ledger" clause that used to sit in this
  list is **withdrawn**; see (b).
  Deliberately **out of scope**, for two different reasons — do not merge them.
  (i) a persistent `spawn_compact` thread-spawn failure leaves `scrub_pending`
  unconsumed, and (ii) a tombstone that appends and flushes cleanly but whose
  scrub compaction's `save()` fails raises nothing (#311), so deleted strings
  sit in the old checkpoint and past Committed frames for the session. Both are
  deferred *physical scrubs*: the deletion itself is durable and startup heals
  it. (iii) was **not** like them — the ledger is process-local, so on the `Io`
  half, where no frame reached the WAL and no checkpoint landed, the deletion is
  not durable, startup does not heal it, and the report was gone on the very
  restart where the entry resurrects. **#312 closed it** (and with it #295) via
  the `.deletion-pending` sidecar. Its settled shape, all of it reached by
  review rather than by first draft:
  the checkpoint header's reserved bytes are unusable, because the raise
  condition *is* a failed checkpoint write (and an in-place header rewrite
  would recompute a CRC outside tmp+rename, risking the whole history to report
  one deletion); only `NotFound` is clean, so every malformed or unreadable
  marker reports and no CRC is needed — and that is enforced by decoding as a
  **round trip against the writer** (only what `encode` emits is accepted)
  rather than by validating fields, which missed four shapes in a row; the
  reader also refuses to *open* anything that is not a regular file, since a
  FIFO at that path would block the IME's startup thread forever, and `rename`
  replaces a symlink, a FIFO or an unwritable file at the path rather than
  writing through it, which also keeps a failed promotion from leaving a
  witness that re-based seq numbers could satisfy — but `rename` guards only
  the destination, so both the tmp `write_atomic` writes through and the
  marker the reader opens go through one resolution
  (`O_NOFOLLOW | O_NONBLOCK` plus an `fstat` on the obtained descriptor),
  closing by construction the symlink that would truncate a file this crate
  does not own and the readerless FIFO that would hang startup or the
  key-processing thread; a check-then-open would leave the two resolutions a
  substitution can race, which is why the reader stopped doing that;
  `rename` promoting a *name* rather than the flushed descriptor stays open
  and is stated rather than papered over — POSIX has no fd-based rename, a
  uniquely-named tmp would break the marker-plus-orphan pair below, and
  anything able to write in that directory can overwrite the destination
  outright with no window to hit;
  promotion of an `Unflushed{seq}` witness to `Lost` is **mandatory, not an
  optimization** — a seq is a position within one WAL file, not an epoch, and
  the witness test `seq > applied_seq` is an *inequality over a high water
  mark*, so once that file is replaced any later frame above the witness
  answers it "applied" whether or not the tombstone ever existed in the new
  lineage; a **seq floor** was tried here and removed, because skipping the one
  number the claim names changes nothing about an inequality (gaps are legal),
  and the entry that claimed it gave "epoch discipline" was wrong: a floor on a
  position creates no identity. While a report is owed and the disk does not
  yet say `Lost`, appends **freeze** — the state that would answer the witness
  must not advance until the lineage-independent form has landed;
  a claim counts as landed when the **logical** marker holds it — the canonical
  file *or* its flushed orphan tmp — because `write_atomic` syncs before it
  renames, so a rename that fails has still made the bytes durable where the
  next `read` merges them; treating that as failure froze appends on every
  commit forever against a record that already said what was wanted (which
  stage failed comes from `write_atomic_staged`, never from comparing the
  bytes back — a `write_all` that completed before `sync_all` failed leaves an
  image that compares equal without being durable, and calling that landed is
  the power-loss hole the sidecar exists to close);
  a stronger record on disk **satisfies** a weaker desired one (the merge
  lattice, not equality): `merge_write` absorbs a requested `Unflushed` back
  into a surviving `Lost`, so exact equality made the desired state
  unreachable and every commit paid another key-thread full sync forever;
  `merge_write` returns the value it **persisted**, not the one requested, and
  the belief records that — the write merges, so asking for `Unflushed` over a
  surviving `Lost` leaves `Lost`, and a caller repeating its own request would
  hold a belief the disk never had and skip the next reconcile as already
  satisfied;
  what the disk holds is a **three**-valued belief (`MarkerState`:
  absent / holds-these-bytes / unknown), because a file nobody could read is
  neither of the first two — collapsing it into absence let a failed unlink of
  an unreadable marker look settled, and `Unknown` never satisfies a desired
  state so the retry cannot be skipped;
  an owed report whose disk does not yet say `Lost` **freezes appends** — the
  one case where a sidecar may stop learning — whether the marker was
  unreadable or holds a decoded `Unflushed` the promotion could not replace,
  because either may name a sequence a later frame would answer and nothing
  about numbering can prevent that; `frozen` already means "appending to this
  file is not safe" (exempt: a disk that already says `Lost`, unsatisfiable by
  any sequence, and a claim this session raised about the file it is still
  appending to);
  a write reports **which stage failed**, and the caller never infers it: bytes
  read back cannot distinguish a flushed image from one that only reached the
  page cache before `sync_all` failed, so `write_atomic_staged` says whether
  the failure was pre-durability or the rename alone;
  a failed write or removal sets the belief to `Unknown` rather than leaving
  the old value — a short write can leave a truncated orphan the previous value
  does not describe, and a stale `Absent` would satisfy a desired `None` and
  skip the cleanup;
  sequence exhaustion is refused **where the number is issued**, never carried
  by `frozen`: a witness at the representable ceiling would otherwise make `+1`
  panic across the UniFFI constructor in debug and hand out seq 0 in release,
  and representing it as a freeze is wrong in kind — a compaction clears a
  freeze by rewriting the file, and rewriting a file creates no numbers, so the
  heal put the wrap straight back (adoption saturates instead, and `u64::MAX`
  is refused rather than spent since assigning it leaves no successor);
  a marker's *claim* and the *observation* of it are separate — anything
  unreadable claims `Lost` by the fail-safe rule but confirms nothing, so only
  a `confirmed` read reaches `marker_on_disk`, and `deletion_pending_checkpoint`
  seeds the `session` claim as well as the ledger so the two cannot disagree
  about what is outstanding;
  **every commit reconciles the marker** — in `apply_records`, under the wal
  mutex, before the appends — and that, not any per-event retry, is what makes
  the disk agree with the projection; a record reconciled only at chosen events
  is a cache with invalidation, which is why six review rounds each found a
  different event with no retry behind it (ack, wipe, the compaction's cover,
  recovery's removal, recovery's promotion, that compaction itself), and it is
  affordable on the key path only because a matching `flushed` makes it a
  memory comparison; before the appends because the harm is a WAL advancing
  past an un-promoted witness;
  the marker's two `compaction_recommended` feeders are **vetoed while
  `migration_failed`**: a compaction is not the migration — it writes a v2
  checkpoint over the v1 file with none of the commit's steps — so scheduling
  one there would destroy the v1 bytes on exactly the path where they are the
  only copy; both feeders are promptness alone, so the veto costs latency and
  nothing else;
  a startup retraction whose unlink fails hands the debt to
  `compaction_recommended` for promptness alone
  instead of a thousand frames later; the runtime seeds `MarkerClaims::flushed`
  from `OpenReport::marker_on_disk` rather than assuming a clear disk, since
  `None` is a positive claim that a failed promotion contradicts by leaving a
  *suppressible* `Unflushed` under a memory claim of `Lost`, and `flushed` is
  an observation rather than a claim, so a wipe folds `session` and leaves it
  standing;
  an empty loaded history and an empty durable set together settle a `Lost`
  claim — either half alone was wrong once, and requiring both also removes
  any need to tell an encoded `Lost` from the fallback a malformed marker
  decodes to; the startup
  counterpart of `clear` covering the ledger from its empty checkpoint, scoped
  to `Lost` because `Unflushed` is about durability rather than presence and
  replay can empty memory while the checkpoint still holds the entry; writes **merge** rather than replace,
  `Io` absorbing, and go through the shared `write_atomic`; the tmp/rename gap
  that once argued for a hand-rolled in-place write is closed by `read` merging
  the orphan tmp, which can only strengthen a claim — writing by hand instead
  cost three rounds of re-deriving durability details the shared primitive
  already encodes (symlink replacement, unlink-failure checking, the new dir
  entry's fsync); a claim
  whose write failed is held in memory so the next raise re-asserts it; startup
  **never retracts**, because a witness satisfied by replay was only satisfied
  out of the page cache — it hands the claim to the runtime ledger for a durable
  checkpoint to settle — and a witness that is *not* satisfied is promoted to
  unconditional, because a WAL quarantine re-bases seq numbering and an
  unrelated later frame would otherwise settle it; the compaction retraction
  shares the wal guard with the ledger cover, because the window between a CAS
  and a separate unlink cannot be pinned by a deterministic test — but it does
  **not** retract an inherited report that nothing has delivered yet, since a
  checkpoint written this session persists the *resurrected* entry and so
  settles nothing about a previous session's claim; the logical marker is the canonical
  file *and* the atomic write's orphan tmp — `read` merges the second, so
  removal must clear both or a `Lost` orphan rebuilds the unclearable latch —
  removal reports whether it is actually clear and an acknowledgement settles
  only when it is (an empty
  directory left there is a placeholder and gets cleared; a non-empty one is
  someone else's and does not, which the return value makes visible rather than
  silent); `clear`
  removes the marker **unconditionally and separately**, since a previous
  session's marker moves no counter in this one and the cover would early-return
  past it when the ledger is untouched; and the acknowledgement happens on a
  **click of the row**, neither at load nor at menu-build time — `bootstrap()`
  runs on IMKit probe launches that never show a menu, and IMKit also builds
  the menu without displaying it (measured: the record was consumed four
  seconds after an untouched relaunch), so only a person clicking is evidence
  the report was delivered.
  One class stays open by construction and is documented rather than fixed:
  the marker lives in the checkpoint's directory, so a failure of that whole
  directory (read-only volume, EACCES, parent removed) takes the marker with
  it. Findings proposing a header flag, a CRC, a plain overwrite, a tmp+rename
  write, a cover outside the wal mutex, a *replay-evidence* retraction at
  startup, an ack at load or at menu-build time, or folding `clear`'s wipe into
  the cover re-litigate these — do not raise them. (Retraction against a
  durable checkpoint at startup **is implemented**, not rejected, and so is the
  single owed-predicate that decides whether the status row stays. Findings
  about either are in scope — this list must never suppress review of the
  mechanism it describes.) Separately, #313 records a
  pre-existing privacy race: `apply_records` appends to the commit log outside
  the wal mutex, so a commit in flight can re-create `commit-log.jsonl` after
  `clear` unlinked it. Findings re-raising any of these should point at the
  issues rather than proposing a fix here.
- **The WAL's freeze flag is private to lex-core (settled)**: `HistoryWal`
  keeps `frozen` as a plain `bool`, read outside the WAL only by
  `OpenReport::appends_frozen` at open. An earlier revision of #317 shared it
  as an `Arc<AtomicBool>` behind a read-only `FreezeFlag` so the status menu
  could poll it lock-free; that whole apparatus was removed when the row
  stopped being derived from the freeze (see (b)). There is no runtime
  cross-thread observer of the freeze left, so findings proposing to share,
  lend, or hand out the flag are proposing a reader that does not exist.
- **The deletion ledger stays on `LexUserHistory`, not in `UserHistory`
  (settled)**: it looks like a duplicate of `DurableResidue` — same raise
  event, same cover moment, and the residue gets its ordering by construction
  (the snapshot clone carries the epochs) where the ledger needs an explicit
  read-before-clone. Reviewers reliably propose merging them. Two things block
  it. (1) **The read must take no lock.** The consumer is a UI poll;
  `DurableResidue` lives behind the `inner` RwLock, which the key thread holds
  for writing inside the wal critical section on every commit and a compaction
  holds for `cover_durable_residue`, so a merged ledger would put a main-thread
  menu poll behind history I/O, where the ledger's single atomic load blocks
  on nothing. Reason (2) is the harder blocker. (2) **`apply_batch`'s witness must not widen.**
  It is `(WalRecord, Option<u64>)`, and a `SyncFailed` tombstone carries
  `Some(seq)`, indistinguishable from a healthy one — by design, since the
  residue deliberately excludes `SyncFailed` (its frame is replayable).
  Merging would mean widening that witness to a three-state durability value
  across a settled PR1 surface to serve a reporting concern. (#312 put the
  ledger's *on-disk projection* in lex-core, `user_history/deletion_marker.rs`,
  so "lex-core never sees this distinction" is no longer the phrasing — lex-core
  owns the file family and the format. What it still does not see is the raise
  event: the engine classifies the failure and calls in. The blocker is the
  witness, not the crate.)
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
  re-litigate it — do not raise them.
- **Focus loss settles the session, without learning (settled)**: on
  `deactivateServer` the settle and the display clear are one teardown
  (`SessionCoordinator.deactivate(client:)`); re-entry may defer that teardown
  to the end of the delivery, but never splits the two apart. Two parts are
  settled:
  (a) it settles through **`settle_unconfirmed()`, not `commit()`** — focus loss
  is not acceptance, so it keeps what the host was showing and records no
  history, where a voluntary commit would resolve to the selected candidate and
  learn from it (Codex raised both halves as P1 on #315). "What the host was
  showing" is **passed in by the frontend**, not held by the engine:
  `settle_unconfirmed(displayed:)` takes `SessionCoordinator.currentDisplay`.
  Two engine-side alternatives were tried and reverted — inferring it from
  selection state (goes stale the moment a re-render puts the reading back) and
  recording it when the response is emitted (runs ahead of a delivery that the
  UI thread can still drop, so the settle commits text never shown). The engine
  emits marked text but cannot observe whether it reached the screen, so any
  copy it keeps is a shadow of unobservable state. Findings proposing to move
  this back into the session re-litigate it. It settles rather
  than discards because IMKit does not reliably send `commitComposition` first
  (measured, #298) and discarding would throw away text the user typed on every
  app switch; (b) settling is **unconditional
  while delivery is best-effort** — with no reachable client there is nowhere
  to insert the text, but that is not a reason to leave the session composing.
  `resetDisplay()` deliberately does *not* settle: committing on arrival would
  insert the previous document's text into the client that just gained focus.
  A `didSet` on `currentDisplay` was considered and rejected — `applyEvents`
  legitimately nils the display on `.commit` while the session keeps composing,
  so a per-write check false-fires, the same reason the Rust side checks per
  *response*. Findings re-proposing exactly these re-litigate them.
  **Everything else in this area is open**, and specifically these are known
  and unfixed, not settled: the `activateServer` side is still unmodelled — if
  IMKit skips `deactivateServer` the session reaches `resetDisplay()` still
  composing, the assert that would catch it is compiled out at `-O`, and the
  next real `deactivateServer` then settles against an empty display and drops
  the typed text without a trace (SPEC § 不変条件 records the path and that
  consequence); whether this settle should ever *learn* is **#310**, still open
  — recording nothing is the safe default, not a measured answer, and it means a
  surface the user did navigate to reaches the document while the ranking that
  produced it stays untrained; and the general display/commit divergence —
  marked text shows the reading while a **voluntary** commit resolves to the
  selected surface, which affects Enter too and contradicts SPEC
  § 各状態でのキー操作 — is #309, still open and needing accuracy measurement.
  Note the narrow scope of (a): it settles *whether to commit or discard*, not
  whether the `resetDisplay()` gap is acceptable, and not whether the no-learn
  default is right.
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
