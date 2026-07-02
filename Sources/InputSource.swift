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
    /// Escort an already-selected standard ABC back to `targetID`. The
    /// session ends as soon as a non-ABC source is observed (either the
    /// revert took effect or the user moved elsewhere).
    func revertFromAbc(to targetID: String)

    /// Watch for standard ABC to appear (an asynchronous IMKit flip that may
    /// land after the trigger, e.g. the ESC race) and revert it to
    /// `targetID`. Non-ABC ticks before the first ABC observation keep the
    /// watch alive; once ABC has been handled, the session ends on the next
    /// non-ABC observation, like `revertFromAbc(to:)`.
    func revertWhenAbcAppears(to targetID: String)
}

/// Shared retry loop for reverting an unexpected standard-ABC selection back
/// to a Lexime mode. This is the single implementation of "ABC detected →
/// retry select" — callers only differ in the target mode and in whether ABC
/// is expected to be current already (`revertFromAbc`) or to appear shortly
/// (`revertWhenAbcAppears`).
///
/// Two failure shapes make a one-shot select insufficient:
/// - TISSelectInputSource can silently fail (during wake or other input
///   source transitions), so a select must be verified and retried.
/// - The IMKit flip to ABC can itself land asynchronously *after* the
///   caller's trigger (the ESC race), so early ticks may not see ABC yet.
///
/// Each session validates its claim on every tick: it selects the target
/// only while the current source is standard ABC, and once ABC has been
/// observed (or was expected up front), the first non-ABC observation ends
/// the session — the revert either succeeded or the user moved somewhere
/// else, and a later return to ABC is a *new* transition owned by whoever
/// observes it (InputSourceMonitor's policy), never by a stale session.
final class InputSourceReverter: AbcReverting {
    static let shared = InputSourceReverter()

    /// Interval between ownership checks.
    static let retryInterval: TimeInterval = 0.05
    /// Total checks per session: one immediate plus retries, so the last
    /// check lands at (maxAttempts - 1) * retryInterval = 0.25s. This keeps
    /// the ESC race watch window at least as wide as the pre-consolidation
    /// ModeController loop, which checked through +0.25s.
    static let maxAttempts = 6

    private let isCurrentStandardABC: () -> Bool
    private let selectSource: (String) -> Void
    private let scheduleTick: (@escaping () -> Void) -> Void

    /// Dependencies default to the real TIS calls and a main-queue delayed
    /// dispatch; tests inject fakes to drive the loop synchronously.
    init(
        isCurrentStandardABC: @escaping () -> Bool = InputSource.isCurrentStandardABC,
        selectSource: @escaping (String) -> Void = { InputSource.select(id: $0) },
        scheduleTick: @escaping (@escaping () -> Void) -> Void = { work in
            DispatchQueue.main.asyncAfter(
                deadline: .now() + InputSourceReverter.retryInterval, execute: work)
        }
    ) {
        self.isCurrentStandardABC = isCurrentStandardABC
        self.selectSource = selectSource
        self.scheduleTick = scheduleTick
    }

    func revertFromAbc(to targetID: String) {
        tick(targetID: targetID, attempt: 0, hasSeenAbc: true)
    }

    func revertWhenAbcAppears(to targetID: String) {
        tick(targetID: targetID, attempt: 0, hasSeenAbc: false)
    }

    private func tick(targetID: String, attempt: Int, hasSeenAbc: Bool) {
        var hasSeenAbc = hasSeenAbc
        if isCurrentStandardABC() {
            selectSource(targetID)
            hasSeenAbc = true
        } else if hasSeenAbc {
            // Session over: the revert took effect, or the user moved off ABC
            // (possibly before the session even started). Stop scheduling so
            // a deliberate re-selection of ABC within the window is not
            // stolen by this stale session.
            return
        }
        guard attempt + 1 < Self.maxAttempts else { return }
        scheduleTick { [weak self] in
            self?.tick(targetID: targetID, attempt: attempt + 1, hasSeenAbc: hasSeenAbc)
        }
    }
}
