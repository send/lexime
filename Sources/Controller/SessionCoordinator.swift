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

    /// Focus is arriving. The display must start empty, but the session must
    /// *not* be settled here: committing at this point would insert the previous
    /// client's pending text into the client that just gained focus — the wrong
    /// document. `deactivate(client:)` is the one place allowed to settle, and
    /// this assert is the regression detector for it having failed to
    /// (mirroring the Rust side's `debug_assert_response_contract`: the assert
    /// catches the drift, the structure prevents it).
    func resetDisplay() {
        assert(!session.isComposing(),
               "session still composing on activateServer — deactivate(client:) failed to settle it (#298)")
        currentDisplay = nil
    }

    /// Focus is leaving this client. The host tears down its marked-text session
    /// along with it, so ours must not stay composing: a session that keeps
    /// composing while the host shows no marked text leaks the next confirming
    /// key to the host — the #293 bug, from the Swift side
    /// (SPEC.md § 不変条件（marked text と session の同期）).
    ///
    /// IMKit does **not** reliably send `commitComposition` first. Measured
    /// 2026-08-01 (#298) by instrumenting the lifecycle boundaries and changing
    /// focus mid-composition: `deactivateServer` repeatedly arrived with the
    /// session composing and a live display, and most of those focus changes
    /// produced no `commitComposition` at all — the composition was abandoned
    /// while the display was cleared, leaving the session composing into the
    /// *next* activation (observed directly, and zero after this change).
    ///
    /// Settling is therefore this call's job, and it happens in the *same call*
    /// that clears the display, so the session and the display cannot diverge.
    /// Committing (rather than discarding) matches what `commitComposition`
    /// does and keeps text the user actually typed.
    ///
    /// One host difference this deliberately does **not** address: Chromium /
    /// Electron finalizes its *own* composition on blur, so the reading it was
    /// displaying is what lands there and this `insertText` has no visible
    /// effect. That predates this change — no commit was issued at all before
    /// it, so the visible outcome on those hosts is unchanged. The underlying
    /// mismatch (marked text shows the reading, a commit resolves to the
    /// selected surface) is tracked separately.
    func deactivate(client: IMKTextInput?) {
        // Settle through whichever client we can still reach: the one IMKit is
        // handing us, else the one the composition was typed into.
        if session.isComposing(), let target = client ?? lastClient {
            let resp = session.commit()
            highestAppliedEpoch = max(highestAppliedEpoch, resp.epoch)
            applyEvents(resp, client: target)
        }
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
