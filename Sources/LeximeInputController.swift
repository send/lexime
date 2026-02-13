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
    var predictionCandidates: [String] = []
    var selectedPredictionIndex: Int = 0

    // Async candidate generation
    private let candidateQueue = DispatchQueue(label: "sh.send.lexime.candidates", qos: .userInitiated)
    private var candidateGeneration: UInt64 = 0

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
        lex_session_set_defer_candidates(session, 1)
        if UserDefaults.standard.bool(forKey: "programmerMode") {
            lex_session_set_programmer_mode(session, 1)
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

    func cursorRect(client: IMKTextInput) -> NSRect {
        var rect = NSRect.zero
        client.attributes(forCharacterIndex: 0, lineHeightRectangle: &rect)
        return rect
    }

    func showCandidatePanel(client: IMKTextInput) {
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

        let rect = cursorRect(client: client)
        AppContext.shared.candidatePanel.show(candidates: pageCandidates, selectedIndex: pageSelectedIndex, cursorRect: rect)
    }

    func hideCandidatePanel() {
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

        // Sync programmerMode setting on each key event
        lex_session_set_programmer_mode(
            session,
            UserDefaults.standard.bool(forKey: "programmerMode") ? 1 : 0
        )

        let dominated = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            .subtracting([.capsLock, .numericPad, .function])
        let shift: UInt8 = dominated.contains(.shift) ? 1 : 0
        let hasModifier: UInt8 = !dominated.subtracting(.shift).isEmpty ? 1 : 0
        let flags = shift | (hasModifier << 1)

        // Invalidate any pending async candidate results
        candidateGeneration += 1

        let text = event.characters ?? ""
        let resp = lex_session_handle_key(session, event.keyCode, text, flags)
        defer { lex_key_response_free(resp) }
        applyResponse(resp, client: client)
        return resp.consumed != 0
    }

    // MARK: - Apply Response

    private func applyResponse(_ resp: LexKeyResponse, client: IMKTextInput) {
        // 1. Commit text
        if resp.commit_text != nil {
            let text = String(cString: resp.commit_text)
            client.insertText(text, replacementRange: NSRange(location: NSNotFound, length: 0))
        }

        // 2. Marked text
        if resp.marked_text != nil {
            let text = String(cString: resp.marked_text)
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
        if resp.needs_candidates != 0, resp.candidate_reading != nil {
            dispatchAsyncCandidates(reading: String(cString: resp.candidate_reading))
        }
    }

    // MARK: - Async Candidates

    private func dispatchAsyncCandidates(reading: String) {
        let gen = candidateGeneration
        // These opaque pointers are long-lived (outlive the session) and read-only
        // on the background thread, so cross-thread capture is safe.
        nonisolated(unsafe) let dict = AppContext.shared.dict
        nonisolated(unsafe) let conn = AppContext.shared.conn
        nonisolated(unsafe) let history = AppContext.shared.history
        let sessionPtr = self.session

        candidateQueue.async { [weak self] in
            var result = reading.withCString { readingCStr in
                lex_generate_candidates(dict, conn, history, readingCStr, 20)
            }
            DispatchQueue.main.async { [weak self] in
                guard let self, let sessionPtr else {
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
                let resp = reading.withCString { readingCStr in
                    lex_session_receive_candidates(sessionPtr, readingCStr, &result)
                }
                lex_candidate_response_free(result)
                defer { lex_key_response_free(resp) }
                self.applyResponse(resp, client: client)
            }
        }
    }

    // MARK: - History

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

    // Block IMKit's built-in mode switching (e.g. Shift -> katakana)
    override func setValue(_ value: Any!, forTag tag: Int, client sender: Any!) {
        if isComposing { return }
        super.setValue(value, forTag: tag, client: sender)
    }
}
