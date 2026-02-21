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
        hostingView.frame = NSRect(x: 0, y: 0, width: 540, height: 600)

        let win = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 540, height: 600),
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
        NSApp.setActivationPolicy(.prohibited)
    }
}
