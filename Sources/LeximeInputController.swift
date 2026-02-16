import Carbon
import Cocoa
import InputMethodKit

@objc(LeximeInputController)
class LeximeInputController: IMKInputController {

    // MARK: - State

    var session: LexSession?

    /// Tracks the currently displayed marked text so composedString stays in sync.
    var currentDisplay: String?

    var isComposing: Bool {
        guard let session else { return false }
        return session.isComposing()
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
        let version = engineVersion()
        NSLog("Lexime: InputController initialized (engine: %@)", version)

        guard let dict = AppContext.shared.dict else {
            if !Self.hasShownDictWarning {
                Self.hasShownDictWarning = true
                NSLog("Lexime: WARNING - dictionary not loaded. Conversion is unavailable.")
            }
            return
        }

        session = LexSession(dict: dict, conn: AppContext.shared.conn, history: AppContext.shared.history)
        guard let session else { return }
        session.setDeferCandidates(enabled: true)
        if UserDefaults.standard.bool(forKey: "programmerMode") {
            session.setProgrammerMode(enabled: true)
        }
        let convMode = UserDefaults.standard.integer(forKey: "conversionMode")
        if convMode > 0, convMode <= UInt8.max {
            session.setConversionMode(mode: UInt8(convMode))
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
        session.setProgrammerMode(
            enabled: UserDefaults.standard.bool(forKey: "programmerMode")
        )
        let convMode = min(max(UserDefaults.standard.integer(forKey: "conversionMode"), 0), Int(UInt8.max))
        session.setConversionMode(mode: UInt8(convMode))

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
        let resp = session.handleKey(keyCode: event.keyCode, text: text, flags: flags)
        applyResponse(resp, client: client)
        return resp.consumed
    }

    // MARK: - Apply Response

    private func applyResponse(_ resp: LexKeyResult, client: IMKTextInput) {
        // 1. Commit text
        if let commitText = resp.commitText {
            client.insertText(commitText, replacementRange: NSRange(location: NSNotFound, length: 0))
            currentDisplay = nil
            panelNeedsReposition = true
        }

        // 2. Marked text
        if let markedText = resp.markedText {
            currentDisplay = markedText
            updateMarkedText(markedText, dashed: resp.isDashedUnderline, client: client)
        }

        // 3. Candidate panel
        switch resp.candidateAction {
        case .hide:
            hideCandidatePanel()
        case .show(let surfaces, let selected):
            predictionCandidates = surfaces
            selectedPredictionIndex = Int(selected)
            showCandidatePanel(client: client)
        case .keep:
            break
        }

        // 4. Side effects
        if resp.switchToAbc {
            selectABCInputSource()
        }
        if resp.saveHistory {
            saveHistory()
        }

        // 5. Async candidate generation
        if resp.needsCandidates, let candidateReading = resp.candidateReading {
            dispatchAsyncCandidates(
                reading: candidateReading,
                dispatch: resp.candidateDispatch
            )
        }

        // 6. Ghost text
        if let ghost = resp.ghostText {
            if ghost.isEmpty {
                // Clear ghost state. Only clear the display if no marked text was set
                // in this same response (step 2 already replaced the screen content).
                ghostText = nil
                ghostDebounceItem?.cancel()
                ghostDebounceItem = nil
                if resp.markedText == nil {
                    clearGhostText(client: client)
                }
            } else {
                ghostText = ghost
                showGhostText(ghost, client: client)
            }
        }

        // 7. Ghost generation request (debounced)
        if resp.needsGhostText, let ghostContext = resp.ghostContext {
            requestGhostText(context: ghostContext, generation: resp.ghostGeneration)
        }
    }

    // MARK: - Async Candidates

    private func dispatchAsyncCandidates(reading: String, dispatch: UInt8 = 0) {
        let gen = candidateGeneration
        let dict = AppContext.shared.dict
        let conn = AppContext.shared.conn
        let history = AppContext.shared.history
        let neural = AppContext.shared.neural
        guard let dict else { return }

        // Capture committed context on main thread (LexSession is not thread-safe).
        let context: String
        if dispatch == 2, let session {
            context = session.committedContext()
        } else {
            context = ""
        }

        candidateQueue.async { [weak self] in
            let result: LexCandidateResult
            switch dispatch {
            case 2:  // neural (speculative decode)
                if let neural {
                    result = generateNeuralCandidates(
                        scorer: neural, dict: dict, conn: conn, history: history,
                        context: context, reading: reading, maxResults: 20)
                } else {
                    // Fallback to standard if neural model not loaded
                    result = generateCandidates(dict: dict, conn: conn, history: history, reading: reading, maxResults: 20)
                }
            case 1:  // prediction (Viterbi + bigram chaining)
                result = generatePredictionCandidates(dict: dict, conn: conn, history: history, reading: reading, maxResults: 20)
            default: // standard
                result = generateCandidates(dict: dict, conn: conn, history: history, reading: reading, maxResults: 20)
            }
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                guard self.candidateGeneration == gen else { return }
                guard let client = self.client() else { return }
                guard let session = self.session else { return }
                guard let resp = session.receiveCandidates(reading: reading, result: result) else { return }
                self.applyResponse(resp, client: client)
            }
        }
    }

    // MARK: - History

    /// Persist history to disk asynchronously.
    /// History records are automatically recorded inside handle_key by the Rust API.
    private func saveHistory() {
        guard let history = AppContext.shared.history else { return }
        let path = AppContext.shared.historyPath
        Self.historySaveQueue.async {
            do {
                try history.save(path: path)
            } catch {
                NSLog("Lexime: Failed to save user history to %@: %@", path, "\(error)")
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
        let neural = AppContext.shared.neural
        guard let neural else { return }
        let item = DispatchWorkItem { [weak self] in
            guard let text = neural.generateGhost(context: context, maxTokens: 30) else { return }
            guard !text.isEmpty else { return }
            DispatchQueue.main.async { [weak self] in
                guard let self, let session = self.session else { return }
                guard let resp = session.receiveGhostText(generation: generation, text: text) else { return }
                guard let client = self.client() else { return }
                self.applyResponse(resp, client: client)
            }
        }
        ghostDebounceItem = item
        ghostQueue.asyncAfter(deadline: .now() + 0.15, execute: item)
    }

    // MARK: - Helpers

    private func cycleConversionMode(session: LexSession, client: IMKTextInput) {
        if isComposing {
            let resp = session.commit()
            applyResponse(resp, client: client)
        }
        if ghostText != nil {
            clearGhostDisplay(client: client)
        }
        let maxModes = AppContext.shared.neural != nil ? 3 : 2
        let current = UserDefaults.standard.integer(forKey: "conversionMode")
        let next = (current + 1) % maxModes
        UserDefaults.standard.set(next, forKey: "conversionMode")
        session.setConversionMode(mode: UInt8(next))
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
        let resp = session.commit()
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
