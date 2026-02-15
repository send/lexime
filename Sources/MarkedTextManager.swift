import Cocoa
import InputMethodKit

extension LeximeInputController {

    /// Update inline marked text with the given display string.
    /// Uses markedClauseSegment to prevent the client's text system from
    /// applying its own transformations (e.g. Shift-triggered katakana conversion).
    func updateMarkedText(_ text: String, dashed: Bool, client: IMKTextInput) {
        let len = text.utf16.count
        var attrs: [NSAttributedString.Key: Any] = [.markedClauseSegment: 0]
        if dashed {
            attrs[.underlineStyle] = NSUnderlineStyle.patternDash.rawValue | NSUnderlineStyle.single.rawValue
        }
        let attrStr = NSAttributedString(string: text, attributes: attrs)
        client.setMarkedText(attrStr,
                             selectionRange: NSRange(location: len, length: 0),
                             replacementRange: NSRange(location: NSNotFound, length: 0))
    }

    /// Display ghost text (grayed-out, no underline) as marked text.
    func showGhostText(_ text: String, client: IMKTextInput) {
        let attrs: [NSAttributedString.Key: Any] = [
            .foregroundColor: NSColor.placeholderTextColor,
        ]
        let attrStr = NSAttributedString(string: text, attributes: attrs)
        client.setMarkedText(attrStr,
                             selectionRange: NSRange(location: 0, length: 0),
                             replacementRange: NSRange(location: NSNotFound, length: 0))
    }

    /// Clear ghost text by removing marked text.
    func clearGhostText(client: IMKTextInput) {
        client.setMarkedText("",
                             selectionRange: NSRange(location: 0, length: 0),
                             replacementRange: NSRange(location: NSNotFound, length: 0))
    }
}
