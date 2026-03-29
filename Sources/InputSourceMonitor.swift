import Carbon
import Foundation

/// Monitors input source changes and automatically reverts unexpected
/// switches to the standard ABC keyboard layout (which can happen due to
/// macOS IMKit race conditions) back to Lexime Roman, with secure input
/// awareness (polls for release before reverting).
final class InputSourceMonitor: NSObject {

    private static let abcSourceID = "com.apple.keylayout.ABC"
    // Runtime IDs include the bundle ID prefix, so they are "Lexime.Lexime.*"
    // rather than the bare "Lexime.*" declared in Info.plist's tsInputModeListKey.
    private static let leximeRomanID = "sh.send.inputmethod.Lexime.Lexime.Roman"

    /// Suppress notifications for this many seconds after init (avoid startup noise).
    private static let startupQuietPeriod: TimeInterval = 5
    /// Delay before auto-reverting non-secure ABC switch.
    private static let autoRevertDelay: TimeInterval = 0.3
    /// Polling interval for secure input release detection.
    private static let secureInputPollInterval: TimeInterval = 0.5
    /// Maximum polling duration for secure input (give up after this).
    private static let secureInputPollTimeout: TimeInterval = 60

    private let startTime = Date()
    private var secureInputTimer: Timer?

    func startMonitoring() {
        DistributedNotificationCenter.default().addObserver(
            self,
            selector: #selector(inputSourceDidChange),
            name: NSNotification.Name("com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged"),
            object: nil
        )
        NSLog("Lexime: InputSourceMonitor started")
    }

    deinit {
        secureInputTimer?.invalidate()
        DistributedNotificationCenter.default().removeObserver(self)
    }

    // MARK: - Input Source Change Handling

    @objc private func inputSourceDidChange() {
        guard let source = TISCopyCurrentKeyboardInputSource()?.takeRetainedValue() else { return }
        guard let idRef = TISGetInputSourceProperty(source, kTISPropertyInputSourceID) else { return }
        let sourceID = Unmanaged<CFString>.fromOpaque(idRef).takeUnretainedValue() as String

        guard sourceID == Self.abcSourceID else { return }

        // Startup quiet period
        guard Date().timeIntervalSince(startTime) >= Self.startupQuietPeriod else {
            NSLog("Lexime: ABC detected but within startup quiet period, suppressing")
            return
        }

        // If secure input is active (e.g. password field), poll for its
        // release and auto-switch back to Lexime.
        if IsSecureEventInputEnabled() {
            NSLog("Lexime: ABC switch detected during secure input, polling for release")
            startSecureInputPolling()
            return
        }

        // Non-secure ABC switch (e.g. IMKit race on Eisu/ESC key).
        // Auto-revert after a short delay.
        NSLog("Lexime: unexpected ABC switch detected, auto-reverting in %.1fs", Self.autoRevertDelay)
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.autoRevertDelay) { [weak self] in
            guard let self else { return }
            self.selectLeximeRoman()
        }
    }

    // MARK: - Secure Input Polling

    private func startSecureInputPolling() {
        secureInputTimer?.invalidate()
        let deadline = Date().addingTimeInterval(Self.secureInputPollTimeout)
        secureInputTimer = Timer.scheduledTimer(
            withTimeInterval: Self.secureInputPollInterval, repeats: true
        ) { [weak self] timer in
            guard let self else { timer.invalidate(); return }
            if !IsSecureEventInputEnabled() {
                timer.invalidate()
                self.secureInputTimer = nil
                NSLog("Lexime: Secure input released, switching back to Lexime")
                self.selectLeximeRoman()
            } else if Date() >= deadline {
                timer.invalidate()
                self.secureInputTimer = nil
                NSLog("Lexime: Secure input poll timed out")
            }
        }
    }

    private func selectLeximeRoman() {
        let conditions = [
            kTISPropertyInputSourceID as String: Self.leximeRomanID
        ] as CFDictionary
        guard let list = TISCreateInputSourceList(conditions, false)?.takeRetainedValue()
                as? [TISInputSource],
              let source = list.first else { return }
        TISSelectInputSource(source)
    }

}
