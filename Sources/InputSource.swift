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
