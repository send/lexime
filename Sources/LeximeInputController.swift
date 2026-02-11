import Carbon
import Cocoa
import InputMethodKit

@objc(LeximeInputController)
class LeximeInputController: IMKInputController {

    // MARK: - State

    var state: InputState = .idle
    var composedKana: String = ""
    var pendingRomaji: String = ""

    // Multi-segment conversion state
    var originalKana: String = ""
    var conversionSegments: [ConversionSegment] = []
    var activeSegmentIndex: Int = 0

    var isComposing: Bool { state != .idle }

    let trie = RomajiTrie.shared
    var selectedPredictionIndex: Int = 0
    var isPunctuationComposing: Bool = false

    /// Maps ASCII key to [fullwidth, halfwidth] candidates
    static let punctuationCandidates: [String: [String]] = [
        ".": ["。", "."], ",": ["、", ","], "?": ["？", "?"], "!": ["！", "!"],
        "[": ["「", "｢", "["], "]": ["」", "｣", "]"], "/": ["・", "/"], "~": ["〜", "~"],
    ]

    static let maxComposedKanaLength = 100

    // Realtime prediction state
    var predictionCandidates: [String] = []

    private static var hasShownDictWarning = false

    override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)
        let version = String(cString: lex_engine_version())
        NSLog("Lexime: InputController initialized (engine: %@)", version)
        if sharedDict == nil && !Self.hasShownDictWarning {
            Self.hasShownDictWarning = true
            NSLog("Lexime: WARNING - dictionary not loaded. Conversion is unavailable.")
        }
    }

    // MARK: - Candidate Panel

    static let maxCandidateDisplay = 9

    func cursorRect(client: IMKTextInput) -> NSRect {
        var rect = NSRect.zero
        client.attributes(forCharacterIndex: 0, lineHeightRectangle: &rect)
        return rect
    }

    func showCandidatePanel(client: IMKTextInput) {
        let allCandidates: [String]
        let selectedIndex: Int

        switch state {
        case .composing:
            allCandidates = predictionCandidates
            selectedIndex = selectedPredictionIndex
        case .converting:
            guard activeSegmentIndex < conversionSegments.count else { return }
            allCandidates = conversionSegments[activeSegmentIndex].candidates
            selectedIndex = conversionSegments[activeSegmentIndex].selectedIndex
        case .idle:
            return
        }

        let pageSize = Self.maxCandidateDisplay
        let page = selectedIndex / pageSize
        let pageStart = page * pageSize
        let pageEnd = min(pageStart + pageSize, allCandidates.count)
        let pageCandidates = Array(allCandidates[pageStart..<pageEnd])
        let pageSelectedIndex = selectedIndex - pageStart

        let rect = cursorRect(client: client)
        sharedCandidatePanel.show(candidates: pageCandidates, selectedIndex: pageSelectedIndex, cursorRect: rect)
    }

    func hideCandidatePanel() {
        sharedCandidatePanel.hide()
    }

    // MARK: - Key Handling

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard let event = event, let client = sender as? IMKTextInput else {
            return false
        }

        guard event.type == .keyDown else {
            return false
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

        // Shift+Arrow in converting state: segment boundary adjustment (U2)
        if dominated == .shift && state == .converting {
            if event.keyCode == Key.left || event.keyCode == Key.right {
                return handleSegmentBoundaryAdjust(
                    shrink: event.keyCode == Key.left, client: client)
            }
        }

        // Modifier keys (Cmd, Ctrl, etc.) — commit first, then pass through
        // Shift alone is excluded (used for normal text input like ?, !, ~)
        if !dominated.subtracting(.shift).isEmpty {
            if isComposing {
                commitCurrentState(client: client)
            }
            return false
        }

        let keyCode = event.keyCode
        guard let text = event.characters, !text.isEmpty else {
            return false
        }

        NSLog("Lexime: handle keyCode=%d text=%@ state=%d", keyCode, text, stateOrdinal)

        switch state {
        case .idle:
            return handleIdle(keyCode: keyCode, text: text, client: client)
        case .composing:
            return handleComposing(keyCode: keyCode, text: text, client: client)
        case .converting:
            return handleConverting(keyCode: keyCode, text: text, client: client)
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

    private var stateOrdinal: Int {
        switch state {
        case .idle: return 0
        case .composing: return 1
        case .converting: return 2
        }
    }

    // MARK: - Punctuation Composing

    func composePunctuation(_ candidates: [String], client: IMKTextInput) {
        state = .composing
        isPunctuationComposing = true
        composedKana = candidates[0]
        pendingRomaji = ""
        predictionCandidates = candidates
        selectedPredictionIndex = 0
        updateMarkedTextWithCandidate(candidates[0], client: client)
        showCandidatePanel(client: client)
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
        updatePredictions()
    }

    func updatePredictions() {
        selectedPredictionIndex = 0
        if !composedKana.isEmpty {
            var candidates = predictCandidates(composedKana)
            // Ensure the raw kana is always the first candidate
            candidates.removeAll { $0 == composedKana }
            candidates.insert(composedKana, at: 0)
            predictionCandidates = candidates
            if let client = self.client() {
                showCandidatePanel(client: client)
            }
        } else {
            predictionCandidates = []
            hideCandidatePanel()
        }
    }

    private func drainPending(force: Bool) {
        let result = drainPendingRomaji(
            composedKana: composedKana,
            pendingRomaji: pendingRomaji,
            trie: trie,
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
            commitComposed(client: client)
        case .converting:
            commitConversion(client: client)
        }
    }

    func commitConversion(client: IMKTextInput) {
        guard !conversionSegments.isEmpty else { return }
        hideCandidatePanel()
        recordToHistory()
        let fullText = conversionSegments.map { $0.surface }.joined()
        commitText(fullText, client: client)
    }

    private static let historySaveQueue = DispatchQueue(label: "dev.sendsh.lexime.history-save")

    private func recordToHistory() {
        guard let history = sharedHistory else { return }

        // strdup ensures C string pointers remain valid independent of Swift string lifetimes
        var cStrings: [UnsafeMutablePointer<CChar>] = []
        var segments: [LexSegment] = []
        for seg in conversionSegments {
            guard let r = strdup(seg.reading), let s = strdup(seg.surface) else { continue }
            cStrings.append(r)
            cStrings.append(s)
            segments.append(LexSegment(reading: r, surface: s))
        }
        defer { cStrings.forEach { free($0) } }

        segments.withUnsafeBufferPointer { buffer in
            guard let base = buffer.baseAddress else { return }
            lex_history_record(history, base, UInt32(buffer.count))
        }

        // Save asynchronously to avoid blocking key handling
        let path = userHistoryPath
        Self.historySaveQueue.async {
            let result = lex_history_save(history, path)
            if result != 0 {
                NSLog("Lexime: Failed to save user history to %@", path)
            }
        }
    }

    func resetState() {
        composedKana = ""
        pendingRomaji = ""
        originalKana = ""
        conversionSegments = []
        activeSegmentIndex = 0
        predictionCandidates = []
        selectedPredictionIndex = 0
        isPunctuationComposing = false
        state = .idle
    }
}
