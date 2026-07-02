import AppKit
import Carbon
import Foundation

/// Monitors input source changes and automatically reverts unexpected
/// switches to the standard ABC keyboard layout (which can happen due to
/// macOS IMKit race conditions) back to Lexime Roman, with secure input
/// awareness (polls for release before reverting).
///
/// Only Lexime → ABC transitions are reverted (see `AbcRevertPolicy`): when
/// the user reaches ABC from another IME or layout — a deliberate choice —
/// the monitor leaves it alone.
final class InputSourceMonitor: NSObject {

    /// Suppress notifications for this many seconds after init (avoid startup noise).
    private static let startupQuietPeriod: TimeInterval = 5
    /// Delay before auto-reverting non-secure ABC switch.
    private static let autoRevertDelay: TimeInterval = 0.3
    /// Polling interval for secure input release detection.
    private static let secureInputPollInterval: TimeInterval = 0.5
    /// Maximum polling duration for secure input (give up after this).
    private static let secureInputPollTimeout: TimeInterval = 60
    /// macOS needs a beat after wake before TIS calls reliably take effect.
    private static let wakeRecheckDelay: TimeInterval = 1.0

    private let startTime = Date()
    private let reverter: AbcReverting
    private var secureInputTimer: Timer?
    /// Tracks the last non-ABC source so only Lexime → ABC transitions are
    /// reverted. Main-thread only (notification and wake handlers).
    private var revertPolicy = AbcRevertPolicy()

    init(reverter: AbcReverting = InputSourceReverter.shared) {
        self.reverter = reverter
        super.init()
    }

    func startMonitoring() {
        // Seed the policy so a Lexime → ABC flip right after startup is
        // attributed correctly.
        revertPolicy.observe(InputSource.currentID())
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
        let currentID = InputSource.currentID()
        revertPolicy.observe(currentID)
        guard currentID == LeximeInputSourceID.standardABC else { return }

        // Startup quiet period
        guard Date().timeIntervalSince(startTime) >= Self.startupQuietPeriod else {
            NSLog("Lexime: ABC detected but within startup quiet period, suppressing")
            return
        }

        // Respect a deliberate ABC choice: only reclaim ABC when it was
        // reached from one of Lexime's own modes (IMKit race on Eisu/ESC, or
        // the engine's temporary ABC switch).
        guard revertPolicy.shouldRevert(currentID: currentID) else {
            NSLog("Lexime: ABC selected from a non-Lexime source, leaving it alone")
            return
        }

        // If secure input is active (e.g. password field), poll for its
        // release and auto-switch back to Lexime.
        if IsSecureEventInputEnabled() {
            NSLog("Lexime: ABC switch detected during secure input, polling for release")
            startSecureInputPolling()
            return
        }

        // Unexpected non-secure ABC switch: auto-revert after a short delay.
        NSLog("Lexime: unexpected ABC switch detected, auto-reverting in %.1fs", Self.autoRevertDelay)
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.autoRevertDelay) { [weak self] in
            self?.revertToRoman()
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
            let currentID = InputSource.currentID()
            self.revertPolicy.observe(currentID)
            guard currentID == LeximeInputSourceID.standardABC else { return }
            guard self.revertPolicy.shouldRevert(currentID: currentID) else {
                NSLog("Lexime: wake on ABC but it was not reached from Lexime, leaving it alone")
                return
            }
            if IsSecureEventInputEnabled() {
                NSLog("Lexime: wake on ABC during secure input, polling for release")
                self.startSecureInputPolling()
                return
            }
            NSLog("Lexime: wake on ABC, reverting to Lexime Roman")
            self.revertToRoman()
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
                self.revertToRoman()
            } else if Date() >= deadline {
                timer.invalidate()
                self.secureInputTimer = nil
                NSLog("Lexime: Secure input poll timed out")
            }
        }
    }

    // MARK: - Revert

    /// A bare ABC appearance is normalized to Lexime *Roman*: outside the ESC
    /// race there is no evidence the user was mid-Japanese-input, and Roman
    /// keeps Lexime active with ABC-equivalent typing behavior. (The ESC race
    /// itself is handled first by ModeController, which reverts to Japanese
    /// well before our `autoRevertDelay` fires — by then the source is no
    /// longer ABC and the reverter's per-tick ABC check leaves it alone.)
    ///
    /// All callers reach this after a delay (`autoRevertDelay`, the wake
    /// recheck, or up to `secureInputPollTimeout` of secure-input polling),
    /// so re-validate ownership at fire time: `inputSourceDidChange` kept
    /// observing in the meantime, and if the user moved to another source and
    /// then deliberately re-selected ABC, the claim this revert was scheduled
    /// under no longer holds.
    private func revertToRoman() {
        guard revertPolicy.shouldRevert(currentID: InputSource.currentID()) else {
            NSLog("Lexime: ABC ownership lapsed before revert fired, leaving it alone")
            return
        }
        reverter.revertFromAbc(to: LeximeInputSourceID.roman)
    }

}
