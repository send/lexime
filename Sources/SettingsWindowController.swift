import Cocoa
import SwiftUI

class SettingsWindowController {

    static let shared = SettingsWindowController()

    private var window: NSWindow?

    func showWindow() {
        if let window, window.isVisible {
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        NSApp.setActivationPolicy(.accessory)

        let settingsView = SettingsView()
        let hostingView = NSHostingView(rootView: settingsView)
        hostingView.frame = NSRect(x: 0, y: 0, width: 540, height: 600)

        let win = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 540, height: 600),
            styleMask: [.titled, .closable, .resizable],
            backing: .buffered,
            defer: false
        )
        win.title = "Lexime 設定"
        win.contentView = hostingView
        win.center()
        win.isReleasedWhenClosed = false

        NotificationCenter.default.addObserver(
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
        NSApp.setActivationPolicy(.prohibited)
    }
}
