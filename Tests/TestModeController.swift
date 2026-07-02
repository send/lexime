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
    // InputSourceMonitor, which normalizes bare ABC to Roman). It must use
    // the *watch* variant because the IMKit flip to ABC lands asynchronously
    // after the ESC commit.
    do {
        let fake = FakeAbcReverter()
        let mc = ModeController(reverter: fake)
        mc.revertToLeximeIfEscapeRaced()
        assertEqual(fake.calls, [.revertWhenAbcAppears(LeximeInputSourceID.japanese)],
                    "ESC-race recovery watches for ABC and targets Japanese")
        mc.revertToLeximeIfEscapeRaced()
        assertEqual(fake.calls.count, 2,
                    "each ESC-race recovery delegates one revert")
    }

    // NOTE: The shared retry loop itself is covered by TestInputSourceReverter
    // (with injected TIS/scheduler fakes) and the ownership policy by
    // TestAbcRevertPolicy; this file covers ModeController's delegation
    // boundary and the escape-flag state machine only.
}
