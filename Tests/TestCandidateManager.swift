import Cocoa
import Foundation
import InputMethodKit

func testCandidateManager() {
    print("--- CandidateManager Tests ---")

    // invalidate() bumps generation monotonically
    do {
        let m = CandidateManager(panel: FakePanel())
        let g0 = m.generation
        m.invalidate()
        let g1 = m.generation
        m.invalidate()
        let g2 = m.generation
        assertTrue(g1 == g0 &+ 1, "generation +1 after invalidate")
        assertTrue(g2 == g1 &+ 1, "generation +1 after second invalidate")
    }

    // update() records surfaces + selected
    do {
        let m = CandidateManager(panel: FakePanel())
        m.update(surfaces: ["あ", "い", "う"], selected: 1)
        assertEqual(m.candidates, ["あ", "い", "う"], "candidates stored")
        assertEqual(m.selectedIndex, 1, "selectedIndex stored")
    }

    // reset() clears candidates, preserves generation
    do {
        let m = CandidateManager(panel: FakePanel())
        m.update(surfaces: ["x"], selected: 0)
        m.invalidate()
        let gen = m.generation
        m.reset()
        assertEqual(m.candidates, [], "reset clears candidates")
        assertEqual(m.selectedIndex, 0, "reset clears selectedIndex")
        assertEqual(m.generation, gen, "reset preserves generation")
    }

    // deactivate() invalidates + hides
    do {
        let panel = FakePanel()
        panel.visible = false
        let m = CandidateManager(panel: panel)
        let g0 = m.generation
        m.deactivate()
        assertTrue(m.generation == g0 &+ 1, "deactivate invalidates")
        assertEqual(panel.hideCalls, 1, "deactivate hides panel")
    }

    // hide() forwards to panel
    do {
        let panel = FakePanel()
        panel.visible = false
        let m = CandidateManager(panel: panel)
        m.hide()
        assertEqual(panel.hideCalls, 1, "hide forwards to panel")
    }

    // show() with empty candidates → hide, not show
    do {
        let panel = FakePanel()
        panel.visible = false
        let m = CandidateManager(panel: panel)
        // Do not call update (candidates stays empty).
        m.show(client: FakeIMKClient(), currentDisplay: nil)
        assertEqual(panel.showCalls.count, 0, "empty candidates: no show")
        assertEqual(panel.hideCalls, 1, "empty candidates: hide instead")
    }

    // show() with visible panel short-circuits (no deferred dispatch, no rect capture)
    do {
        let panel = FakePanel()
        panel.visible = true
        let m = CandidateManager(panel: panel)
        m.update(surfaces: ["一", "二"], selected: 0)
        m.show(client: FakeIMKClient(), currentDisplay: "いち")
        assertEqual(panel.showCalls.count, 1, "visible path calls show immediately")
        assertEqual(panel.showCalls[0].candidates, ["一", "二"], "visible: candidates page")
        assertEqual(panel.showCalls[0].hasCursorRect, false, "visible: no cursor rect recomputed")
    }

    // flagReposition forces full path even when already visible
    do {
        let panel = FakePanel()
        panel.visible = true
        let m = CandidateManager(panel: panel)
        m.update(surfaces: ["a"], selected: 0)
        m.flagReposition()
        m.show(client: FakeIMKClient(), currentDisplay: nil)
        // Deferred path schedules to runloop; drain once so the async block fires.
        runLoopSpin()
        assertTrue(panel.showCalls.count >= 1, "flagReposition triggers full path show")
        assertTrue(panel.showCalls.last?.hasCursorRect == true,
                   "flagReposition: cursor rect recomputed")
    }

    // Generation bump between show() scheduling and runloop drain cancels the deferred show
    do {
        let panel = FakePanel()
        panel.visible = false  // forces deferred path
        let m = CandidateManager(panel: panel)
        m.update(surfaces: ["x"], selected: 0)
        m.show(client: FakeIMKClient(), currentDisplay: nil)
        m.invalidate()  // bumps generation before deferred block runs
        runLoopSpin()
        assertEqual(panel.showCalls.count, 0, "generation mismatch cancels deferred show")
    }

    // Pagination: selected index past page boundary slices the correct page
    do {
        let panel = FakePanel()
        panel.visible = true  // synchronous path for determinism
        let m = CandidateManager(panel: panel)
        let surfaces = (0..<20).map { "c\($0)" }
        m.update(surfaces: surfaces, selected: 10)  // page 1 (of size 9)
        m.show(client: FakeIMKClient(), currentDisplay: nil)
        let call = panel.showCalls[0]
        // page 1 = indices 9..<18
        assertEqual(call.candidates, Array(surfaces[9..<18]), "page 1 candidates")
        assertEqual(call.selectedIndex, 1, "selected within page")
        assertEqual(call.globalIndex, 10, "global index preserved")
        assertEqual(call.totalCount, 20, "total count preserved")
    }
}

/// Pump the main run loop briefly so DispatchQueue.main.async blocks execute.
private func runLoopSpin() {
    RunLoop.current.run(until: Date().addingTimeInterval(0.01))
}
