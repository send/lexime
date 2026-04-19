import Carbon
import Cocoa

/// Handles input-source side effects around the session: switching to the
/// system ABC layout when the engine asks for it, and recovering from the
/// IMKit ESC race where macOS silently flips the user to standard ABC.
final class ModeController {

    private static let escapeRevertRetryInterval: TimeInterval = 0.05
    private static let escapeRevertRetryMaxAttempts = 5

    // IMKit mode IDs as declared in Info.plist's tsInputModeListKey.
    // These match the values IMKit passes to setValue(_:forTag:client:).
    static let japaneseModeID = "sh.send.inputmethod.Lexime.Japanese"
    static let romanModeID = "sh.send.inputmethod.Lexime.Roman"

    /// Set when ESC is pressed during composing, so commitComposition can
    /// guard against macOS switching to standard ABC.
    private var escapeCausedCommit = false

    func noteEscapeDuringCompose() {
        escapeCausedCommit = true
    }

    /// Consume and return the pending ESC-commit flag.
    func takePendingEscapeCommit() -> Bool {
        let flag = escapeCausedCommit
        escapeCausedCommit = false
        return flag
    }

    /// Switch to the standard ABC keyboard layout. Called when the engine
    /// emits `.switchToAbc` (e.g. the Eisu key in composing state).
    func selectStandardABC() {
        InputSource.select(id: LeximeInputSourceID.standardABC)
    }

    /// If the ESC race flipped us to standard ABC, retry reverting to the
    /// Lexime Japanese mode. The IMKit race fires asynchronously so we check
    /// each tick, and we re-select on subsequent ticks if still on ABC to
    /// recover from silent TISSelectInputSource failures.
    func revertToLeximeIfEscapeRaced(attempt: Int = 0) {
        guard attempt < Self.escapeRevertRetryMaxAttempts else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.escapeRevertRetryInterval) { [weak self] in
            guard let self else { return }
            if InputSource.isCurrentStandardABC() {
                InputSource.select(id: LeximeInputSourceID.japanese)
            }
            self.revertToLeximeIfEscapeRaced(attempt: attempt + 1)
        }
    }
}
