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

    /// True if `id` is one of Lexime's own input modes.
    static func isLeximeMode(_ id: String?) -> Bool {
        id == japanese || id == roman
    }
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

// MARK: - ABC auto-revert

/// Decides whether an observed switch to the standard ABC layout should be
/// automatically reverted. Pure state machine (no TIS calls) so the policy is
/// unit-testable.
///
/// Only ABC appearances that interrupt an active Lexime session are reclaimed:
/// we remember the last non-ABC source we observed, and revert only when it
/// was one of Lexime's own modes (a Lexime → ABC transition). Those are either
/// IMKit races (ESC/Eisu) or the engine's own temporary ABC switch. When ABC
/// was reached from another IME or keyboard layout instead, the user picked it
/// deliberately (e.g. from the menu bar) and we must leave it alone.
struct AbcRevertPolicy {
    /// Last observed source that was not the standard ABC layout, i.e. the
    /// source that was in use before ABC appeared.
    private(set) var lastNonAbcSourceID: String?

    /// Record the currently selected source ID. ABC and nil are ignored so
    /// `lastNonAbcSourceID` keeps naming the source in use before ABC
    /// appeared, even across repeated ABC notifications.
    mutating func observe(_ id: String?) {
        if let id, id != LeximeInputSourceID.standardABC {
            lastNonAbcSourceID = id
        }
    }

    /// True when `currentID` is standard ABC and it was reached from one of
    /// Lexime's own modes.
    func shouldRevert(currentID: String?) -> Bool {
        currentID == LeximeInputSourceID.standardABC
            && LeximeInputSourceID.isLeximeMode(lastNonAbcSourceID)
    }
}

/// Abstraction over the ABC revert retry loop so callers (ModeController,
/// InputSourceMonitor) can be unit-tested with a fake.
protocol AbcReverting: AnyObject {
    /// Leave the standard ABC layout and select `targetID`, retrying over a
    /// short window.
    func revertFromAbc(to targetID: String)
}

/// Shared retry loop for reverting an unexpected standard-ABC selection back
/// to a Lexime mode. This is the single implementation of "ABC detected →
/// retry select" — callers only differ in the target mode they pass.
///
/// Two failure shapes make a one-shot select insufficient:
/// - TISSelectInputSource can silently fail (during wake or other input
///   source transitions), so a select must be verified and retried.
/// - The IMKit flip to ABC can itself land asynchronously *after* the
///   caller's trigger (the ESC race), so early ticks may not see ABC yet.
///
/// Hence the loop re-checks every `retryInterval` for `maxAttempts` ticks and
/// re-selects the target whenever the current source is standard ABC at that
/// tick. Ticks where the source is not ABC do nothing, which means a
/// user/system switch away from ABC during the window is never overridden,
/// and a revert that already succeeded is left alone.
final class InputSourceReverter: AbcReverting {
    static let shared = InputSourceReverter()

    static let retryInterval: TimeInterval = 0.05
    static let maxAttempts = 5

    func revertFromAbc(to targetID: String) {
        tick(targetID: targetID, attempt: 0)
    }

    private func tick(targetID: String, attempt: Int) {
        if InputSource.isCurrentStandardABC() {
            InputSource.select(id: targetID)
        }
        guard attempt + 1 < Self.maxAttempts else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.retryInterval) { [weak self] in
            self?.tick(targetID: targetID, attempt: attempt + 1)
        }
    }
}
