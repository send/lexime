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

    private var pollTimer: Timer?

    override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)
        let version = engineVersion()
        NSLog("Lexime: InputController initialized (engine: %@)", version)

        guard let engine = AppContext.shared.engine else {
            if !Self.hasShownDictWarning {
                Self.hasShownDictWarning = true
                NSLog("Lexime: WARNING - engine not loaded. Conversion is unavailable.")
            }
            return
        }

        session = engine.createSession()
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

        // Poll for completed async results before handling new key
        while let asyncResp = session.poll() {
            applyEvents(asyncResp, client: client)
        }
        cancelPollTimer()

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
        if ghostManager.text != nil && event.keyCode != 48 /* Tab */ {
            ghostManager.clear(client: client, updateDisplay: true)
        }

        // Invalidate any pending async candidate results
        candidateManager.invalidate()

        let text = event.characters ?? ""
        let resp = session.handleKey(keyCode: event.keyCode, text: text, flags: flags)
        applyEvents(resp, client: client)
        return resp.consumed
    }

    // MARK: - Apply Events

    private func applyEvents(_ resp: LexKeyResponse, client: IMKTextInput) {
        for event in resp.events {
            switch event {
            case .commit(let text):
                client.insertText(text, replacementRange: NSRange(location: NSNotFound, length: 0))
                currentDisplay = nil
                candidateManager.flagReposition()
            case .setMarkedText(let text, let dashed):
                currentDisplay = text
                updateMarkedText(text, dashed: dashed, client: client)
            case .clearMarkedText:
                currentDisplay = nil
                updateMarkedText("", dashed: false, client: client)
            case .showCandidates(let surfaces, let selected):
                candidateManager.update(surfaces: surfaces, selected: Int(selected))
                candidateManager.show(client: client, currentDisplay: currentDisplay)
            case .hideCandidates:
                candidateManager.hide()
            case .switchToAbc:
                selectABCInputSource()
            case .saveHistory:
                saveHistory()
            case .setGhostText(let text):
                ghostManager.set(text, client: client)
            case .clearGhostText(let updateDisplay):
                ghostManager.clear(client: client, updateDisplay: updateDisplay)
            case .schedulePoll:
                schedulePollTimer(client: client)
            }
        }
    }

    // MARK: - Poll Timer

    private func schedulePollTimer(client: IMKTextInput) {
        guard pollTimer == nil else { return }
        var idleTicks = 0
        pollTimer = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { [weak self] _ in
            guard let self, let session = self.session, let client = self.client() else {
                self?.cancelPollTimer()
                return
            }
            var hadResult = false
            while let resp = session.poll() {
                self.applyEvents(resp, client: client)
                hadResult = true
            }
            if hadResult {
                idleTicks = 0
            } else {
                idleTicks += 1
                // Stop polling after ~5s of no results (100 * 50ms)
                if idleTicks >= 100 {
                    self.cancelPollTimer()
                }
            }
        }
    }

    private func cancelPollTimer() {
        pollTimer?.invalidate()
        pollTimer = nil
    }

    // MARK: - History

    /// Persist history to disk asynchronously.
    /// History records are automatically recorded inside handle_key by the Rust API.
    private func saveHistory() {
        guard let engine = AppContext.shared.engine else { return }
        let path = AppContext.shared.historyPath
        Self.historySaveQueue.async {
            do {
                try engine.saveHistory(path: path)
            } catch {
                NSLog("Lexime: Failed to save user history to %@: %@", path, "\(error)")
            }
        }
    }

    // MARK: - Helpers

    private func cycleConversionMode(session: LexSession, client: IMKTextInput) {
        if isComposing {
            let resp = session.commit()
            applyEvents(resp, client: client)
        }
        if ghostManager.text != nil {
            ghostManager.clear(client: client, updateDisplay: true)
        }
        let maxModes = (AppContext.shared.engine?.hasNeural() ?? false) ? 3 : 2
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
        applyEvents(resp, client: client)
    }

    override func activateServer(_ sender: Any!) {
        currentDisplay = nil
        candidateManager.reset()
        super.activateServer(sender)
    }

    override func deactivateServer(_ sender: Any!) {
        cancelPollTimer()
        candidateManager.deactivate()
        ghostManager.deactivate()
        currentDisplay = nil
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
