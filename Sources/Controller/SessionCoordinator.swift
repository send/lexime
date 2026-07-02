import Cocoa
import InputMethodKit

/// Owns the Rust LexSession and translates IMKit key events into session calls,
/// applying the resulting LexEvent stream to the IMKTextInput client and the
/// candidate panel. Async results are delivered via the `LexSessionEvents`
/// callback, dispatched onto the main thread.
final class SessionCoordinator {

    // Held as the UniFFI-generated protocol so tests can inject a fake session
    // without crossing the FFI boundary.
    private let session: LexSessionProtocol
    private let candidateManager: CandidateManager
    private let onSwitchToAbc: () -> Void

    /// Tracks the currently displayed marked text so composedString stays in sync.
    private(set) var currentDisplay: String?

    /// Client captured by the most recent handleKey. Used when an async callback
    /// arrives between keystrokes and we need an IMKTextInput to apply events against.
    private weak var lastClient: IMKTextInput?

    /// Highest session epoch whose events have been applied to the UI.
    ///
    /// The Rust session rejects stale async responses under its own lock, but
    /// accepted async responses are re-queued onto the main thread (see
    /// `Listener`) while key responses are applied inline in `handleKey` /
    /// `commit`. A response accepted *before* a key was processed can
    /// therefore reach the UI *after* that key's events — re-showing hidden
    /// candidates or rewinding the selection. Epochs are monotonic, so any
    /// async response carrying an epoch lower than the highest already
    /// applied is a re-ordered delivery and must be dropped.
    private(set) var highestAppliedEpoch: UInt64 = 0

    init(factory: (LexSessionEvents) -> LexSessionProtocol,
         candidateManager: CandidateManager,
         onSwitchToAbc: @escaping () -> Void) {
        self.candidateManager = candidateManager
        self.onSwitchToAbc = onSwitchToAbc
        // Build the listener first, then construct the session with it. The
        // listener holds only a weak reference to `self`, breaking the retain
        // cycle created by LexSession -> listener -> SessionCoordinator.
        let listener = Listener()
        self.session = factory(listener)
        listener.coordinator = self
    }

    deinit {
        session.shutdown()
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

    func handleKey(_ keyEvent: LexKeyEvent, client: IMKTextInput) -> Bool {
        lastClient = client
        candidateManager.invalidate()
        let resp = session.handleKey(event: keyEvent)
        // Synchronous responses always reflect the latest session state
        // (the session lock serializes them), so apply unconditionally and
        // advance the epoch watermark.
        highestAppliedEpoch = max(highestAppliedEpoch, resp.epoch)
        applyEvents(resp, client: client)
        return resp.consumed
    }

    func commit(client: IMKTextInput) {
        lastClient = client
        let resp = session.commit()
        highestAppliedEpoch = max(highestAppliedEpoch, resp.epoch)
        applyEvents(resp, client: client)
    }

    // MARK: - Lifecycle

    func resetDisplay() {
        currentDisplay = nil
    }

    func deactivate() {
        candidateManager.deactivate()
        currentDisplay = nil
        lastClient = nil
    }

    // MARK: - Apply Events

    /// Apply an async candidate response on the main thread.
    /// Internal (not fileprivate) so unit tests can drive it directly.
    func applyAsyncResponse(_ resp: LexKeyResponse) {
        // IMKit and the epoch watermark are main-thread only; the Listener
        // hops here via DispatchQueue.main.async. Debug-build guard against
        // future in-module callers invoking this directly off-main.
        assert(Thread.isMainThread)
        // Drop re-ordered deliveries: this response was accepted by the Rust
        // session, but a newer key response has already been applied to the
        // UI while this one sat in the main queue (see `highestAppliedEpoch`).
        guard resp.epoch >= highestAppliedEpoch else { return }
        guard let client = lastClient else { return }
        highestAppliedEpoch = resp.epoch
        applyEvents(resp, client: client)
    }

    private func applyEvents(_ resp: LexKeyResponse, client: IMKTextInput) {
        assert(Thread.isMainThread)
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
}

/// Bridge object passed to `LexEngine.createSession`. Holds a weak reference
/// back to the coordinator so the Rust-held listener does not keep the
/// coordinator alive (breaking the retain cycle `LexSession` -> listener ->
/// `SessionCoordinator` -> `LexSession`).
private final class Listener: LexSessionEvents, @unchecked Sendable {
    weak var coordinator: SessionCoordinator?

    func onAsyncResponse(response: LexKeyResponse) {
        // Invoked on the Rust AsyncWorker thread; bounce to the main thread
        // where UI / IMKit calls are safe.
        DispatchQueue.main.async { [weak self] in
            self?.coordinator?.applyAsyncResponse(response)
        }
    }
}
