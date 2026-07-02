import Foundation

func testAbcRevertPolicy() {
    print("--- AbcRevertPolicy Tests ---")

    let abc = LeximeInputSourceID.standardABC
    let japanese = LeximeInputSourceID.japanese
    let roman = LeximeInputSourceID.roman
    let otherIME = "com.google.inputmethod.Japanese.base"

    // No prior observation: ABC out of nowhere is not reverted.
    do {
        let policy = AbcRevertPolicy()
        assertTrue(!policy.shouldRevert(currentID: abc),
                   "no prior source: leave ABC alone")
    }

    // Lexime Japanese → ABC: revert (IMKit race / engine switch shape).
    do {
        var policy = AbcRevertPolicy()
        policy.observe(japanese)
        assertTrue(policy.shouldRevert(currentID: abc),
                   "Japanese → ABC is reverted")
    }

    // Lexime Roman → ABC: revert.
    do {
        var policy = AbcRevertPolicy()
        policy.observe(roman)
        assertTrue(policy.shouldRevert(currentID: abc),
                   "Roman → ABC is reverted")
    }

    // Other IME → ABC: deliberate user choice, leave it alone.
    do {
        var policy = AbcRevertPolicy()
        policy.observe(otherIME)
        assertTrue(!policy.shouldRevert(currentID: abc),
                   "other IME → ABC is respected")
    }

    // Lexime → other IME → ABC: the ABC hop came from the other IME.
    do {
        var policy = AbcRevertPolicy()
        policy.observe(japanese)
        policy.observe(otherIME)
        assertTrue(!policy.shouldRevert(currentID: abc),
                   "Lexime → other IME → ABC is respected")
    }

    // Repeated ABC observations do not clobber the pre-ABC source: duplicate
    // change notifications for one switch keep the original attribution.
    do {
        var policy = AbcRevertPolicy()
        policy.observe(otherIME)
        policy.observe(abc)
        policy.observe(abc)
        assertEqual(policy.lastNonAbcSourceID, otherIME,
                    "ABC observations keep the pre-ABC source")
        assertTrue(!policy.shouldRevert(currentID: abc),
                   "still respected across repeated ABC notifications")
    }

    // nil observations (TIS lookup failure) are ignored.
    do {
        var policy = AbcRevertPolicy()
        policy.observe(japanese)
        policy.observe(nil)
        assertTrue(policy.shouldRevert(currentID: abc),
                   "nil observation does not clobber the pre-ABC source")
    }

    // Non-ABC current source never triggers a revert.
    do {
        var policy = AbcRevertPolicy()
        policy.observe(japanese)
        assertTrue(!policy.shouldRevert(currentID: roman),
                   "non-ABC current source is never reverted")
        assertTrue(!policy.shouldRevert(currentID: nil),
                   "nil current source is never reverted")
    }

    // Full cycle: race reverted to Roman, then a second race is still caught.
    do {
        var policy = AbcRevertPolicy()
        policy.observe(japanese)
        assertTrue(policy.shouldRevert(currentID: abc), "first race reverted")
        policy.observe(abc)     // the race's own change notification
        policy.observe(roman)   // our revert landing on Lexime Roman
        assertTrue(policy.shouldRevert(currentID: abc),
                   "second race after recovery is reverted again")
    }

    // Deferred-revert ownership re-check (secure input regression): a revert
    // scheduled for a Lexime -> ABC transition can fire much later (up to the
    // 60s secure-input poll). If the user meanwhile moved to another source
    // and then deliberately re-selected ABC, re-evaluating the policy at fire
    // time must now say "leave it alone".
    do {
        var policy = AbcRevertPolicy()
        policy.observe(japanese)
        policy.observe(abc)       // Lexime -> ABC detected, revert deferred
        assertTrue(policy.shouldRevert(currentID: abc),
                   "claim valid when the revert was scheduled")
        policy.observe(otherIME)  // user moves on during the wait...
        policy.observe(abc)       // ...then deliberately re-selects ABC
        assertTrue(!policy.shouldRevert(currentID: abc),
                   "claim lapsed by fire time: deferred revert must be skipped")
    }
}
