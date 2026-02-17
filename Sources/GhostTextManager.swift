import Cocoa
import InputMethodKit

class GhostTextManager {

    private(set) var text: String?

    // MARK: - Display

    func showDisplay(_ text: String, client: IMKTextInput) {
        let attrs: [NSAttributedString.Key: Any] = [
            .foregroundColor: NSColor.placeholderTextColor,
            .markedClauseSegment: 0,
        ]
        let attrStr = NSAttributedString(string: text, attributes: attrs)
        client.setMarkedText(attrStr,
                             selectionRange: NSRange(location: 0, length: 0),
                             replacementRange: NSRange(location: NSNotFound, length: 0))
    }

    func clearDisplay(client: IMKTextInput) {
        client.setMarkedText("",
                             selectionRange: NSRange(location: 0, length: 0),
                             replacementRange: NSRange(location: NSNotFound, length: 0))
    }

    // MARK: - State

    func set(_ ghost: String, client: IMKTextInput) {
        text = ghost
        showDisplay(ghost, client: client)
    }

    func clear(client: IMKTextInput, updateDisplay: Bool) {
        text = nil
        if updateDisplay {
            clearDisplay(client: client)
        }
    }

    func deactivate() {
        text = nil
    }
}
