import Carbon
import Cocoa
import InputMethodKit

@objc(LeximeInputController)
class LeximeInputController: IMKInputController {

    // MARK: - State

    private let candidateManager = CandidateManager(panel: AppContext.shared.candidatePanel)
    private let modeController = ModeController()
    private var coordinator: SessionCoordinator?

    private static var hasShownEngineFailureAlert = false
    private static var hasLoggedEngineUnavailable = false

    private lazy var cachedTrigger: LexTriggerKey? = snippetTriggerKey()

    override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)
        let version = engineVersion()
        NSLog("Lexime: InputController initialized (engine: %@)", version)

        guard let engine = AppContext.shared.engine else {
            // Alert is deferred to the first real key press (handle()) so that
            // short-lived IMKit probe processes never trigger UI.
            if !Self.hasLoggedEngineUnavailable {
                Self.hasLoggedEngineUnavailable = true
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
        guard let event, let client = sender as? IMKTextInput else {
            return false
        }

        guard let coordinator else {
            // Engine failed to initialize: surface the fatal failure once,
            // on the first real key press (never on IMKit probe processes).
            if event.type == .keyDown {
                Self.showEngineFailureAlertIfNeeded()
            }
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

    // MARK: - Engine Failure Visibility

    /// Show a one-time critical alert when the engine is unavailable.
    /// Called from the first real key press; the IME is a background process,
    /// so temporarily switch the activation policy (same pattern as
    /// SettingsWindowController) to bring the alert to the front.
    private static func showEngineFailureAlertIfNeeded() {
        guard !hasShownEngineFailureAlert else { return }
        hasShownEngineFailureAlert = true

        let detail = AppContext.shared.engineContainer.fatalFailureDetail ?? ""
        DispatchQueue.main.async {
            NSApp.setActivationPolicy(.accessory)
            NSApp.activate(ignoringOtherApps: true)
            let alert = NSAlert()
            alert.alertStyle = .critical
            alert.messageText = NSLocalizedString(
                "Lexime の辞書の読み込みに失敗しました",
                comment: "Engine failure alert title")
            alert.informativeText = String(
                format: NSLocalizedString(
                    "かな漢字変換は利用できません。Lexime を再インストールしてください。\n\n詳細: %@",
                    comment: "Engine failure alert body"),
                detail)
            alert.runModal()
            // Restore the background-process policy. SettingsWindowController
            // owns the policy while its window is alive (visible, miniaturized,
            // or hidden), so defer the decision to its shared logic.
            SettingsWindowController.shared.restoreBackgroundActivationPolicy()
        }
    }

    // MARK: - Menu

    override func menu() -> NSMenu! {
        let menu = NSMenu()

        // Re-derived on every open, so a runtime issue that has since healed
        // stops being shown. See DegradedStatus for why the polled issues are
        // not merged into the container's latched initFailures.
        let control = AppContext.shared.makeEngineControlService()
        let rows = DegradedStatus.rows(
            initFailures: AppContext.shared.engineContainer.initFailures,
            runtimeIssues: control.historyDurabilityIssues())
        if !rows.isEmpty {
            for row in rows {
                // Status rows are disabled — except the one whose durable
                // record a click retires. Building the menu is not delivery:
                // IMKit calls this method on its own, so acknowledging here
                // would consume the #312 report on a launch that displayed
                // nothing (measured: four seconds after an untouched relaunch).
                let item = NSMenuItem(
                    title: row.title,
                    action: row.acknowledgeable ? #selector(acknowledgeDeletionReport) : nil,
                    keyEquivalent: "")
                item.target = row.acknowledgeable ? self : nil
                item.isEnabled = row.acknowledgeable
                menu.addItem(item)
            }
            menu.addItem(.separator())
        }

        let settingsItem = NSMenuItem(
            title: NSLocalizedString("設定...", comment: "Settings menu item"),
            action: #selector(showSettings),
            keyEquivalent: ","
        )
        settingsItem.target = self
        menu.addItem(settingsItem)
        return menu
    }

    /// The user clicked the lost-deletion row, which is the only evidence
    /// this process can have that the report reached a person.
    @objc private func acknowledgeDeletionReport() {
        AppContext.shared.makeEngineControlService().acknowledgeHistoryReport()
        AppContext.shared.engineContainer.retractDeletionLostRow()
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
        // If IMKit skipped the previous controller's deactivateServer (e.g. the
        // client crashed), the shared panel may still be visible at the old
        // cursor position — and a visible panel takes the no-reposition fast
        // path on the next show(). Hide it before reuse. Note: deactivate()
        // cancels only this controller's own pending deferred shows; the
        // shared panel has no cross-controller cancellation.
        candidateManager.deactivate()
        candidateManager.reset()
        super.activateServer(sender)
    }

    override func deactivateServer(_ sender: Any!) {
        // The settle below performs the commit `commitComposition` would have,
        // so the ESC flag has to be consumed with it. Its contract is "the next
        // commit is the ESC-caused one"; once we commit, every later reading is
        // a lie, and a stale `true` fires `revertWhenAbcAppears` — which has no
        // provenance check — yanking a deliberate ABC switch back to Japanese.
        //
        // Consumed but the revert is NOT armed. `revertToLeximeIfEscapeRaced`
        // is a global TIS action and focus has already left this client: its
        // watch would overlap macOS restoring the *newly focused* app's input
        // source and could drag the user into Lexime Japanese in an app where
        // they never chose it. The genuine-ESC-race residual is still covered,
        // at lower fidelity, by InputSourceMonitor; a spurious flip *into*
        // Japanese has no net at all. That asymmetry is what decides it.
        // Distinguishing "deactivate caused by the ESC input-source flip" from
        // "deactivate caused by focus moving away" would need a signal IMKit
        // does not provide.
        _ = modeController.takePendingEscapeCommit()
        guard let coordinator else {
            super.deactivateServer(sender)
            return
        }
        // `super.deactivateServer` is the other half of the same teardown, so it
        // has to wait on the same condition. If the coordinator defers (a client
        // callback re-entered us from inside `insertText`), deactivating the
        // host here would strand the rest of the response: its `.setMarkedText`
        // would land on an already torn-down client.
        // Captured **strongly** on purpose. This runs after this override has
        // returned, and the focus loss that triggered it is exactly when IMKit
        // may release the controller — a weak capture would go nil and silently
        // skip the superclass teardown this deferral exists to order. The cycle
        // it creates is bounded: the coordinator clears `pendingTeardown`
        // before invoking it, and the delivery it waits on is a synchronous
        // main-thread loop that always unwinds.
        coordinator.deactivate(client: sender as? IMKTextInput) {
            self.finishDeactivateServer(sender)
        }
    }

    /// The superclass half of `deactivateServer`, split out so it can run after
    /// a deferred coordinator teardown (`super` cannot be called from a closure).
    private func finishDeactivateServer(_ sender: Any!) {
        super.deactivateServer(sender)
    }

    // Handle internal mode switching (Eisu → Roman, Kana → Japanese).
    // macOS calls setValue when input mode changes (Eisu/Kana keys, menu bar selection).
    // We intercept to toggle abc_passthrough in the Rust engine.
    // Other mode changes (Caps Lock etc.) are blocked during composition.
    override func setValue(_ value: Any!, forTag tag: Int, client sender: Any!) {
        // The mode IDs IMKit passes here are the tsInputModeListKey keys from
        // Info.plist, which are identical to the TIS input source IDs.
        let modeID = value as? String ?? ""

        if modeID == LeximeInputSourceID.roman {
            if let coordinator, coordinator.isComposing, let client = sender as? IMKTextInput {
                coordinator.commit(client: client)
            }
            coordinator?.setAbcPassthrough(enabled: true)
            super.setValue(value, forTag: tag, client: sender)
            return
        }
        if modeID == LeximeInputSourceID.japanese {
            coordinator?.setAbcPassthrough(enabled: false)
            super.setValue(value, forTag: tag, client: sender)
            return
        }

        if coordinator?.isComposing == true { return }
        super.setValue(value, forTag: tag, client: sender)
    }
}
