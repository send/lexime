import AppKit
import Carbon
import Foundation

/// Monitors input source changes and automatically reverts unexpected
/// switches to the standard ABC keyboard layout (which can happen due to
/// macOS IMKit race conditions) back to Lexime Roman, with secure input
/// awareness (polls for release before reverting).
final class InputSourceMonitor: NSObject {

    private static let abcSourceID = "com.apple.keylayout.ABC"

    /// Suppress notifications for this many seconds after init (avoid startup noise).
    private static let startupQuietPeriod: TimeInterval = 5
    /// Delay before auto-reverting non-secure ABC switch.
    private static let autoRevertDelay: TimeInterval = 0.3
    /// Polling interval for secure input release detection.
    private static let secureInputPollInterval: TimeInterval = 0.5
    /// Maximum polling duration for secure input (give up after this).
    private static let secureInputPollTimeout: TimeInterval = 60
    /// Delay after wake before rechecking input source.
    private static let wakeRecheckDelay: TimeInterval = 1.0
    /// Interval between retry attempts when revert fails.
    private static let revertRetryInterval: TimeInterval = 0.05
    /// Max retry attempts verifying revert took effect.
    private static let revertRetryMaxAttempts = 5

    private let startTime = Date()
    private var secureInputTimer: Timer?

    func startMonitoring() {
        DistributedNotificationCenter.default().addObserver(
            self,
            selector: #selector(inputSourceDidChange),
            name: NSNotification.Name("com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged"),
            object: nil
        )
        NSWorkspace.shared.notificationCenter.addObserver(
            self,
            selector: #selector(didWake),
            name: NSWorkspace.didWakeNotification,
            object: nil
        )
        NSLog("Lexime: InputSourceMonitor started")
    }

    deinit {
        secureInputTimer?.invalidate()
        DistributedNotificationCenter.default().removeObserver(self)
        NSWorkspace.shared.notificationCenter.removeObserver(self)
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
            self?.revertFromAbcWithRetry()
        }
    }

    // MARK: - Wake Handling

    /// After sleep/wake, macOS often ends up on ABC without firing a
    /// TISNotifySelectedKeyboardInputSourceChanged we can act on in time,
    /// so re-check explicitly once the system has settled.
    @objc private func didWake() {
        NSLog("Lexime: wake detected, rechecking input source in %.1fs", Self.wakeRecheckDelay)
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.wakeRecheckDelay) { [weak self] in
            guard let self else { return }
            guard self.isCurrentInputSourceAbc() else { return }
            if IsSecureEventInputEnabled() {
                NSLog("Lexime: wake on ABC during secure input, polling for release")
                self.startSecureInputPolling()
                return
            }
            NSLog("Lexime: wake on ABC, reverting to Lexime Roman")
            self.revertFromAbcWithRetry()
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
                self.revertFromAbcWithRetry()
            } else if Date() >= deadline {
                timer.invalidate()
                self.secureInputTimer = nil
                NSLog("Lexime: Secure input poll timed out")
            }
        }
    }

    private func selectLeximeRoman() {
        let conditions = [
            kTISPropertyInputSourceID as String: LeximeInputSourceID.roman
        ] as CFDictionary
        guard let list = TISCreateInputSourceList(conditions, false)?.takeRetainedValue()
                as? [TISInputSource],
              let source = list.first else { return }
        TISSelectInputSource(source)
    }

    private func isCurrentInputSourceAbc() -> Bool {
        guard let current = TISCopyCurrentKeyboardInputSource()?.takeRetainedValue() else { return false }
        guard let idRef = TISGetInputSourceProperty(current, kTISPropertyInputSourceID) else { return false }
        let id = Unmanaged<CFString>.fromOpaque(idRef).takeUnretainedValue() as String
        return id == Self.abcSourceID
    }

    /// TISSelectInputSource can silently fail during wake or other input source
    /// transitions. Verify the switch took effect and retry if still on ABC.
    private func revertFromAbcWithRetry(attempt: Int = 0) {
        selectLeximeRoman()
        guard attempt + 1 < Self.revertRetryMaxAttempts else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.revertRetryInterval) { [weak self] in
            guard let self else { return }
            guard self.isCurrentInputSourceAbc() else { return }
            self.revertFromAbcWithRetry(attempt: attempt + 1)
        }
    }

}
