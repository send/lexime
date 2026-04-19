import Carbon
import Foundation

// MARK: - Input Source IDs

/// Runtime input source IDs for TIS API lookups (TISCreateInputSourceList etc).
/// These match the fully-qualified mode IDs declared in Info.plist's
/// tsInputModeListKey (e.g. "sh.send.inputmethod.Lexime.Japanese"). Derived
/// from Bundle.main.bundleIdentifier + suffix so they stay in sync
/// automatically if the bundle ID changes.
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
