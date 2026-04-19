import Cocoa
import InputMethodKit

/// Owns the Rust LexSession and translates IMKit key events into session calls,
/// applying the resulting LexEvent stream to the IMKTextInput client and the
/// candidate panel. Also owns the async poll timer that drains deferred
/// session results between keystrokes.
final class SessionCoordinator {

    private let session: LexSession
    private let candidateManager: CandidateManager
    private let onSwitchToAbc: () -> Void

    /// Tracks the currently displayed marked text so composedString stays in sync.
    private(set) var currentDisplay: String?

    private var pollTimer: Timer?
    private weak var pollClient: IMKTextInput?

    init(session: LexSession,
         candidateManager: CandidateManager,
         onSwitchToAbc: @escaping () -> Void) {
        self.session = session
        self.candidateManager = candidateManager
        self.onSwitchToAbc = onSwitchToAbc
    }

    deinit {
        pollTimer?.invalidate()
    }

    // MARK: - Session Passthrough

    var isComposing: Bool { session.isComposing() }

    func setSnippetStore(_ store: LexSnippetStore?) {
        session.setSnippetStore(store: store)
    }

    func setAbcPassthrough(enabled: Bool) {
        session.setAbcPassthrough(enabled: enabled)
    }

    // MARK: - Key Handling

    /// Drain any pending async results before taking a new key event.
    /// Safe to call on any incoming IMKit event; does not stop the poll loop,
    /// so modifier-only events won't starve deferred candidate updates.
    func drainPending(client: IMKTextInput) {
        while let resp = session.poll() {
            applyEvents(resp, client: client)
        }
    }

    func handleKey(_ keyEvent: LexKeyEvent, client: IMKTextInput) -> Bool {
        cancelPollTimer()
        candidateManager.invalidate()
        let resp = session.handleKey(event: keyEvent)
        applyEvents(resp, client: client)
        return resp.consumed
    }

    func commit(client: IMKTextInput) {
        let resp = session.commit()
        applyEvents(resp, client: client)
    }

    // MARK: - Lifecycle

    func resetDisplay() {
        currentDisplay = nil
    }

    func deactivate() {
        cancelPollTimer()
        candidateManager.deactivate()
        currentDisplay = nil
    }

    // MARK: - Apply Events

    private func applyEvents(_ resp: LexKeyResponse, client: IMKTextInput) {
        for event in resp.events {
            switch event {
            case .commit(let text):
                client.insertText(text, replacementRange: NSRange(location: NSNotFound, length: 0))
                currentDisplay = nil
                candidateManager.flagReposition()
            case .setMarkedText(let text):
                currentDisplay = text.isEmpty ? nil : text
                Self.updateMarkedText(text, client: client)
            case .showCandidates(let surfaces, let selected):
                candidateManager.update(surfaces: surfaces, selected: Int(selected))
                candidateManager.show(client: client, currentDisplay: currentDisplay)
            case .hideCandidates:
                candidateManager.hide()
            case .switchToAbc:
                onSwitchToAbc()
            case .schedulePoll:
                schedulePollTimer(client: client)
            }
        }
    }

    /// Update inline marked text with the given display string.
    /// Uses markedClauseSegment to prevent the client's text system from
    /// applying its own transformations (e.g. Shift-triggered katakana conversion).
    private static func updateMarkedText(_ text: String, client: IMKTextInput) {
        let len = text.utf16.count
        let attrs: [NSAttributedString.Key: Any] = [.markedClauseSegment: 0]
        let attrStr = NSAttributedString(string: text, attributes: attrs)
        client.setMarkedText(attrStr,
                             selectionRange: NSRange(location: len, length: 0),
                             replacementRange: NSRange(location: NSNotFound, length: 0))
    }

    // MARK: - Poll Timer

    private func schedulePollTimer(client: IMKTextInput) {
        pollClient = client
        guard pollTimer == nil else { return }
        var idleTicks = 0
        pollTimer = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { [weak self] _ in
            guard let self, let client = self.pollClient else {
                self?.cancelPollTimer()
                return
            }
            var hadResult = false
            while let resp = self.session.poll() {
                self.applyEvents(resp, client: client)
                hadResult = true
            }
            if hadResult {
                idleTicks = 0
            } else {
                idleTicks += 1
                if idleTicks >= 100 {
                    self.cancelPollTimer()
                }
            }
        }
    }

    private func cancelPollTimer() {
        pollTimer?.invalidate()
        pollTimer = nil
        pollClient = nil
    }
}
