import Carbon
import Cocoa
import InputMethodKit

@objc(LeximeInputController)
class LeximeInputController: IMKInputController {

    // MARK: - State

    private var session: LexSession?

    /// Tracks the currently displayed marked text so composedString stays in sync.
    private var currentDisplay: String?

    var isComposing: Bool {
        guard let session else { return false }
        return session.isComposing()
    }

    let candidateManager = CandidateManager()

    private static let japaneseModeID = "sh.send.inputmethod.Lexime.Japanese"
    private static let romanModeID = "sh.send.inputmethod.Lexime.Roman"

    private static var hasShownDictWarning = false

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
        let convMode = UserDefaults.standard.integer(forKey: "conversionMode")
        if convMode == 1 {
            session.setConversionMode(mode: .predictive)
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

        let hasShift = dominated.contains(.shift)
        let hasModifier = !dominated.subtracting(.shift).isEmpty

        // Invalidate any pending async candidate results
        candidateManager.invalidate()

        let text = event.characters ?? ""

        // Build platform-independent key event.
        // Eisu/Kana are always mode-switch regardless of modifiers.
        // For all other keys, modifier → commit + passthrough (ModifiedKey).
        let keyEvent: LexKeyEvent
        switch event.keyCode {
        case 102: keyEvent = .switchToDirectInput
        case 104: keyEvent = .switchToJapanese
        default:
            if hasModifier {
                keyEvent = .modifiedKey
            } else {
                switch event.keyCode {
                case 36:  keyEvent = .enter
                case 49:  keyEvent = .space
                case 51:  keyEvent = .backspace
                case 53:  keyEvent = .escape
                case 48:  keyEvent = .tab
                case 117: keyEvent = .forwardDelete
                case 125: keyEvent = .arrowDown
                case 126: keyEvent = .arrowUp
                default:
                    if let remapped = keymapGet(keyCode: event.keyCode, hasShift: hasShift) {
                        keyEvent = .remapped(text: remapped, shift: hasShift)
                    } else {
                        keyEvent = .text(text: text, shift: hasShift)
                    }
                }
            }
        }

        let resp = session.handleKey(event: keyEvent)
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
            case .setMarkedText(let text):
                currentDisplay = text.isEmpty ? nil : text
                updateMarkedText(text, client: client)
            case .showCandidates(let surfaces, let selected):
                candidateManager.update(surfaces: surfaces, selected: Int(selected))
                candidateManager.show(client: client, currentDisplay: currentDisplay)
            case .hideCandidates:
                candidateManager.hide()
            case .switchToAbc:
                selectABCInputSource()
            case .schedulePoll:
                schedulePollTimer()
            }
        }
    }

    // MARK: - Poll Timer

    private func schedulePollTimer() {
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

    // MARK: - Helpers

    private func selectABCInputSource() {
        let conditions = [
            kTISPropertyInputSourceID as String: "com.apple.keylayout.ABC"
        ] as CFDictionary
        guard let list = TISCreateInputSourceList(conditions, false)?.takeRetainedValue()
                as? [TISInputSource],
              let source = list.first else { return }
        TISSelectInputSource(source)
    }

    // MARK: - Menu

    override func menu() -> NSMenu! {
        let menu = NSMenu()
        let settingsItem = NSMenuItem(
            title: NSLocalizedString("設定...", comment: "Settings menu item"),
            action: #selector(showSettings),
            keyEquivalent: ","
        )
        settingsItem.target = self
        menu.addItem(settingsItem)
        return menu
    }

    @objc private func showSettings() {
        SettingsWindowController.shared.showWindow()
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
        currentDisplay = nil
        super.deactivateServer(sender)
    }

    // Handle internal mode switching (Eisu → Roman, Kana → Japanese).
    // macOS calls setValue when input mode changes (Eisu/Kana keys, menu bar selection).
    // We intercept to toggle abc_passthrough in the Rust engine.
    // Other mode changes (Caps Lock etc.) are blocked during composition.
    override func setValue(_ value: Any!, forTag tag: Int, client sender: Any!) {
        let modeID = value as? String ?? ""

        if modeID == Self.romanModeID {
            if isComposing, let session, let client = sender as? IMKTextInput {
                let resp = session.commit()
                applyEvents(resp, client: client)
            }
            session?.setAbcPassthrough(enabled: true)
            super.setValue(value, forTag: tag, client: sender)
            return
        }
        if modeID == Self.japaneseModeID {
            session?.setAbcPassthrough(enabled: false)
            super.setValue(value, forTag: tag, client: sender)
            return
        }

        // Block other mode changes (Caps Lock etc.) during composing
        if isComposing { return }
        super.setValue(value, forTag: tag, client: sender)
    }
}
