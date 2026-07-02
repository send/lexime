import Cocoa
import SwiftUI

class SettingsWindowController {

    static let shared = SettingsWindowController()

    private var window: NSWindow?
    private var closeObserver: NSObjectProtocol?

    func showWindow() {
        if let window {
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        NSApp.setActivationPolicy(.accessory)

        let settingsView = SettingsView()
        let hostingView = NSHostingView(rootView: settingsView)
        hostingView.frame = NSRect(x: 0, y: 0, width: 540, height: 700)

        let win = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 540, height: 700),
            styleMask: [.titled, .closable, .resizable],
            backing: .buffered,
            defer: false
        )
        win.title = NSLocalizedString("Lexime 設定", comment: "")
        win.contentView = hostingView
        win.center()
        win.isReleasedWhenClosed = false

        closeObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification,
            object: win,
            queue: .main
        ) { [weak self] _ in
            self?.windowDidClose()
        }

        window = win
        win.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    private func windowDidClose() {
        if let observer = closeObserver {
            NotificationCenter.default.removeObserver(observer)
            closeObserver = nil
        }
        window = nil
        restoreBackgroundActivationPolicy()
    }

    /// Restore the background-process activation policy (`.prohibited`)
    /// unless the settings window is still alive — including miniaturized
    /// or hidden states, where the window exists but is not visible.
    /// Shared with other transient UI (e.g. the engine-failure alert) so
    /// policy restoration follows a single ownership rule.
    func restoreBackgroundActivationPolicy() {
        guard window == nil else { return }
        NSApp.setActivationPolicy(.prohibited)
    }
}
