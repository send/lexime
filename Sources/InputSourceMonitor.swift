import Carbon
import Foundation

/// Monitors input source changes and notifies the user when the system
/// switches to the standard ABC keyboard layout (which can happen
/// unexpectedly due to macOS behaviour), offering a one-tap action to
/// switch back to Lexime via macnotifier.
final class InputSourceMonitor: NSObject {

    private static let abcSourceID = "com.apple.keylayout.ABC"
    private static let leximeRomanID = "sh.send.inputmethod.Lexime.Lexime.Roman"

    /// Suppress notifications for this many seconds after init (avoid startup noise).
    private static let startupQuietPeriod: TimeInterval = 5
    /// Minimum interval between consecutive notifications.
    private static let notificationCooldown: TimeInterval = 5

    private let startTime = Date()
    private var lastNotificationTime: Date?

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

        // Cooldown
        if let last = lastNotificationTime,
           Date().timeIntervalSince(last) < Self.notificationCooldown {
            NSLog("Lexime: ABC detected but within cooldown, suppressing")
            return
        }

        lastNotificationTime = Date()
        sendNotification()
    }

    private func sendNotification() {
        guard let macnotifier = findMacnotifier() else {
            NSLog("Lexime: macnotifier not found, skipping notification")
            return
        }

        let helperPath = selectInputHelperPath()
        let executeCmd = "\"\(helperPath)\" \(Self.leximeRomanID)"

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
