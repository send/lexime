import Foundation

/// Drives an InputSourceReverter with fake TIS calls and a manual scheduler
/// so the retry loop's tick semantics can be tested synchronously.
private final class ReverterHarness {
    var currentID: String
    /// When false, a select is recorded but the current source is unchanged
    /// (simulates TISSelectInputSource failing silently).
    var selectSucceeds = true
    var selectCalls: [String] = []
    private var pendingTicks: [() -> Void] = []
    private(set) var reverter: InputSourceReverter!

    init(startingOn id: String) {
        currentID = id
        reverter = InputSourceReverter(
            isCurrentStandardABC: { [unowned self] in
                self.currentID == LeximeInputSourceID.standardABC
            },
            selectSource: { [unowned self] id in
                self.selectCalls.append(id)
                if self.selectSucceeds { self.currentID = id }
            },
            scheduleTick: { [unowned self] work in
                self.pendingTicks.append(work)
            }
        )
    }

    var hasPendingTicks: Bool { !pendingTicks.isEmpty }

    /// Run the next scheduled tick, if any.
    @discardableResult
    func runNextTick() -> Bool {
        guard !pendingTicks.isEmpty else { return false }
        pendingTicks.removeFirst()()
        return true
    }

    /// Run scheduled ticks until the session stops scheduling; returns the
    /// number of ticks run (bounded to catch runaway loops).
    func runToCompletion(bound: Int = 100) -> Int {
        var ran = 0
        while ran < bound && runNextTick() { ran += 1 }
        return ran
    }
}

func testInputSourceReverter() {
    print("--- InputSourceReverter Tests ---")

    let abc = LeximeInputSourceID.standardABC
    let japanese = LeximeInputSourceID.japanese
    let roman = LeximeInputSourceID.roman
    let otherIME = "com.google.inputmethod.Japanese.base"

    // Watch-window coverage: the last check must land at >= 0.25s so the
    // consolidated loop covers the pre-consolidation ESC race window
    // ([0.05s, 0.25s] in the old ModeController loop).
    assertTrue(
        Double(InputSourceReverter.maxAttempts - 1) * InputSourceReverter.retryInterval
            >= 0.25 - 1e-9,
        "retry window covers the old ESC race watch window through 0.25s")

    // revertFromAbc: happy path — ABC is current, select succeeds, and the
    // session ends at the next tick without further selects.
    do {
        let h = ReverterHarness(startingOn: abc)
        h.reverter.revertFromAbc(to: roman)
        assertEqual(h.selectCalls, [roman], "immediate select on ABC")
        assertEqual(h.currentID, roman, "revert took effect")
        _ = h.runToCompletion()
        assertEqual(h.selectCalls, [roman], "no extra selects after success")
    }

    // revertFromAbc: silent select failure — retries on every tick of the
    // window while the source stays ABC.
    do {
        let h = ReverterHarness(startingOn: abc)
        h.selectSucceeds = false
        h.reverter.revertFromAbc(to: roman)
        _ = h.runToCompletion()
        assertEqual(h.selectCalls.count, InputSourceReverter.maxAttempts,
                    "keeps retrying while stuck on ABC, once per tick")
    }

    // revertFromAbc: fires after the user already moved off ABC — the
    // session ends immediately, without selecting or scheduling anything.
    do {
        let h = ReverterHarness(startingOn: otherIME)
        h.reverter.revertFromAbc(to: roman)
        assertEqual(h.selectCalls, [], "no select when no longer on ABC")
        assertTrue(!h.hasPendingTicks, "no ticks scheduled after immediate bail")
    }

    // revertFromAbc: a deliberate return to ABC inside the retry window must
    // not be stolen by the stale session (regression: the session must stop
    // once a non-ABC source is observed).
    do {
        let h = ReverterHarness(startingOn: abc)
        h.reverter.revertFromAbc(to: roman)          // tick 0: select succeeds
        h.currentID = otherIME                       // user moves on...
        h.runNextTick()                              // tick 1: non-ABC -> session over
        assertTrue(!h.hasPendingTicks, "session stops after leaving ABC")
        h.currentID = abc                            // ...then deliberately picks ABC
        _ = h.runToCompletion()
        assertEqual(h.selectCalls, [roman],
                    "re-selected ABC is not reclaimed by the stale session")
    }

    // revertWhenAbcAppears: the ESC race lands mid-window — early non-ABC
    // ticks keep the watch alive, the flip is caught and reverted, then the
    // session ends.
    do {
        let h = ReverterHarness(startingOn: japanese)
        h.reverter.revertWhenAbcAppears(to: japanese)
        assertEqual(h.selectCalls, [], "no select before the race lands")
        h.runNextTick()                              // tick 1: still Japanese
        h.runNextTick()                              // tick 2: still Japanese
        assertTrue(h.hasPendingTicks, "watch survives non-ABC ticks")
        h.currentID = abc                            // the race lands
        h.runNextTick()                              // tick 3: caught
        assertEqual(h.selectCalls, [japanese], "race flip reverted to Japanese")
        assertEqual(h.currentID, japanese, "back on Japanese")
        _ = h.runToCompletion()
        assertEqual(h.selectCalls, [japanese], "session ends after recovery")
    }

    // revertWhenAbcAppears: the race lands on the very last tick of the
    // window (the 0.20-0.25s coverage this loop must preserve).
    do {
        let h = ReverterHarness(startingOn: japanese)
        h.reverter.revertWhenAbcAppears(to: japanese)
        for _ in 0..<(InputSourceReverter.maxAttempts - 2) { h.runNextTick() }
        assertTrue(h.hasPendingTicks, "one tick left at the end of the window")
        h.currentID = abc
        h.runNextTick()                              // final tick
        assertEqual(h.selectCalls, [japanese], "race caught on the final tick")
        assertTrue(!h.hasPendingTicks, "window exhausted afterwards")
    }

    // revertWhenAbcAppears: the race never lands — the watch expires without
    // ever selecting, after exactly maxAttempts - 1 scheduled ticks.
    do {
        let h = ReverterHarness(startingOn: japanese)
        h.reverter.revertWhenAbcAppears(to: japanese)
        let ran = h.runToCompletion()
        assertEqual(ran, InputSourceReverter.maxAttempts - 1,
                    "watch runs the full window")
        assertEqual(h.selectCalls, [], "no select when the race never lands")
    }
}
