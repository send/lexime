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

    /// Bumped by every lifecycle teardown. `applyEvents` samples it and stops
    /// if it moves mid-loop.
    ///
    /// The epoch watermark only rejects *separately queued* responses; it
    /// cannot stop a delivery that is already running. A client callback —
    /// `insertText` on an auto-commit response — can re-enter
    /// `deactivateServer` synchronously, which settles the engine and clears
    /// the display while the outer loop still has `.setMarkedText` /
    /// `.showCandidates` events left to apply. Those would then re-open marked
    /// text and re-show the panel against an Idle session, after focus left.
    private var lifecycleGeneration: UInt64 = 0

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
        deliver(session.commit(), to: client)
    }

    /// Apply a settlement response: raise the epoch watermark, then deliver.
    ///
    /// Delivery is best-effort — with no reachable client there is nowhere to
    /// put the text. Settling is not: the caller has already asked the engine
    /// to leave the composing state, because a session left composing after its
    /// display is gone is the #293 leak shape.
    private func deliver(_ resp: LexKeyResponse, to client: IMKTextInput?) {
        // Raise the watermark *before* applying. `applyEvents` calls into the
        // client and the panel; if either ever pumped the run loop, a re-entrant
        // `applyAsyncResponse` would find `lastClient` still set and be stopped
        // only by the epoch guard. This ordering is what makes that safe.
        highestAppliedEpoch = max(highestAppliedEpoch, resp.epoch)
        if let client {
            applyEvents(resp, client: client)
        }
    }

    // MARK: - Lifecycle

    /// The one place the display is cleared *out of band* — driven by an IMKit
    /// lifecycle event rather than by a session response. Both such clears come
    /// through here so the invariant is checked in the call that performs the
    /// clear, the way the Rust side checks inside each response producer rather
    /// than at the next event.
    ///
    /// Deliberately not a `didSet` on `currentDisplay`: `applyEvents`
    /// legitimately nils the display on a `.commit` while the session keeps
    /// composing, with a later `.setMarkedText` in the same response restoring
    /// it (SPEC.md § 不変条件 ②). A per-write check would false-fire — the same
    /// reason Rust checks per *response*, not per event.
    ///
    /// What it can actually catch: `deactivate(client:)` cannot trip it —
    /// settling there is unconditional and `commit()` resets state on both its
    /// branches, so the session is Idle regardless of whether the text could be
    /// delivered. The live path is `resetDisplay()`, which by design does not
    /// settle: if IMKit skipped the previous `deactivateServer` (see
    /// `LeximeInputController.activateServer`), the session arrives here still
    /// composing. Note the assert is compiled out at `-O`, so on that path the
    /// shipped build has neither structure nor detection — recorded as a known
    /// gap in SPEC.md rather than claimed as covered.
    private func clearDisplay() {
        assert(!session.isComposing(),
               "display cleared while the session is still composing (#298)")
        currentDisplay = nil
    }

    /// Focus is arriving. The display starts empty, but the session must *not*
    /// be settled here: committing now would insert the previous client's
    /// pending text into the client that just gained focus — the wrong document.
    /// `deactivate(client:)` is the only place allowed to settle.
    func resetDisplay() {
        clearDisplay()
    }

    /// Focus is leaving this client. The host tears down its marked-text session
    /// with it, so ours must not stay composing — that is #293's leak shape
    /// reached from the Swift side.
    ///
    /// IMKit does **not** reliably send `commitComposition` first (measured
    /// 2026-08-01 by instrumenting the lifecycle boundaries and changing focus
    /// mid-composition), so settling is this call's job, and it happens in the
    /// same call that clears the display — splitting the two is what let them
    /// diverge. Settling is unconditional: whether the text can be *delivered*
    /// depends on having a client, but leaving the session composing does not
    /// become acceptable just because there is nowhere to put the text.
    ///
    /// It settles through `settleUnconfirmed()`, **not** `commit()`. Focus loss
    /// is not acceptance, and the engine's voluntary commit does two things
    /// this must not: it resolves to the selected candidate (so an app switch
    /// would drop a conversion the user never saw into their document — the
    /// composing response shows the *reading* until they navigate) and it
    /// records that conversion as accepted history, training top-1 on a
    /// non-signal. `settleUnconfirmed` learns nothing and commits exactly what
    /// we pass it — `currentDisplay`, which is written right next to the
    /// `setMarkedText` call, so it is what the client was actually told to show.
    /// The engine deliberately does not keep its own copy: it emits marked text
    /// but cannot see whether a response reached the screen (an async one is
    /// queued to this thread and can be dropped here), so a copy on that side
    /// would be a shadow of state it cannot observe. For a `Snippet` browse —
    /// which `isComposing()` also covers — it cancels, exactly as `commit()`
    /// does: there is nothing confirmed to keep.
    ///
    /// One pre-existing hazard survives: an auto-commit already accepted by the
    /// engine but not yet delivered can be dropped here (#314).
    ///
    /// Rule, evidence, and the host difference this does *not* address:
    /// SPEC.md § 不変条件（marked text と session の同期）, #298, #309.
    func deactivate(client: IMKTextInput?) {
        // Abandon any `applyEvents` loop already running: from here its
        // remaining events describe a composition that is about to be settled.
        lifecycleGeneration &+= 1
        // Invalidate first. `applyEvents` below calls into the client and the
        // panel, and a deferred show queued by the last keystroke is guarded by
        // `CandidateManager.generation`, not by the epoch watermark — bumping it
        // here is what stops that block re-showing the shared panel for a
        // composition we are about to settle.
        candidateManager.deactivate()
        if session.isComposing() {
            // Prefer `lastClient`: it is by construction the client the marked
            // text was put on, so settling there replaces that marked text in
            // place. `client` is whatever IMKit hands `deactivateServer`, and is
            // the fallback for `lastClient` being weak and already gone.
            deliver(session.settleUnconfirmed(displayed: currentDisplay), to: lastClient ?? client)
        }
        clearDisplay()
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
        let generation = lifecycleGeneration
        for event in resp.events {
            // A client callback below can re-enter `deactivate(client:)`. Once
            // it has, the rest of this response describes a composition that no
            // longer exists — applying it would restore marked text or the
            // candidate panel after focus already left.
            guard generation == lifecycleGeneration else { return }
            switch event {
            case .commit(let text):
                // Update our record of the display *before* calling the client,
                // the way `.setMarkedText` below already does. `insertText` ends
                // the host's marked session and can re-enter
                // `deactivate(client:)` synchronously; a settle running in that
                // window reads `currentDisplay`, and if it still held the
                // pre-commit composition it would re-commit the text this very
                // call is inserting — duplicating it after the prefix.
                currentDisplay = nil
                client.insertText(text, replacementRange: NSRange(location: NSNotFound, length: 0))
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
