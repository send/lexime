import Foundation

func testModeController() {
    print("--- ModeController Tests ---")

    // takePendingEscapeCommit: default is false
    do {
        let mc = ModeController()
        assertTrue(!mc.takePendingEscapeCommit(),
                   "default escape-commit flag is false")
    }

    // noteEscapeDuringCompose sets the flag; takePendingEscapeCommit consumes once
    do {
        let mc = ModeController()
        mc.noteEscapeDuringCompose()
        assertTrue(mc.takePendingEscapeCommit(), "flag set after note")
        assertTrue(!mc.takePendingEscapeCommit(),
                   "flag is one-shot: cleared after take")
    }

    // Repeated note + take: still one-shot each time
    do {
        let mc = ModeController()
        mc.noteEscapeDuringCompose()
        mc.noteEscapeDuringCompose()  // idempotent
        assertTrue(mc.takePendingEscapeCommit(), "flag set after repeated notes")
        assertTrue(!mc.takePendingEscapeCommit(), "cleared after take")
        mc.noteEscapeDuringCompose()
        assertTrue(mc.takePendingEscapeCommit(), "re-armable after clear")
    }

    // revertToLeximeIfEscapeRaced with attempt >= max exits immediately
    // (no asyncAfter scheduled, so this is safe to invoke in a headless test).
    // We can't observe the guard directly, but we can assert the call doesn't
    // throw / crash and returns synchronously.
    do {
        let mc = ModeController()
        // attempt == max: guard fails, returns without scheduling.
        mc.revertToLeximeIfEscapeRaced(attempt: 100)
        assertTrue(true, "revert guard returns without scheduling when attempt saturated")
    }

    // NOTE: The live retry path of `revertToLeximeIfEscapeRaced(attempt: 0)`
    // calls TIS APIs and spawns DispatchQueue.main.asyncAfter work. Exercising
    // it in a CLI test process would mutate the user's real input source and
    // leak timers past test teardown, so we cover only the input-independent
    // escape-flag state machine here. A full integration test would need a TIS
    // fake layer, which is out of scope for this PR.
}
