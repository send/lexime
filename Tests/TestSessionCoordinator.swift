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
