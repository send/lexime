import Carbon
import Foundation

/// Monitors input source changes and notifies the user when the system
/// switches to the standard ABC keyboard layout (which can happen
/// unexpectedly due to macOS behaviour), offering a one-tap action to
/// switch back to Lexime via macnotifier.
final class InputSourceMonitor: NSObject {

    private static let abcSourceID = "com.apple.keylayout.ABC"
    private static let leximeJapaneseID = "sh.send.inputmethod.Lexime.Lexime.Japanese"
    private static let leximeRomanID = "sh.send.inputmethod.Lexime.Lexime.Roman"

    /// Suppress notifications for this many seconds after init (avoid startup noise).
    private static let startupQuietPeriod: TimeInterval = 5
    /// Minimum interval between consecutive notifications.
    private static let notificationCooldown: TimeInterval = 5
    /// Polling interval for secure input release detection.
    private static let secureInputPollInterval: TimeInterval = 0.5
    /// Maximum polling duration for secure input (give up after this).
    private static let secureInputPollTimeout: TimeInterval = 60

    private let startTime = Date()
    private var lastNotificationTime: Date?
    private var secureInputTimer: Timer?
    /// The Lexime input source ID that was active before ABC switch.
    private var previousLeximeSourceID: String?

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

        // Track the last Lexime source so we can restore it after ABC switch
        if sourceID.hasPrefix("sh.send.inputmethod.Lexime.") {
            previousLeximeSourceID = sourceID
            return
        }

        guard sourceID == Self.abcSourceID else { return }

        // Startup quiet period
        guard Date().timeIntervalSince(startTime) >= Self.startupQuietPeriod else {
            NSLog("Lexime: ABC detected but within startup quiet period, suppressing")
            return
        }

        // Cooldown
        if let last = lastNotificationTime,
           Date().timeIntervalSince(last) < Self.notificationCooldown {
            NSLog("Lexime: ABC detected but within cooldown, suppressing")
            return
        }

        lastNotificationTime = Date()

        // If secure input is active (e.g. password field), poll for its
        // release and auto-switch back to Lexime instead of notifying.
        if IsSecureEventInputEnabled() {
            NSLog("Lexime: ABC switch detected during secure input, polling for release")
            startSecureInputPolling()
            return
        }

        sendNotification()
    }

    private func sendNotification() {
        guard let macnotifier = findMacnotifier() else {
            NSLog("Lexime: macnotifier not found, skipping notification")
            return
        }

        let helperPath = selectInputHelperPath()
        let revertID = previousLeximeSourceID ?? Self.leximeJapaneseID
        let executeCmd = "\"\(helperPath)\" \(revertID)"

        let task = Process()
        task.executableURL = URL(fileURLWithPath: macnotifier)
        task.arguments = [
            "-t", "Lexime",
            "-m", "標準 ABC に切り替わりました",
            "-e", executeCmd,
        ]

        do {
            try task.run()
            NSLog("Lexime: Sent ABC switch notification via macnotifier")
        } catch {
            NSLog("Lexime: Failed to launch macnotifier: %@", "\(error)")
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
                self.selectPreviousLexime()
            } else if Date() >= deadline {
                timer.invalidate()
                self.secureInputTimer = nil
                NSLog("Lexime: Secure input poll timed out")
            }
        }
    }

    private func selectPreviousLexime() {
        let revertID = previousLeximeSourceID ?? Self.leximeJapaneseID
        let conditions = [
            kTISPropertyInputSourceID as String: revertID
        ] as CFDictionary
        guard let list = TISCreateInputSourceList(conditions, false)?.takeRetainedValue()
                as? [TISInputSource],
              let source = list.first else { return }
        TISSelectInputSource(source)
    }

    // MARK: - Helper Paths

    /// Path to lexime-select-input inside the app bundle.
    private func selectInputHelperPath() -> String {
        if let bundlePath = Bundle.main.executablePath {
            let macosDir = (bundlePath as NSString).deletingLastPathComponent
            return (macosDir as NSString).appendingPathComponent("lexime-select-input")
        }
        return "lexime-select-input"
    }

    /// Find macnotifier in PATH (Homebrew).
    private func findMacnotifier() -> String? {
        let candidates = [
            "/opt/homebrew/bin/macnotifier",
            "/usr/local/bin/macnotifier",
        ]
        for path in candidates {
            if FileManager.default.isExecutableFile(atPath: path) {
                return path
            }
        }
        return nil
    }
}
