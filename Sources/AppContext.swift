import Carbon
import Foundation

// MARK: - Input Source IDs

/// Runtime input source IDs for TIS API lookups (TISCreateInputSourceList etc).
/// macOS prefixes the bundle identifier to the Info.plist tsInputModeListKey
/// mode IDs, so "sh.send.inputmethod.Lexime.Japanese" in Info.plist becomes
/// "sh.send.inputmethod.Lexime.Lexime.Japanese" at runtime. These IDs are
/// derived from Bundle.main.bundleIdentifier to stay in sync automatically.
enum LeximeInputSourceID {
    private static let bundleID = Bundle.main.bundleIdentifier ?? "sh.send.inputmethod.Lexime"
    static let japanese = bundleID + ".Japanese"
    static let roman = bundleID + ".Roman"
    static let standardABC = "com.apple.keylayout.ABC"
}

// MARK: - TIS helpers

enum InputSource {
    static func currentID() -> String? {
        guard let src = TISCopyCurrentKeyboardInputSource()?.takeRetainedValue() else { return nil }
        guard let ref = TISGetInputSourceProperty(src, kTISPropertyInputSourceID) else { return nil }
        return Unmanaged<CFString>.fromOpaque(ref).takeUnretainedValue() as String
    }

    static func isCurrentStandardABC() -> Bool {
        currentID() == LeximeInputSourceID.standardABC
    }

    static func select(id: String) {
        let conditions = [kTISPropertyInputSourceID as String: id] as CFDictionary
        guard let list = TISCreateInputSourceList(conditions, false)?.takeRetainedValue()
                as? [TISInputSource],
              let source = list.first else { return }
        TISSelectInputSource(source)
    }
}

// MARK: - UserDefaults Keys

enum DefaultsKey {
    static let conversionMode = "conversionMode"
    static let developerMode = "developerMode"
}

extension Notification.Name {
    static let snippetsDidReload = Notification.Name("LeximeSnippetsDidReload")
}

/// Composite facade over `EngineContainer`, `UIServices`, and `ConfigStore`.
/// Retained for backwards compatibility; prefer the underlying containers in new code.
final class AppContext {
    /// Process-wide shared instance, assigned by `main.swift` at startup.
    static var shared: AppContext!

    let engineContainer: EngineContainer
    let ui: UIServices
    let config: ConfigStore

    init(engineContainer: EngineContainer, ui: UIServices, config: ConfigStore) {
        self.engineContainer = engineContainer
        self.ui = ui
        self.config = config
    }

    // MARK: - Forwarded properties (backwards-compatible surface)

    var engine: LexEngine? { engineContainer.engine }
    var snippetStore: LexSnippetStore? { config.snippetStore }
    var userDictPath: String { config.userDictPath }
    var supportDir: String { config.supportDir }
    var candidatePanel: CandidatePanel { ui.candidatePanel }
    var inputSourceMonitor: InputSourceMonitor { ui.inputSourceMonitor }

    func reloadSnippets() throws {
        try config.reloadSnippets()
    }

    // MARK: - Bootstrap

    static func bootstrap() -> AppContext {
        guard let resourcePath = Bundle.main.resourcePath else {
            fatalError("Lexime: Bundle.main.resourcePath is nil")
        }

        guard let appSupport = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask).first else {
            fatalError("Lexime: Cannot find Application Support directory")
        }
        let leximeDir = appSupport.appendingPathComponent("Lexime").path

        let config = ConfigStore(supportDir: leximeDir)

        // Initialize tracing (no-op unless built with --features trace)
        let libraryDir = NSSearchPathForDirectoriesInDomains(
            .libraryDirectory, .userDomainMask, true).first ?? "/tmp"
        let logDir = ((libraryDir as NSString).appendingPathComponent("Logs") as NSString)
            .appendingPathComponent("Lexime")
        try? FileManager.default.createDirectory(
            atPath: logDir, withIntermediateDirectories: true)
        traceInit(logDir: logDir)

        let settingsPath = (leximeDir as NSString).appendingPathComponent("settings.toml")
        if FileManager.default.fileExists(atPath: settingsPath) {
            do {
                try settingsLoadConfig(path: settingsPath)
                NSLog("Lexime: Custom settings loaded from %@", settingsPath)
            } catch {
                NSLog("Lexime: settings config error at %@: %@",
                      settingsPath, "\(error)")
            }
        }

        let romajiPath = (leximeDir as NSString).appendingPathComponent("romaji.toml")
        if FileManager.default.fileExists(atPath: romajiPath) {
            do {
                try romajiLoadConfig(path: romajiPath)
                NSLog("Lexime: Custom romaji loaded from %@", romajiPath)
            } catch {
                NSLog("Lexime: romaji config error at %@: %@",
                      romajiPath, "\(error)")
            }
        }

        let historyPath = (leximeDir as NSString).appendingPathComponent("user_history.lxud")
        let engineContainer = EngineContainer.load(
            resourcePath: resourcePath,
            userDictPath: config.userDictPath,
            historyPath: historyPath)

        let ui = UIServices()

        let ctx = AppContext(engineContainer: engineContainer, ui: ui, config: config)

        do {
            try config.reloadSnippets()
        } catch {
            NSLog("Lexime: snippets load error: %@", "\(error)")
        }

        ui.startMonitoring()

        return ctx
    }
}
