import Cocoa
import Foundation
import InputMethodKit

/// Build a coordinator wired to a FakeLexSession. The factory receives the
/// listener the coordinator would normally hand to Rust; tests ignore it since
/// nothing drives async responses here.
private func makeCoordinator(
    session: FakeLexSession,
    panel: FakePanel = FakePanel(),
    onSwitchToAbc: @escaping () -> Void = {}
) -> (SessionCoordinator, CandidateManager) {
    let manager = CandidateManager(panel: panel)
    let coordinator = SessionCoordinator(
        factory: { _ in session },
        candidateManager: manager,
        onSwitchToAbc: onSwitchToAbc)
    return (coordinator, manager)
}

func testSessionCoordinator() {
    print("--- SessionCoordinator Tests ---")

    // handleKey: forwards key event to session + returns consumed flag
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [LexKeyResponse(consumed: true, events: [])]
        let (coordinator, _) = makeCoordinator(session: session)

        let client = FakeIMKClient()
        let consumed = coordinator.handleKey(.space, client: client)
        assertTrue(consumed, "handleKey returns response.consumed")
        assertEqual(session.handleKeyCalls.count, 1, "session.handleKey called once")
        assertEqual(session.handleKeyCalls[0], LexKeyEvent.space, "forwarded event matches")
    }

    // handleKey: bumps candidate generation (invalidates stale async work)
    do {
        let session = FakeLexSession()
        let (coordinator, manager) = makeCoordinator(session: session)
        let before = manager.generation
        _ = coordinator.handleKey(.space, client: FakeIMKClient())
        assertTrue(manager.generation == before &+ 1,
                   "handleKey invalidates candidate generation")
    }

    // .commit event → client.insertText + currentDisplay cleared
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [.commit(text: "漢字")])
        ]
        let (coordinator, _) = makeCoordinator(session: session)
        let client = FakeIMKClient()
        _ = coordinator.handleKey(.enter, client: client)
        assertEqual(client.insertCalls.count, 1, "commit → one insertText")
        assertEqual(client.insertCalls[0].text, "漢字", "commit text passed through")
        // Match the live NSRange(location: NSNotFound, length: 0) literal.
        assertEqual(client.insertCalls[0].replacementRange.location, NSNotFound,
                    "commit uses replacementRange at NSNotFound")
        assertTrue(coordinator.currentDisplay == nil, "commit clears currentDisplay")
    }

    // .setMarkedText event (non-empty) → client.setMarkedText + currentDisplay updated
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [.setMarkedText(text: "あ")])
        ]
        let (coordinator, _) = makeCoordinator(session: session)
        let client = FakeIMKClient()
        _ = coordinator.handleKey(.text(text: "a", shift: false), client: client)
        assertEqual(client.markedCalls.count, 1, "setMarkedText called")
        assertEqual(client.markedCalls[0].text, "あ", "marked text passed through")
        assertEqual(coordinator.currentDisplay, "あ", "currentDisplay tracks marked text")
    }

    // .setMarkedText with empty string clears currentDisplay (nil, not "")
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [
                .setMarkedText(text: "あ"),
                .setMarkedText(text: ""),
            ])
        ]
        let (coordinator, _) = makeCoordinator(session: session)
        _ = coordinator.handleKey(.backspace, client: FakeIMKClient())
        assertTrue(coordinator.currentDisplay == nil,
                   "empty marked text → currentDisplay nil")
    }

    // .commit followed by .setMarkedText → the marked text survives the commit
    do {
        // Pins the two currentDisplay writers that lex-session's proptest models
        // (`proptest_fsm::HostMarked`): .commit clears it, .setMarkedText sets
        // it, applied in event order. This is the pair auto-commit emits — the
        // stable prefix is inserted while the remainder stays marked — so if
        // applying them left currentDisplay nil, the session would still be
        // composing with the host dropped out of composition, which is what
        // leaks the next key on Chromium/Electron hosts. The Rust-side ordering
        // that produces this pair is pinned separately by
        // `api::mapping::commit_precedes_marked_text`.
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [
                .commit(text: "今日"),
                .setMarkedText(text: "は"),
            ])
        ]
        let (coordinator, _) = makeCoordinator(session: session)
        let client = FakeIMKClient()
        _ = coordinator.handleKey(.text(text: "h", shift: false), client: client)
        assertEqual(client.insertCalls.count, 1, "commit → one insertText")
        assertEqual(client.insertCalls[0].text, "今日", "commit text passed through")
        assertEqual(client.markedCalls.count, 1, "marked text applied after commit")
        assertEqual(client.markedCalls[0].text, "は", "client gets the marked text, not just the shadow")
        assertEqual(coordinator.currentDisplay, "は",
                    "marked text after a commit leaves the host composing")
    }

    // .showCandidates → CandidateManager populated + panel.show called
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [
                .showCandidates(surfaces: ["一", "二"], selected: 0)
            ])
        ]
        let panel = FakePanel()
        panel.visible = true
        let (coordinator, manager) = makeCoordinator(session: session, panel: panel)
        _ = coordinator.handleKey(.space, client: FakeIMKClient())
        assertEqual(manager.candidates, ["一", "二"], "candidates applied")
        assertEqual(manager.selectedIndex, 0, "selected applied")
        assertTrue(panel.showCount >= 1, "panel.show called for candidates")
    }

    // .hideCandidates → panel.hide
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [.hideCandidates])
        ]
        let panel = FakePanel()
        let (coordinator, _) = makeCoordinator(session: session, panel: panel)
        _ = coordinator.handleKey(.escape, client: FakeIMKClient())
        assertTrue(panel.hideCount >= 1, "hideCandidates → panel.hide")
    }

    // .switchToAbc → onSwitchToAbc closure fired
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [.switchToAbc])
        ]
        var switched = 0
        let (coordinator, _) = makeCoordinator(session: session, onSwitchToAbc: {
            switched += 1
        })
        _ = coordinator.handleKey(.switchToDirectInput, client: FakeIMKClient())
        assertEqual(switched, 1, "switchToAbc event triggers closure")
    }

    // commit(client:) forwards to session.commit + applies events
    do {
        let session = FakeLexSession()
        session.commitResponses = [
            LexKeyResponse(consumed: true, events: [.commit(text: "あ")])
        ]
        let (coordinator, _) = makeCoordinator(session: session)
        let client = FakeIMKClient()
        coordinator.commit(client: client)
        assertEqual(session.commitCalls, 1, "session.commit called")
        assertEqual(client.insertCalls.count, 1, "commit response events applied")
        assertEqual(client.insertCalls[0].text, "あ", "committed text")
    }

    // Session passthroughs
    do {
        let session = FakeLexSession()
        let (coordinator, _) = makeCoordinator(session: session)
        session.isComposingValue = true
        assertTrue(coordinator.isComposing, "isComposing forwarded")

        coordinator.setSnippetStore(nil)
        assertEqual(session.setSnippetStoreCalls, 1, "setSnippetStore forwarded")

        coordinator.setAbcPassthrough(enabled: true)
        assertEqual(session.setAbcPassthroughCalls, [true],
                    "setAbcPassthrough forwarded")
    }

    // resetDisplay clears currentDisplay without side effects
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [.setMarkedText(text: "x")])
        ]
        let (coordinator, _) = makeCoordinator(session: session)
        _ = coordinator.handleKey(.text(text: "x", shift: false), client: FakeIMKClient())
        assertEqual(coordinator.currentDisplay, "x", "precondition: display set")
        coordinator.resetDisplay()
        assertTrue(coordinator.currentDisplay == nil, "resetDisplay clears display")
    }

    // Async response with a stale epoch is dropped (main-queue reordering guard):
    // a response accepted by Rust before a key was processed must not re-apply
    // its events after that key's response already reached the UI.
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, epoch: 5, events: [.hideCandidates])
        ]
        let panel = FakePanel()
        let (coordinator, manager) = makeCoordinator(session: session, panel: panel)
        // Hold the client strongly so the drop below is provably caused by
        // the epoch guard, not by the weak lastClient having gone away.
        let client = FakeIMKClient()
        // Key response (epoch 5) applied inline → watermark advances to 5.
        _ = coordinator.handleKey(.escape, client: client)
        assertTrue(coordinator.highestAppliedEpoch == 5,
                   "sync key response advances epoch watermark")

        withExtendedLifetime(client) {
            // Async response from before the key (epoch 4) arrives late from
            // the main queue → must be silently dropped.
            coordinator.applyAsyncResponse(LexKeyResponse(
                consumed: true, epoch: 4,
                events: [.showCandidates(surfaces: ["古い候補"], selected: 0)]))
            assertTrue(manager.candidates.isEmpty,
                       "stale-epoch async response must not update candidates")
            assertEqual(panel.showCount, 0,
                        "stale-epoch async response must not re-show the panel")
            assertTrue(coordinator.highestAppliedEpoch == 5,
                       "dropped response must not move the watermark")
        }
    }

    // Async response with a fresh epoch (>= watermark) applies normally.
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, epoch: 5, events: [])
        ]
        let panel = FakePanel()
        let (coordinator, manager) = makeCoordinator(session: session, panel: panel)
        // Keep the client alive across applyAsyncResponse: the coordinator
        // holds it weakly (lastClient) and needs it to apply events.
        let client = FakeIMKClient()
        _ = coordinator.handleKey(.space, client: client)

        withExtendedLifetime(client) {
            coordinator.applyAsyncResponse(LexKeyResponse(
                consumed: true, epoch: 6,
                events: [.showCandidates(surfaces: ["新", "候補"], selected: 0)]))
            assertEqual(manager.candidates, ["新", "候補"],
                        "fresh async response applies candidates")
            assertTrue(coordinator.highestAppliedEpoch == 6,
                       "applied async response advances epoch watermark")
        }
    }

    // commit(client:) also advances the epoch watermark.
    do {
        let session = FakeLexSession()
        session.commitResponses = [
            LexKeyResponse(consumed: true, epoch: 7, events: [])
        ]
        let (coordinator, manager) = makeCoordinator(session: session)
        let client = FakeIMKClient()
        coordinator.commit(client: client)
        assertTrue(coordinator.highestAppliedEpoch == 7,
                   "commit response advances epoch watermark")

        withExtendedLifetime(client) {
            // In-flight async from before the commit is now stale.
            coordinator.applyAsyncResponse(LexKeyResponse(
                consumed: true, epoch: 6,
                events: [.showCandidates(surfaces: ["古"], selected: 0)]))
            assertTrue(manager.candidates.isEmpty,
                       "async response older than commit must be dropped")
        }
    }

    // deactivate: invalidates candidates, hides panel, clears display.
    // Nothing is composing here, so this covers the no-settle branch only —
    // the settling branch is covered by the block below.
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [.setMarkedText(text: "y")])
        ]
        let panel = FakePanel()
        let (coordinator, manager) = makeCoordinator(session: session, panel: panel)
        _ = coordinator.handleKey(.text(text: "y", shift: false), client: FakeIMKClient())
        let genBefore = manager.generation
        coordinator.deactivate(client: nil)
        assertTrue(manager.generation == genBefore &+ 1,
                   "deactivate invalidates generation")
        assertTrue(panel.hideCount >= 1, "deactivate hides panel")
        assertTrue(coordinator.currentDisplay == nil, "deactivate clears display")
    }

    // #298: the same teardown, but on the settling branch — the one the
    // no-settle block above cannot reach. The generation must be bumped
    // *before* the settle applies its events: a deferred panel show queued by
    // the last keystroke is guarded by `generation`, not by the epoch
    // watermark, so if the settle ran first that block could still re-show the
    // shared panel for a composition being committed right now.
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [
                .setMarkedText(text: "にほんご"),
                .showCandidates(surfaces: ["日本語", "二本語"], selected: 0),
            ])
        ]
        session.settleUnconfirmedResponses = [
            LexKeyResponse(consumed: true, events: [.commit(text: "にほんご")])
        ]
        let panel = FakePanel()
        let client = FakeIMKClient()
        let (coordinator, manager) = makeCoordinator(session: session, panel: panel)
        _ = coordinator.handleKey(.text(text: "o", shift: false), client: client)
        session.isComposingValue = true
        let genBefore = manager.generation

        coordinator.deactivate(client: client)

        assertTrue(manager.generation == genBefore &+ 1,
                   "the settling branch invalidates the generation too")
        assertTrue(panel.hideCount >= 1, "the settling branch hides the panel")
        assertEqual(session.settleUnconfirmedCalls, 1, "and still settles the session")
        assertTrue(client.insertCalls.contains { $0.text == "にほんご" },
                   "and still delivers what the host was showing")
        assertTrue(coordinator.currentDisplay == nil, "and still clears the display")
    }

    // #298: deactivate settles a live composition instead of orphaning it.
    // Measured 2026-08-01 — IMKit delivers deactivateServer with the session
    // still composing and, in most focus changes, never sends
    // commitComposition at all. Clearing the display while the session keeps
    // composing is the #293 leak shape reached from the Swift side.
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [.setMarkedText(text: "にほんご")])
        ]
        session.settleUnconfirmedResponses = [
            LexKeyResponse(consumed: true, events: [.commit(text: "にほんご")])
        ]
        let client = FakeIMKClient()
        let (coordinator, _) = makeCoordinator(session: session)
        _ = coordinator.handleKey(.text(text: "o", shift: false), client: client)
        assertEqual(coordinator.currentDisplay, "にほんご",
                    "precondition: composing with a display")
        session.isComposingValue = true

        coordinator.deactivate(client: client)

        assertEqual(session.settleUnconfirmedCalls, 1,
                    "deactivate settles through the focus-loss path")
        assertEqual(session.commitCalls, 0,
                    "and never through the learning commit path")
        assertTrue(client.insertCalls.contains { $0.text == "にほんご" },
                   "the text the host was showing reaches the client that had focus")
        assertTrue(coordinator.currentDisplay == nil,
                   "deactivate still clears the display")
    }

    // #298: `deactivateServer(_ sender: Any!)` is untyped, so the cast to
    // IMKTextInput is defensive — this is not an observed IMKit behaviour, and
    // `handle(_:client:)` failing the same cast would mean the IME cannot type
    // at all. `lastClient` going nil (it is weak) is the reachable case. Either
    // way the composition must still be settled rather than orphaned.
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [.setMarkedText(text: "にほんご")])
        ]
        session.settleUnconfirmedResponses = [
            LexKeyResponse(consumed: true, events: [.commit(text: "にほんご")])
        ]
        let client = FakeIMKClient()
        let (coordinator, _) = makeCoordinator(session: session)
        _ = coordinator.handleKey(.text(text: "o", shift: false), client: client)
        session.isComposingValue = true

        coordinator.deactivate(client: nil)

        assertEqual(session.settleUnconfirmedCalls, 1, "settles through lastClient")
        assertTrue(client.insertCalls.contains { $0.text == "にほんご" },
                   "the text the host was showing reaches the client it was typed into")
    }

    // PR315 Codex R3: the engine keeps no copy of what the host is showing —
    // it emits marked text but cannot see whether the response reached the
    // screen. So the coordinator must hand it `currentDisplay`, the value
    // written next to the setMarkedText call, and not let the engine guess.
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [.setMarkedText(text: "にほんご")])
        ]
        let client = FakeIMKClient()
        let (coordinator, _) = makeCoordinator(session: session)
        _ = coordinator.handleKey(.text(text: "o", shift: false), client: client)
        assertEqual(coordinator.currentDisplay, "にほんご", "precondition")
        session.isComposingValue = true

        coordinator.deactivate(client: client)

        assertEqual(session.settleUnconfirmedDisplayed.count, 1, "settled once")
        assertEqual(session.settleUnconfirmedDisplayed[0], "にほんご",
                    "the engine is told exactly what we put on screen")
    }

    // PR315 Codex R3: a client callback can re-enter deactivateServer while an
    // earlier applyEvents loop is still running. The epoch watermark only
    // rejects separately queued responses, so without a lifecycle generation
    // the outer loop resumes and re-opens marked text against an Idle session.
    do {
        let session = FakeLexSession()
        // An auto-commit shape: insertText first, then more events after it.
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [
                .commit(text: "今日"),
                .setMarkedText(text: "は"),
                .showCandidates(surfaces: ["は", "歯"], selected: 0),
            ])
        ]
        let panel = FakePanel()
        let (coordinator, _) = makeCoordinator(session: session, panel: panel)
        let client = FakeIMKClient()
        // Re-enter deactivate from inside the client's insertText, the way a
        // host that changes focus synchronously would.
        client.onInsertText = { [weak coordinator] in
            session.isComposingValue = true
            coordinator?.deactivate(client: nil)
        }

        _ = coordinator.handleKey(.text(text: "h", shift: false), client: client)

        assertEqual(session.settleUnconfirmedCalls, 1, "the reentrant deactivate settled")
        assertTrue(client.markedCalls.isEmpty,
                   "events after the reentrant teardown must not re-open marked text")
        assertTrue(coordinator.currentDisplay == nil,
                   "and must not leave a display behind an Idle session")
    }

    // PR315 Codex R4: `insertText` can re-enter deactivateServer. The settle
    // then reads `currentDisplay`, so that value must already reflect the
    // commit being applied — otherwise it re-commits the text this very call is
    // inserting, duplicating it after the prefix.
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [
                .commit(text: "今日"),
                .setMarkedText(text: "は"),
            ])
        ]
        let (coordinator, _) = makeCoordinator(session: session)
        let client = FakeIMKClient()
        // Put the full pre-commit composition on screen first.
        session.handleKeyResponses.insert(
            LexKeyResponse(consumed: true, events: [.setMarkedText(text: "きょうは")]), at: 0)
        _ = coordinator.handleKey(.text(text: "a", shift: false), client: client)
        assertEqual(coordinator.currentDisplay, "きょうは", "precondition: composition on screen")

        client.onInsertText = { [weak coordinator] in
            session.isComposingValue = true
            coordinator?.deactivate(client: nil)
        }
        _ = coordinator.handleKey(.text(text: "h", shift: false), client: client)

        assertEqual(session.settleUnconfirmedCalls, 1, "the reentrant deactivate settled")
        assertTrue(session.settleUnconfirmedDisplayed[0] == nil,
                   "the settle must not see the pre-commit composition")
        assertTrue(!client.insertCalls.contains { $0.text == "きょうは" },
                   "and must not re-insert what the commit is replacing")
    }

    // #298: nothing composing → nothing to settle. An Idle `commit()` is
    // harmless (commit_current_state early-returns with no events), so this
    // gate is an optimization, not a host-correctness guard — pinned here so a
    // future change that needs to settle unconditionally is not argued down by
    // a justification that does not hold.
    do {
        let session = FakeLexSession()
        let client = FakeIMKClient()
        let (coordinator, _) = makeCoordinator(session: session)
        session.isComposingValue = false

        coordinator.deactivate(client: client)

        assertEqual(session.settleUnconfirmedCalls, 0, "no settle when not composing")
        assertTrue(client.insertCalls.isEmpty, "no text inserted when not composing")
    }

    // #298: no reachable client at all — IMKit hands a non-IMKTextInput sender
    // *and* the weak lastClient has gone. Settling is still mandatory: leaving
    // the session composing while the display is cleared is the leak shape, and
    // "there was nowhere to put the text" does not make it acceptable. Delivery
    // is what degrades here, not settlement.
    do {
        let session = FakeLexSession()
        let (coordinator, _) = makeCoordinator(session: session)
        // No handleKey → lastClient was never set, so both sources are nil.
        session.isComposingValue = true

        coordinator.deactivate(client: nil)

        // Note: asserting !session.isComposingValue here would be tautological
        // — FakeLexSession.commit() sets it. What actually pins the invariant is
        // clearDisplay()'s assert, which is live in this (-Onone) build and
        // would trap if the display were cleared over a composing session.
        assertEqual(session.settleUnconfirmedCalls, 1,
                    "settles the session even with no client to deliver to")
        assertTrue(coordinator.currentDisplay == nil, "display cleared")
    }

    // #298: `isComposing()` also covers snippet browse, where the engine's
    // commit cancels rather than commits (no .commit event). The invariant is
    // that the session reaches Idle — not that text is preserved, which holds
    // for the composing case only.
    do {
        let session = FakeLexSession()
        session.settleUnconfirmedResponses = [
            LexKeyResponse(consumed: true, events: [.setMarkedText(text: ""), .hideCandidates])
        ]
        let client = FakeIMKClient()
        let (coordinator, _) = makeCoordinator(session: session)
        session.isComposingValue = true

        coordinator.deactivate(client: client)

        assertEqual(session.settleUnconfirmedCalls, 1, "snippet browse is settled too")
        assertTrue(client.insertCalls.isEmpty, "a cancelled browse inserts nothing")
        assertTrue(coordinator.currentDisplay == nil, "display cleared")
    }
}
