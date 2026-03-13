import Carbon
import Foundation
import UserNotifications

/// Monitors input source changes and notifies the user when the system
/// switches to the standard ABC keyboard layout (which can happen
/// unexpectedly due to macOS behaviour), offering a one-tap action to
/// switch back to Lexime.
final class InputSourceMonitor: NSObject, UNUserNotificationCenterDelegate {

    private static let abcSourceID = "com.apple.keylayout.ABC"
    private static let leximeJapaneseID = "sh.send.inputmethod.Lexime.Japanese"
    private static let leximeRomanID = "sh.send.inputmethod.Lexime.Roman"
    private static let notificationCategoryID = "LEXIME_ABC_SWITCH"
    private static let switchActionID = "SWITCH_TO_LEXIME"

    /// Suppress notifications for this many seconds after init (avoid startup noise).
    private static let startupQuietPeriod: TimeInterval = 5
    /// Minimum interval between consecutive notifications.
    private static let notificationCooldown: TimeInterval = 5

    private let notificationCenter = UNUserNotificationCenter.current()
    private let startTime = Date()
    private var lastNotificationTime: Date?

    func startMonitoring() {
        configureNotificationCenter()
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

    // MARK: - Notification Centre Setup

    private func configureNotificationCenter() {
        notificationCenter.delegate = self

        let switchAction = UNNotificationAction(
            identifier: Self.switchActionID,
            title: "Lexime に切り替え",
            options: [.foreground]
        )
        let category = UNNotificationCategory(
            identifier: Self.notificationCategoryID,
            actions: [switchAction],
            intentIdentifiers: [],
            options: []
        )
        notificationCenter.setNotificationCategories([category])
        notificationCenter.requestAuthorization(options: [.alert, .sound]) { granted, error in
            if let error {
                NSLog("Lexime: Notification authorization error: %@", "\(error)")
            } else {
                NSLog("Lexime: Notification authorization granted: %@", "\(granted)")
            }
        }
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
        let content = UNMutableNotificationContent()
        content.title = "Lexime"
        content.body = "標準 ABC に切り替わりました"
        content.categoryIdentifier = Self.notificationCategoryID
        content.sound = .default

        let request = UNNotificationRequest(
            identifier: "lexime-abc-switch-\(UUID().uuidString)",
            content: content,
            trigger: nil
        )
        notificationCenter.add(request) { error in
            if let error {
                NSLog("Lexime: Failed to deliver notification: %@", "\(error)")
            }
        }
        NSLog("Lexime: Sent ABC switch notification")
    }

    // MARK: - UNUserNotificationCenterDelegate

    /// Handle notification tap (default action) or explicit "Switch to Lexime" action.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let actionID = response.actionIdentifier
        if actionID == Self.switchActionID
            || actionID == UNNotificationDefaultActionIdentifier {
            selectLeximeInputSource()
        }
        completionHandler()
    }

    /// Show notifications even when the app is in the foreground.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound])
    }

    // MARK: - Input Source Selection

    private func selectLeximeInputSource() {
        let conditions = [
            kTISPropertyInputSourceID as String: Self.leximeJapaneseID
        ] as CFDictionary
        guard let list = TISCreateInputSourceList(conditions, false)?.takeRetainedValue()
                as? [TISInputSource],
              let source = list.first else {
            NSLog("Lexime: Could not find Lexime Japanese input source")
            return
        }
        TISSelectInputSource(source)
        NSLog("Lexime: Switched back to Lexime Japanese")
    }
}
