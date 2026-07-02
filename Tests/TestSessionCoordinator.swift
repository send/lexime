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

    // deactivate: invalidates candidates, hides panel, clears display
    do {
        let session = FakeLexSession()
        session.handleKeyResponses = [
            LexKeyResponse(consumed: true, events: [.setMarkedText(text: "y")])
        ]
        let panel = FakePanel()
        let (coordinator, manager) = makeCoordinator(session: session, panel: panel)
        _ = coordinator.handleKey(.text(text: "y", shift: false), client: FakeIMKClient())
        let genBefore = manager.generation
        coordinator.deactivate()
        assertTrue(manager.generation == genBefore &+ 1,
                   "deactivate invalidates generation")
        assertTrue(panel.hideCount >= 1, "deactivate hides panel")
        assertTrue(coordinator.currentDisplay == nil, "deactivate clears display")
    }
}
