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

    // revertToLeximeIfEscapeRaced delegates to the shared reverter with the
    // Lexime *Japanese* mode as the target: the ESC race interrupts active
    // Japanese composing, so recovery must land back on Japanese (unlike
    // InputSourceMonitor, which normalizes bare ABC to Roman).
    do {
        let fake = FakeAbcReverter()
        let mc = ModeController(reverter: fake)
        mc.revertToLeximeIfEscapeRaced()
        assertEqual(fake.revertCalls, [LeximeInputSourceID.japanese],
                    "ESC-race revert targets the Japanese mode")
        mc.revertToLeximeIfEscapeRaced()
        assertEqual(fake.revertCalls.count, 2,
                    "each ESC-race recovery delegates one revert")
    }

    // NOTE: The live retry loop (InputSourceReverter) calls TIS APIs and
    // spawns DispatchQueue.main.asyncAfter work. Exercising it in a CLI test
    // process would mutate the user's real input source and leak timers past
    // test teardown, so tests cover the delegation boundary (above) and the
    // pure revert policy (TestAbcRevertPolicy), not the TIS-side loop.
}
