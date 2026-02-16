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

    let candidateManager = CandidateManager()
    let ghostManager = GhostTextManager()

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

    // MARK: - Key Handling

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard let session, let event, let client = sender as? IMKTextInput else {
            return false
        }

        guard event.type == .keyDown else {
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
        if ghostManager.text != nil && event.keyCode != 48 /* Tab */ {
            ghostManager.clear(client: client, updateDisplay: true)
        }

        // Invalidate any pending async candidate results
        candidateManager.invalidate()

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
            candidateManager.flagReposition()
        }

        // 2. Marked text
        if let markedText = resp.markedText {
            currentDisplay = markedText
            updateMarkedText(markedText, dashed: resp.isDashedUnderline, client: client)
        }

        // 3. Candidate panel
        switch resp.candidateAction {
        case .hide:
            candidateManager.hide()
        case .show(let surfaces, let selected):
            candidateManager.update(surfaces: surfaces, selected: Int(selected))
            candidateManager.show(client: client, currentDisplay: currentDisplay)
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
        if resp.needsCandidates, let candidateReading = resp.candidateReading, let session {
            candidateManager.dispatchAsync(
                reading: candidateReading,
                dispatch: resp.candidateDispatch,
                session: session
            ) { [weak self] resp in
                guard let self, let client = self.client() else { return }
                self.applyResponse(resp, client: client)
            }
        }

        // 6. Ghost text
        if let ghost = resp.ghostText {
            if ghost.isEmpty {
                // Clear ghost state. Only clear the display if no marked text was set
                // in this same response (step 2 already replaced the screen content).
                ghostManager.clear(client: client, updateDisplay: resp.markedText == nil)
            } else {
                ghostManager.set(ghost, client: client)
            }
        }

        // 7. Ghost generation request (debounced)
        if resp.needsGhostText, let ghostContext = resp.ghostContext, let session {
            ghostManager.requestGeneration(
                context: ghostContext,
                generation: resp.ghostGeneration,
                session: session
            ) { [weak self] resp in
                guard let self, let client = self.client() else { return }
                self.applyResponse(resp, client: client)
            }
        }
    }

    // MARK: - History

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

    // MARK: - Helpers

    private func cycleConversionMode(session: LexSession, client: IMKTextInput) {
        if isComposing {
            let resp = session.commit()
            applyResponse(resp, client: client)
        }
        if ghostManager.text != nil {
            ghostManager.clear(client: client, updateDisplay: true)
        }
        let maxModes = AppContext.shared.neural != nil ? 3 : 2
        let current = UserDefaults.standard.integer(forKey: "conversionMode")
        let next = (current + 1) % maxModes
        UserDefaults.standard.set(next, forKey: "conversionMode")
        session.setConversionMode(mode: UInt8(next))
        let names = ["standard", "predictive", "ghost"]
        NSLog("Lexime: conversion mode → %@", names[next])
        let rect = candidateManager.cursorRect(client: client, currentDisplay: currentDisplay)
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
        candidateManager.reset()
        super.activateServer(sender)
    }

    override func deactivateServer(_ sender: Any!) {
        candidateManager.deactivate()
        ghostManager.deactivate()
        currentDisplay = nil
        super.deactivateServer(sender)
    }

    override func setValue(_ value: Any!, forTag tag: Int, client sender: Any!) {
        if isComposing { return }
        super.setValue(value, forTag: tag, client: sender)
    }
}
