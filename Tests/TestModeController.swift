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

    // revertToLeximeIfEscapeRaced with attempt >= max exits immediately:
    // no asyncAfter is scheduled, so calling it is safe in a headless test.
    // We cannot observe the guard directly (no scheduler injection here), so
    // this is a smoke check that the call returns synchronously without crashing.
    do {
        let mc = ModeController()
        mc.revertToLeximeIfEscapeRaced(attempt: 100)
    }

    // NOTE: The live retry path of `revertToLeximeIfEscapeRaced(attempt: 0)`
    // calls TIS APIs and spawns DispatchQueue.main.asyncAfter work. Exercising
    // it in a CLI test process would mutate the user's real input source and
    // leak timers past test teardown, so we cover only the input-independent
    // escape-flag state machine here. A full integration test would need a TIS
    // fake layer, which is out of scope for this PR.
}
