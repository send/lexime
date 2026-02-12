import Cocoa
import InputMethodKit

extension LeximeInputController {

    /// Update inline marked text with the given display string.
    /// Uses markedClauseSegment to prevent the client's text system from
    /// applying its own transformations (e.g. Shift-triggered katakana conversion).
    func updateMarkedText(_ display: String? = nil, client: IMKTextInput) {
        let text = display ?? (composedKana + pendingRomaji)
        let len = text.utf16.count
        let attrs: [NSAttributedString.Key: Any] = [.markedClauseSegment: 0]
        let attrStr = NSAttributedString(string: text, attributes: attrs)
        client.setMarkedText(attrStr,
                             selectionRange: NSRange(location: len, length: 0),
                             replacementRange: NSRange(location: NSNotFound, length: 0))
    }
}
