import Carbon
import Cocoa
import InputMethodKit

@objc(LeximeInputController)
class LeximeInputController: IMKInputController {

    // MARK: - State

    var session: OpaquePointer?

    /// Tracks the currently displayed marked text so composedString stays in sync.
    var currentDisplay: String?

    var isComposing: Bool {
        guard let session else { return false }
        return lex_session_is_composing(session) != 0
    }

    // Candidate panel state (for pagination)
    private var predictionCandidates: [String] = []
    private var selectedPredictionIndex: Int = 0

    // Async candidate generation
    private let candidateQueue = DispatchQueue(label: "sh.send.lexime.candidates", qos: .userInitiated)
    private var candidateGeneration: UInt64 = 0
    /// Set when commit_text moves the cursor; forces panel to recalculate position on next show.
    private var panelNeedsReposition = false

    // Ghost text state (GhostText mode)
    private var ghostText: String?
    private let ghostQueue = DispatchQueue(label: "sh.send.lexime.ghost", qos: .utility)
    private var ghostDebounceItem: DispatchWorkItem?

    private static var hasShownDictWarning = false
    private static let historySaveQueue = DispatchQueue(label: "sh.send.lexime.history-save")

    override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)
        let version = String(cString: lex_engine_version())
        NSLog("Lexime: InputController initialized (engine: %@)", version)

        guard let dict = AppContext.shared.dict else {
            if !Self.hasShownDictWarning {
                Self.hasShownDictWarning = true
                NSLog("Lexime: WARNING - dictionary not loaded. Conversion is unavailable.")
            }
            return
        }

        session = lex_session_new(dict, AppContext.shared.conn, AppContext.shared.history)
        guard let session else { return }
        lex_session_set_defer_candidates(session, 1)
        if UserDefaults.standard.bool(forKey: "programmerMode") {
            lex_session_set_programmer_mode(session, 1)
        }
        let convMode = UserDefaults.standard.integer(forKey: "conversionMode")
        if convMode > 0, convMode <= UInt8.max {
            lex_session_set_conversion_mode(session, UInt8(convMode))
        }
    }

    deinit {
        if let s = session {
            lex_session_free(s)
        }
    }

    override func recognizedEvents(_ sender: Any!) -> Int {
        let mask = NSEvent.EventTypeMask.keyDown.union(.flagsChanged)
        return Int(mask.rawValue)
    }

    // MARK: - Candidate Panel

    static let maxCandidateDisplay = 9

    private func cursorRect(client: IMKTextInput) -> NSRect {
        var rect = NSRect.zero
        client.attributes(forCharacterIndex: 0, lineHeightRectangle: &rect)
        // Use end-of-text position for horizontal follow
        let index = currentDisplay?.utf16.count ?? 0
        if index > 0 {
            var endRect = NSRect.zero
            client.attributes(forCharacterIndex: index, lineHeightRectangle: &endRect)
            if endRect != .zero {
                rect.origin.x = endRect.origin.x
            }
        }
        return rect
    }

    private static let modeNames = ["standard", "predictive", "ghost"]

    private var currentModeName: String? {
        let mode = UserDefaults.standard.integer(forKey: "conversionMode")
        guard mode > 0, mode < Self.modeNames.count else { return nil }
        return Self.modeNames[mode]
    }

    private func showCandidatePanel(client: IMKTextInput) {
        let allCandidates = predictionCandidates
        let selectedIndex = selectedPredictionIndex

        guard !allCandidates.isEmpty else { hideCandidatePanel(); return }
        let clampedIndex = min(selectedIndex, allCandidates.count - 1)

        let pageSize = Self.maxCandidateDisplay
        let page = clampedIndex / pageSize
        let pageStart = page * pageSize
        let pageEnd = min(pageStart + pageSize, allCandidates.count)
        let pageCandidates = Array(allCandidates[pageStart..<pageEnd])
        let pageSelectedIndex = clampedIndex - pageStart

        let panel = AppContext.shared.candidatePanel

        let totalCount = allCandidates.count
        let modeName = currentModeName

        // Mozc style: don't recalculate position while panel is visible (prevents jitter)
        // But if cursor moved (auto-commit), force reposition.
        if panel.isVisible && !panelNeedsReposition {
            panel.show(candidates: pageCandidates, selectedIndex: pageSelectedIndex,
                       globalIndex: clampedIndex, totalCount: totalCount, cursorRect: nil,
                       modeName: modeName)
            return
        }
        // Reset early: if the async block below is cancelled (generation mismatch),
        // the panel stays hidden, so the next showCandidatePanel takes the full show path anyway.
        panelNeedsReposition = false

        // Capture rect synchronously (client state is correct here),
        // then defer panel show to next run loop (workaround for Chrome etc.)
        let rect = cursorRect(client: client)
        let generation = candidateGeneration
        DispatchQueue.main.async { [weak self] in
            guard let self, self.candidateGeneration == generation else { return }
            panel.show(candidates: pageCandidates, selectedIndex: pageSelectedIndex,
                       globalIndex: clampedIndex, totalCount: totalCount, cursorRect: rect,
                       modeName: modeName)
        }
    }

    private func hideCandidatePanel() {
        AppContext.shared.candidatePanel.hide()
    }

    // MARK: - Key Handling

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard let session, let event, let client = sender as? IMKTextInput else {
            return false
        }

        guard event.type == .keyDown else {
            // Consume modifier-only events while composing
            return isComposing
        }

        let dominated = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            .subtracting([.capsLock, .numericPad, .function])

        // Cycle conversion mode: Option+Tab or Shift+Tab
        let isCycleMode = event.keyCode == 48 /* Tab */
            && (dominated == [.option] || dominated == [.shift])
        if isCycleMode {
            cycleConversionMode(session: session, client: client)
            return true
        }

        // Sync programmerMode setting on each key event
        lex_session_set_programmer_mode(
            session,
            UserDefaults.standard.bool(forKey: "programmerMode") ? 1 : 0
        )
        let convMode = min(max(UserDefaults.standard.integer(forKey: "conversionMode"), 0), Int(UInt8.max))
        lex_session_set_conversion_mode(session, UInt8(convMode))

        let shift: UInt8 = dominated.contains(.shift) ? 1 : 0
        let hasModifier: UInt8 = !dominated.subtracting(.shift).isEmpty ? 1 : 0
        let flags = shift | (hasModifier << 1)

        // Clear ghost text on any key except Tab (ghost accept is handled by the engine)
        if ghostText != nil && event.keyCode != 48 /* Tab */ {
            clearGhostDisplay(client: client)
        }

        // Invalidate any pending async candidate results
        candidateGeneration &+= 1

        let text = event.characters ?? ""
        let resp = lex_session_handle_key(session, event.keyCode, text, flags)
        defer { lex_key_response_free(resp) }
        applyResponse(resp, client: client)
        return resp.consumed != 0
    }

    // MARK: - Apply Response

    private func applyResponse(_ resp: LexKeyResponse, client: IMKTextInput) {
        // 1. Commit text
        if let commitText = resp.commit_text {
            let text = String(cString: commitText)
            client.insertText(text, replacementRange: NSRange(location: NSNotFound, length: 0))
            currentDisplay = nil
            panelNeedsReposition = true
        }

        // 2. Marked text
        if let markedText = resp.marked_text {
            let text = String(cString: markedText)
            currentDisplay = text
            updateMarkedText(text, dashed: resp.is_dashed_underline != 0, client: client)
        }

        // 3. Candidate panel
        if resp.hide_candidates != 0 {
            hideCandidatePanel()
        }
        if resp.show_candidates != 0 {
            predictionCandidates = extractCandidates(resp)
            selectedPredictionIndex = Int(resp.selected_index)
            showCandidatePanel(client: client)
        }

        // 4. Side effects
        if resp.switch_to_abc != 0 {
            selectABCInputSource()
        }
        if resp.save_history != 0 {
            recordAndSaveHistory(resp)
        }

        // 5. Async candidate generation
        if resp.needs_candidates != 0, let candidateReading = resp.candidate_reading {
            dispatchAsyncCandidates(
                reading: String(cString: candidateReading),
                dispatch: resp.candidate_dispatch
            )
        }

        // 6. Ghost text
        if let ghostTextPtr = resp.ghost_text {
            let text = String(cString: ghostTextPtr)
            if text.isEmpty {
                // Clear ghost state. Only clear the display if no marked text was set
                // in this same response (step 2 already replaced the screen content).
                ghostText = nil
                ghostDebounceItem?.cancel()
                ghostDebounceItem = nil
                if resp.marked_text == nil {
                    clearGhostText(client: client)
                }
            } else {
                ghostText = text
                showGhostText(text, client: client)
            }
        }

        // 7. Ghost generation request (debounced)
        if resp.needs_ghost_text != 0, let ghostContext = resp.ghost_context {
            let context = String(cString: ghostContext)
            let generation = resp.ghost_generation
            requestGhostText(context: context, generation: generation)
        }
    }

    // MARK: - Async Candidates

    private func dispatchAsyncCandidates(reading: String, dispatch: UInt8 = 0) {
        let gen = candidateGeneration
        // These opaque pointers are long-lived singletons. The Rust types use
        // internal synchronization (RwLock), so concurrent access from
        // candidateQueue, historySaveQueue, and the main thread is safe.
        nonisolated(unsafe) let dict = AppContext.shared.dict
        nonisolated(unsafe) let conn = AppContext.shared.conn
        nonisolated(unsafe) let history = AppContext.shared.history
        nonisolated(unsafe) let neural = AppContext.shared.neural
        guard let dict else { return }

        // Capture committed context on main thread (LexSession is not thread-safe).
        let context: String
        if dispatch == 2, let session {
            let ctxPtr = lex_session_committed_context(session)
            context = ctxPtr.map { String(cString: $0) } ?? ""
            lex_committed_context_free(ctxPtr)
        } else {
            context = ""
        }

        candidateQueue.async { [weak self] in
            var result: LexCandidateResponse
            switch dispatch {
            case 2:  // neural (speculative decode)
                if let neural {
                    result = reading.withCString { readingCStr in
                        context.withCString { ctxCStr in
                            lex_generate_neural_candidates(neural, dict, conn, history, ctxCStr, readingCStr, 20)
                        }
                    }
                } else {
                    // Fallback to standard if neural model not loaded
                    result = reading.withCString { lex_generate_candidates(dict, conn, history, $0, 20) }
                }
            case 1:  // prediction (Viterbi + bigram chaining)
                result = reading.withCString { lex_generate_prediction_candidates(dict, conn, history, $0, 20) }
            default: // standard
                result = reading.withCString { lex_generate_candidates(dict, conn, history, $0, 20) }
            }
            DispatchQueue.main.async { [weak self] in
                guard let self else {
                    lex_candidate_response_free(result)
                    return
                }
                guard self.candidateGeneration == gen else {
                    lex_candidate_response_free(result)
                    return
                }
                guard let client = self.client() else {
                    lex_candidate_response_free(result)
                    return
                }
                // Access session through self (protected by weak self guard above)
                // instead of capturing the raw pointer directly.
                guard let session = self.session else {
                    lex_candidate_response_free(result)
                    return
                }
                let resp = reading.withCString { readingCStr in
                    lex_session_receive_candidates(session, readingCStr, &result)
                }
                lex_candidate_response_free(result)
                defer { lex_key_response_free(resp) }
                self.applyResponse(resp, client: client)
            }
        }
    }

    // MARK: - History

    /// Record history entries and persist to disk asynchronously.
    /// Thread safety: `LexUserHistoryWrapper` uses `RwLock` internally, so
    /// `record_history` (write lock, main thread) and `history_save`
    /// (read lock + clone, historySaveQueue) are safely synchronized.
    private func recordAndSaveHistory(_ resp: LexKeyResponse) {
        guard let history = AppContext.shared.history else { return }
        withUnsafePointer(to: resp) { respPtr in
            lex_key_response_record_history(respPtr, history)
        }
        let path = AppContext.shared.historyPath
        Self.historySaveQueue.async {
            let result = lex_history_save(history, path)
            if result != 0 {
                NSLog("Lexime: Failed to save user history to %@", path)
            }
        }
    }

    // MARK: - Ghost Text

    private func clearGhostDisplay(client: IMKTextInput) {
        ghostText = nil
        ghostDebounceItem?.cancel()
        ghostDebounceItem = nil
        clearGhostText(client: client)
    }

    private func requestGhostText(context: String, generation: UInt64) {
        ghostDebounceItem?.cancel()
        nonisolated(unsafe) let neural = AppContext.shared.neural
        guard let neural else { return }
        let item = DispatchWorkItem { [weak self] in
            let result = context.withCString { ctxCStr in
                lex_neural_generate_ghost(neural, ctxCStr, 30)
            }
            defer { lex_ghost_text_free(result) }
            guard let textPtr = result.text else { return }
            let text = String(cString: textPtr)
            guard !text.isEmpty else { return }
            DispatchQueue.main.async { [weak self] in
                guard let self, let session = self.session else { return }
                let resp = text.withCString { textCStr in
                    lex_session_receive_ghost_text(session, generation, textCStr)
                }
                defer { lex_key_response_free(resp) }
                guard let client = self.client() else { return }
                self.applyResponse(resp, client: client)
            }
        }
        ghostDebounceItem = item
        ghostQueue.asyncAfter(deadline: .now() + 0.15, execute: item)
    }

    // MARK: - Helpers

    private func extractCandidates(_ resp: LexKeyResponse) -> [String] {
        var result: [String] = []
        guard resp.candidates_len > 0, let ptrs = resp.candidates else { return result }
        for i in 0..<Int(resp.candidates_len) {
            if let ptr = ptrs[i] {
                result.append(String(cString: ptr))
            }
        }
        return result
    }

    private func cycleConversionMode(session: OpaquePointer, client: IMKTextInput) {
        if isComposing {
            let commitResp = lex_session_commit(session)
            defer { lex_key_response_free(commitResp) }
            applyResponse(commitResp, client: client)
        }
        if ghostText != nil {
            clearGhostDisplay(client: client)
        }
        let maxModes = AppContext.shared.neural != nil ? 3 : 2
        let current = UserDefaults.standard.integer(forKey: "conversionMode")
        let next = (current + 1) % maxModes
        UserDefaults.standard.set(next, forKey: "conversionMode")
        lex_session_set_conversion_mode(session, UInt8(next))
        let names = ["standard", "predictive", "ghost"]
        NSLog("Lexime: conversion mode → %@", names[next])
        let rect = cursorRect(client: client)
        AppContext.shared.candidatePanel.showNotification(text: names[next], cursorRect: rect)
    }

    private func selectABCInputSource() {
        let conditions = [
            kTISPropertyInputSourceID as String: "com.apple.keylayout.ABC"
        ] as CFDictionary
        guard let list = TISCreateInputSourceList(conditions, false)?.takeRetainedValue()
                as? [TISInputSource],
              let source = list.first else { return }
        TISSelectInputSource(source)
    }

    // MARK: - IMKInputController Overrides

    override func composedString(_ sender: Any!) -> Any! {
        return currentDisplay ?? ""
    }

    override func originalString(_ sender: Any!) -> NSAttributedString! {
        return NSAttributedString(string: currentDisplay ?? "")
    }

    override func commitComposition(_ sender: Any!) {
        guard let session, let client = sender as? IMKTextInput else { return }
        let resp = lex_session_commit(session)
        defer { lex_key_response_free(resp) }
        applyResponse(resp, client: client)
    }

    override func activateServer(_ sender: Any!) {
        currentDisplay = nil
        predictionCandidates = []
        selectedPredictionIndex = 0
        super.activateServer(sender)
    }

    override func deactivateServer(_ sender: Any!) {
        candidateGeneration &+= 1
        ghostDebounceItem?.cancel()
        ghostText = nil
        currentDisplay = nil
        hideCandidatePanel()
        super.deactivateServer(sender)
    }

    // Block IMKit's built-in mode switching during composition.
    // IMKit calls setValue when Caps Lock or other mode keys are pressed.
    // Passing these through during composition can trigger unwanted transformations
    // (e.g. Shift-triggered katakana). We intentionally drop all mode changes
    // while composing and let the engine handle mode via its own key handlers.
    override func setValue(_ value: Any!, forTag tag: Int, client sender: Any!) {
        if isComposing { return }
        super.setValue(value, forTag: tag, client: sender)
    }
}
