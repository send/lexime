import Carbon
import Cocoa
import InputMethodKit

@objc(LeximeInputController)
class LeximeInputController: IMKInputController {

    // MARK: - State

    var state: InputState = .idle
    var composedKana: String = ""
    var pendingRomaji: String = ""

    var nbestPaths: [[(reading: String, surface: String)]] = []
    /// Tracks the currently displayed marked text so composedString stays in sync.
    var currentDisplay: String?

    var isComposing: Bool { state != .idle }

    var selectedPredictionIndex: Int = 0
    var programmerMode: Bool {
        UserDefaults.standard.bool(forKey: "programmerMode")
    }

    static let maxComposedKanaLength = 100

    // Realtime prediction state
    var predictionCandidates: [String] = []

    private static var hasShownDictWarning = false

    override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)
        let version = String(cString: lex_engine_version())
        NSLog("Lexime: InputController initialized (engine: %@)", version)
        if AppContext.shared.dict == nil && !Self.hasShownDictWarning {
            Self.hasShownDictWarning = true
            NSLog("Lexime: WARNING - dictionary not loaded. Conversion is unavailable.")
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
        guard state == .composing else { return }

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
        guard let event = event, let client = sender as? IMKTextInput else {
            return false
        }

        guard event.type == .keyDown else {
            // Consume modifier-only events (e.g. Shift press) while composing
            // to prevent IMKit's default handling from interfering.
            return isComposing
        }

        // Eisu key (102) → switch to ABC input source
        if event.keyCode == Key.eisu {
            if isComposing { commitCurrentState(client: client) }
            selectABCInputSource()
            return true
        }
        // Kana key (104) → already in Japanese mode, consume the event
        if event.keyCode == Key.kana {
            return true
        }

        let dominated = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            .subtracting([.capsLock, .numericPad, .function])

        // Modifier keys (Cmd, Ctrl, etc.) — commit first, then pass through
        // Shift alone is excluded (used for normal text input like ?, !, ~)
        if !dominated.subtracting(.shift).isEmpty {
            NSLog("Lexime: modifier key pass-through (dominated=%lu, composing=%d)",
                  dominated.rawValue, isComposing ? 1 : 0)
            if isComposing {
                commitCurrentState(client: client)
            }
            return false
        }

        // Programmer mode: ¥ key → insert backslash (Shift+¥ = pipe is excluded)
        if event.keyCode == Key.yen && programmerMode && !dominated.contains(.shift) {
            if isComposing { commitCurrentState(client: client) }
            client.insertText("\\", replacementRange: NSRange(location: NSNotFound, length: 0))
            return true
        }

        let keyCode = event.keyCode
        guard let text = event.characters, !text.isEmpty else {
            return false
        }

        switch state {
        case .idle:
            return handleIdle(keyCode: keyCode, text: text, client: client)
        case .composing:
            return handleComposing(keyCode: keyCode, text: text, client: client)
        }
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

    // MARK: - Romaji Conversion

    func appendAndConvert(_ input: String, client: IMKTextInput) {
        if composedKana.count >= Self.maxComposedKanaLength {
            flush()
            commitComposed(client: client)
            state = .composing
        }
        pendingRomaji += input
        drainPending(force: false)
        updateMarkedText(client: client)
        updateCandidates()
    }

    func updateCandidates() {
        selectedPredictionIndex = 0
        guard !composedKana.isEmpty else {
            predictionCandidates = []
            hideCandidatePanel()
            return
        }

        let (candidates, paths) = generateCandidates(composedKana)
        predictionCandidates = candidates
        nbestPaths = paths

        // Real-time conversion: show Viterbi #1 inline and open candidate panel
        if let client = self.client() {
            if let best = candidates.first {
                updateMarkedText(best, client: client)
            }
            showCandidatePanel(client: client)
        }
    }

    private func drainPending(force: Bool) {
        let result = convertRomaji(
            composedKana: composedKana,
            pendingRomaji: pendingRomaji,
            force: force)
        composedKana = result.composedKana
        pendingRomaji = result.pendingRomaji
    }

    // MARK: - Flush & Commit

    func flush() {
        drainPending(force: true)
    }

    func commitComposed(client: IMKTextInput) {
        if !composedKana.isEmpty {
            NSLog("Lexime: commit %@", composedKana)
            client.insertText(composedKana, replacementRange: NSRange(location: NSNotFound, length: 0))
        } else {
            client.setMarkedText("",
                                 selectionRange: NSRange(location: 0, length: 0),
                                 replacementRange: NSRange(location: NSNotFound, length: 0))
        }
        resetState()
    }

    func commitText(_ text: String, client: IMKTextInput) {
        NSLog("Lexime: commit %@", text)
        client.insertText(text, replacementRange: NSRange(location: NSNotFound, length: 0))
        resetState()
    }

    func commitCurrentState(client: IMKTextInput) {
        switch state {
        case .idle:
            break
        case .composing:
            hideCandidatePanel()
            flush()
            if selectedPredictionIndex < predictionCandidates.count {
                let reading = composedKana
                let surface = predictionCandidates[selectedPredictionIndex]
                if surface != reading {
                    recordToHistory(reading: reading, surface: surface)
                }
                commitText(surface, client: client)
            } else {
                commitComposed(client: client)
            }
        }
    }

    private static let historySaveQueue = DispatchQueue(label: "sh.send.lexime.history-save")

    func recordToHistory(reading: String, surface: String) {
        guard let history = AppContext.shared.history else { return }

        // Record the committed pair (whole reading → surface)
        recordPairsToFFI([(reading, surface)], history: history)

        // Sub-phrase learning: if there's a matching N-best path whose
        // joined surface equals the committed surface, also record its
        // individual segments so sub-phrase mappings are learned.
        if let matchingPath = nbestPaths.first(where: { path in
            path.map { $0.surface }.joined() == surface
        }), matchingPath.count > 1 {
            recordPairsToFFI(matchingPath, history: history)
        }

        // Save asynchronously to avoid blocking key handling
        let path = AppContext.shared.historyPath
        Self.historySaveQueue.async {
            let result = lex_history_save(history, path)
            if result != 0 {
                NSLog("Lexime: Failed to save user history to %@", path)
            }
        }
    }

    private func recordPairsToFFI(_ pairs: [(reading: String, surface: String)], history: OpaquePointer) {
        var cStrings: [UnsafeMutablePointer<CChar>] = []
        var lexSegments: [LexSegment] = []
        for (reading, surface) in pairs {
            guard let r = strdup(reading), let s = strdup(surface) else { continue }
            cStrings.append(r)
            cStrings.append(s)
            lexSegments.append(LexSegment(reading: r, surface: s))
        }
        defer { cStrings.forEach { free($0) } }

        lexSegments.withUnsafeBufferPointer { buffer in
            guard let base = buffer.baseAddress else { return }
            lex_history_record(history, base, UInt32(buffer.count))
        }
    }

    override func composedString(_ sender: Any!) -> Any! {
        return currentDisplay ?? (composedKana + pendingRomaji)
    }

    override func originalString(_ sender: Any!) -> NSAttributedString! {
        return NSAttributedString(string: composedKana + pendingRomaji)
    }

    override func commitComposition(_ sender: Any!) {
        if let client = sender as? IMKTextInput {
            commitCurrentState(client: client)
        }
    }

    // Block IMKit's built-in mode switching (e.g. Shift→katakana)
    // which would interfere with our own composing state management.
    override func setValue(_ value: Any!, forTag tag: Int, client sender: Any!) {
        if isComposing { return }
        super.setValue(value, forTag: tag, client: sender)
    }

    func resetState() {
        composedKana = ""
        pendingRomaji = ""
        nbestPaths = []
        predictionCandidates = []
        selectedPredictionIndex = 0
        currentDisplay = nil
        state = .idle
    }
}
