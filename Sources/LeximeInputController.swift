import Carbon
import Cocoa
import InputMethodKit

@objc(LeximeInputController)
class LeximeInputController: IMKInputController {

    // MARK: - State

    private let candidateManager = CandidateManager()
    private let modeController = ModeController()
    private var coordinator: SessionCoordinator?

    private static var hasShownDictWarning = false

    private lazy var cachedTrigger: LexTriggerKey? = snippetTriggerKey()

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

        let modeController = self.modeController
        let convMode = UserDefaults.standard.integer(forKey: DefaultsKey.conversionMode)
        coordinator = SessionCoordinator(
            factory: { listener in
                let session = engine.createSession(listener: listener)
                session.setDeferCandidates(enabled: true)
                session.setSnippetStore(store: AppContext.shared.snippetStore)
                if convMode == 1 {
                    session.setConversionMode(mode: .predictive)
                }
                return session
            },
            candidateManager: candidateManager,
            onSwitchToAbc: { modeController.selectStandardABC() })

        NotificationCenter.default.addObserver(
            self, selector: #selector(snippetsDidReload),
            name: .snippetsDidReload, object: nil)
    }

    deinit {
        NotificationCenter.default.removeObserver(self, name: .snippetsDidReload, object: nil)
    }

    @objc private func snippetsDidReload() {
        coordinator?.setSnippetStore(AppContext.shared.snippetStore)
    }

    override func recognizedEvents(_ sender: Any!) -> Int {
        let mask = NSEvent.EventTypeMask.keyDown.union(.flagsChanged)
        return Int(mask.rawValue)
    }

    // MARK: - Key Handling

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard let coordinator, let event, let client = sender as? IMKTextInput else {
            return false
        }

        guard event.type == .keyDown else {
            // Consume modifier-only events while composing
            return coordinator.isComposing
        }

        let dominated = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            .subtracting([.capsLock, .numericPad, .function])

        let hasShift = dominated.contains(.shift)
        let hasModifier = !dominated.subtracting(.shift).isEmpty
        let text = event.characters ?? ""

        let keyEvent = buildKeyEvent(
            event: event, dominated: dominated,
            hasShift: hasShift, hasModifier: hasModifier, text: text)

        if case .escape = keyEvent, coordinator.isComposing {
            modeController.noteEscapeDuringCompose()
        }

        return coordinator.handleKey(keyEvent, client: client)
    }

    /// Build platform-independent key event.
    /// Eisu/Kana are always mode-switch regardless of modifiers.
    /// For all other keys, modifier → commit + passthrough (ModifiedKey).
    private func buildKeyEvent(event: NSEvent,
                               dominated: NSEvent.ModifierFlags,
                               hasShift: Bool,
                               hasModifier: Bool,
                               text: String) -> LexKeyEvent {
        switch event.keyCode {
        case 102: return .switchToDirectInput
        case 104: return .switchToJapanese
        default:
            if isSnippetTrigger(event: event, dominated: dominated) {
                return .snippetTrigger
            }
            if hasModifier {
                return .modifiedKey
            }
            switch event.keyCode {
            case 36:  return .enter
            case 49:  return .space
            case 51:  return .backspace
            case 53:  return .escape
            case 48:  return .tab
            case 117: return .forwardDelete
            case 125: return .arrowDown
            case 126: return .arrowUp
            default:
                if let remapped = keymapGet(keyCode: event.keyCode, hasShift: hasShift) {
                    return .remapped(text: remapped, shift: hasShift)
                }
                return .text(text: text, shift: hasShift)
            }
        }
    }

    private func isSnippetTrigger(event: NSEvent, dominated: NSEvent.ModifierFlags) -> Bool {
        guard let trigger = cachedTrigger else { return false }
        guard event.charactersIgnoringModifiers == trigger.char else { return false }
        return dominated.contains(.control) == trigger.ctrl
            && dominated.contains(.shift) == trigger.shift
            && dominated.contains(.option) == trigger.alt
            && dominated.contains(.command) == trigger.cmd
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
        return coordinator?.currentDisplay ?? ""
    }

    override func originalString(_ sender: Any!) -> NSAttributedString! {
        return NSAttributedString(string: coordinator?.currentDisplay ?? "")
    }

    override func commitComposition(_ sender: Any!) {
        guard let coordinator, let client = sender as? IMKTextInput else { return }
        let wasEscapeCommit = modeController.takePendingEscapeCommit()
        coordinator.commit(client: client)
        if wasEscapeCommit {
            modeController.revertToLeximeIfEscapeRaced()
        }
    }

    override func activateServer(_ sender: Any!) {
        coordinator?.resetDisplay()
        candidateManager.reset()
        super.activateServer(sender)
    }

    override func deactivateServer(_ sender: Any!) {
        coordinator?.deactivate()
        super.deactivateServer(sender)
    }

    // Handle internal mode switching (Eisu → Roman, Kana → Japanese).
    // macOS calls setValue when input mode changes (Eisu/Kana keys, menu bar selection).
    // We intercept to toggle abc_passthrough in the Rust engine.
    // Other mode changes (Caps Lock etc.) are blocked during composition.
    override func setValue(_ value: Any!, forTag tag: Int, client sender: Any!) {
        let modeID = value as? String ?? ""

        if modeID == ModeController.romanModeID {
            if let coordinator, coordinator.isComposing, let client = sender as? IMKTextInput {
                coordinator.commit(client: client)
            }
            coordinator?.setAbcPassthrough(enabled: true)
            super.setValue(value, forTag: tag, client: sender)
            return
        }
        if modeID == ModeController.japaneseModeID {
            coordinator?.setAbcPassthrough(enabled: false)
            super.setValue(value, forTag: tag, client: sender)
            return
        }

        if coordinator?.isComposing == true { return }
        super.setValue(value, forTag: tag, client: sender)
    }
}
