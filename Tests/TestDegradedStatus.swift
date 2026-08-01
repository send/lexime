import Foundation

func testDegradedStatus() {
    // S1. The main #295 scenario is a clean launch followed by a disk that
    // fails hours later: no init failure, one runtime issue. The rows used to
    // be built inside an `isDegraded` check, which shows nothing here.
    let runtimeOnly = DegradedStatus.rows(
        initFailures: [],
        runtimeIssues: [.deletionNotPersisted])
    assertEqual(runtimeOnly.count, 1, "a runtime issue shows without any init failure")
    assertTrue(
        runtimeOnly[0].contains("削除"),
        "the row must name the deletion, not just 'degraded'")

    // S2. Both sources, both rendered, init failures first.
    let both = DegradedStatus.rows(
        initFailures: [.historyDataLoss(detail: "x")],
        runtimeIssues: [.deletionNotPersisted, .learningMemoryOnly])
    assertEqual(both.count, 3)
    assertEqual(both[0], DegradedStatus.title(for: EngineInitFailure.historyDataLoss(detail: "x")))
    assertEqual(both[1], DegradedStatus.title(for: LexHistoryDurabilityIssue.deletionNotPersisted))
    assertEqual(both[2], DegradedStatus.title(for: LexHistoryDurabilityIssue.learningMemoryOnly))

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
