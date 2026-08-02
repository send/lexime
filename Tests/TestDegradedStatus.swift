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
        runtimeOnly.first?.contains("削除") ?? false,
        "the row must name the deletion, not just 'degraded'")

    // S2. Both sources, both rendered, init failures first.
    let both = DegradedStatus.rows(
        initFailures: [.historyDataLoss(detail: "x")],
        runtimeIssues: [.deletionNotPersisted, .learningMemoryOnly])
    // Compared as a whole rather than by index: a wrong count would otherwise
    // trap on subscript and take the whole runner down with it, hiding every
    // later test behind a crash instead of a named failure.
    assertEqual(
        both,
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

    // It also co-occurs with a quarantine: independent facts about one startup,
    // and EngineContainer appends it outside the mutually exclusive chain so
    // neither can mask the other.
    assertEqual(
        DegradedStatus.rows(
            initFailures: [.historyDeletionLost(detail: "x"), .historyDataLoss(detail: "y")],
            runtimeIssues: []
        ).count,
        2,
        "a lost deletion and a quarantine are independent")
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
