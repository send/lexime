import Foundation

func testDegradedStatus() {
    // S1. The main #295 scenario is a clean launch followed by a disk that
    // fails hours later: no init failure, one runtime issue. Gating the rows
    // on "is the engine degraded" — as menu() once did — shows nothing here.
    let runtimeOnly = DegradedStatus.rows(
        initFailures: [],
        runtimeIssues: [.deletionNotPersisted])
    assertEqual(runtimeOnly.count, 1, "a runtime issue shows without any init failure")
    assertTrue(
        runtimeOnly.first?.title.contains("削除") ?? false,
        "the row must name the deletion, not just 'degraded'")
    assertTrue(
        !(runtimeOnly.first?.acknowledgeable ?? true),
        "a polled row has no durable record to acknowledge")

    // S2. Both sources, both rendered, init failures first.
    let both = DegradedStatus.rows(
        initFailures: [.historyDataLoss(detail: "x")],
        runtimeIssues: [.deletionNotPersisted, .learningMemoryOnly])
    // Compared as a whole rather than by index: a wrong count would otherwise
    // trap on subscript and take the whole runner down with it, hiding every
    // later test behind a crash instead of a named failure.
    assertEqual(
        both.map { $0.title },
        [
            DegradedStatus.title(for: EngineInitFailure.historyDataLoss(detail: "x")),
            DegradedStatus.title(for: LexHistoryDurabilityIssue.deletionNotPersisted),
            DegradedStatus.title(for: LexHistoryDurabilityIssue.learningMemoryOnly),
        ],
        "init failures first, then every runtime issue")

    // Nothing to report renders nothing — the menu adds no separator either.
    assertTrue(
        DegradedStatus.rows(initFailures: [], runtimeIssues: []).isEmpty,
        "a healthy engine shows no status rows")

    // The two issues must not collapse to the same string: on a failing volume
    // both hold at once, and two identical rows would read as a rendering bug.
    assertTrue(
        DegradedStatus.title(for: LexHistoryDurabilityIssue.deletionNotPersisted)
            != DegradedStatus.title(for: LexHistoryDurabilityIssue.learningMemoryOnly),
        "the two runtime issues need distinguishable rows")

    // S3 (#312). A disk that is still failing shows the past loss and the live
    // one together — the steady state, not a corner. They must not read as the
    // same sentence twice: one says a save is failing now, the other that one
    // already failed and the entry may be back.
    let acrossRestart = DegradedStatus.rows(
        initFailures: [.historyDeletionLost(detail: "x")],
        runtimeIssues: [.deletionNotPersisted])
    assertEqual(acrossRestart.count, 2, "a past loss and a live one are two rows")
    assertTrue(
        DegradedStatus.title(for: EngineInitFailure.historyDeletionLost(detail: "x"))
            != DegradedStatus.title(for: LexHistoryDurabilityIssue.deletionNotPersisted),
        "the latching row must not duplicate the polled one")
    assertTrue(
        DegradedStatus.title(for: EngineInitFailure.historyDeletionLost(detail: "x"))
            .contains("前回"),
        "the latching row must place the loss in a previous session")

    // It also co-occurs with a quarantine: independent facts about one startup.
    // `rows` cannot establish that — it maps whatever list it is handed, so
    // asserting over it would only be testing Array.map. The claim that matters
    // is EngineContainer's, that the lost-deletion case is appended outside its
    // mutually exclusive branch chain, which is what S4 below tests directly.

    // S4 (#312). A lost deletion must survive alongside a quarantine, which is
    // the branch chain's masking case: routing it through the chain would let
    // dataLossSuspected swallow it, and no assertion over `rows` could see it.
    let coexisting = EngineContainer.historyFailures(
        deletionLost: true, dataLossSuspected: true, detail: "d", deletionDetail: "x")
    assertEqual(coexisting.count, 2, "a quarantine must not mask the lost deletion")
    assertTrue(
        coexisting.contains {
            if case .historyDeletionLost = $0 { return true } else { return false }
        },
        "the lost deletion is one of them")
    assertEqual(
        EngineContainer.historyFailures(
            deletionLost: false, dataLossSuspected: true, detail: "d", deletionDetail: "x"
        ).count,
        1,
        "a quarantine alone is one row")
    assertTrue(
        EngineContainer.historyFailures(
            deletionLost: false, dataLossSuspected: false, detail: "d", deletionDetail: "x"
        ).isEmpty,
        "a clean start reports nothing")

    // S4b (#312). Exactly one row is clickable, and it is the one backed by a
    // file. IMKit builds this menu on its own without displaying it — measured
    // on-device, the record was consumed four seconds after an untouched
    // relaunch — so a click is the only evidence delivery actually happened.
    let acknowledgeable = acrossRestart.filter { $0.acknowledgeable }
    assertEqual(acknowledgeable.count, 1, "only the lost-deletion row is acknowledgeable")
    assertEqual(
        acknowledgeable.first?.title,
        DegradedStatus.title(for: EngineInitFailure.historyDeletionLost(detail: "x")),
        "and it is that row, not another")
    assertTrue(
        DegradedStatus.rows(initFailures: [.historyDataLoss(detail: "x")], runtimeIssues: [])
            .allSatisfy { !$0.acknowledgeable },
        "a quarantine latches but has no durable record a click could retire")

    // S5 (#312). A wipe retracts the latched row — the row asks the user to
    // delete an entry that no longer exists — and nothing else.
    //
    // Against `retractDeletionLostRow` directly, which is what production
    // reaches. It used to go through a `historyWasCleared()` wrapper that no
    // production path ever called: the wipe runs
    // `EngineControlService.clearHistory`'s `defer { retractRowIfSettled() }`,
    // and the interesting half is that gate — it asks the engine whether the
    // report is still owed rather than inferring it from which action ran, so
    // a wipe that throws before its commit point, or one whose unlink failed,
    // keeps the row. Pinning the wrapper vouched for the unconditional shape
    // that gate replaced.
    let container = EngineContainer(
        engine: nil, dictionary: nil, history: nil, userDict: nil,
        initFailures: [.historyDeletionLost(detail: "x"), .historyDataLoss(detail: "y")])
    container.retractDeletionLostRow()
    assertEqual(container.initFailures.count, 1, "only the lost-deletion row is retracted")
    assertTrue(
        container.initFailures.contains {
            if case .historyDataLoss = $0 { return true } else { return false }
        },
        "an unrelated latched failure survives a history wipe")
}

func testHistoryDurabilityFFI() {
    // Round-trip across the UniFFI boundary: a healthy history reports
    // nothing. (The failure combinations are pinned by the engine's F1-F15
    // tests, which can inject WAL I/O faults; this one pins that the call
    // exists, returns a Swift array, and is callable from the menu path.)
    let dir = NSTemporaryDirectory() + "lexime-durability-\(ProcessInfo.processInfo.processIdentifier)"
    try? FileManager.default.createDirectory(
        atPath: dir, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(atPath: dir) }

    guard let history = try? LexUserHistory.open(path: dir + "/user_history.lxud") else {
        assertTrue(false, "opening a fresh history must succeed")
        return
    }
    assertTrue(history.durabilityIssues().isEmpty, "a fresh history has no issues")
}
