import Foundation

/// Handles input-source side effects around the session: switching to the
/// system ABC layout when the engine asks for it, and recovering from the
/// IMKit ESC race where macOS silently flips the user to standard ABC.
final class ModeController {

    private let reverter: AbcReverting

    init(reverter: AbcReverting = InputSourceReverter.shared) {
        self.reverter = reverter
    }

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

    /// If the ESC race flipped us to standard ABC, revert to the Lexime
    /// *Japanese* mode: the race is an accident that interrupts active
    /// Japanese composing, so the user should land back where they were.
    /// (Contrast with InputSourceMonitor, which normalizes a bare ABC
    /// appearance to Lexime Roman.) The race fires asynchronously after the
    /// ESC commit, so the reverter's whole retry window doubles as the watch
    /// window for the flip to arrive.
    func revertToLeximeIfEscapeRaced() {
        reverter.revertFromAbc(to: LeximeInputSourceID.japanese)
    }
}
