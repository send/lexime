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

    func updateConvertingMarkedText(client: IMKTextInput) {
        let attributed = NSMutableAttributedString()
        var cursorOffset = 0

        for (i, seg) in conversionSegments.enumerated() {
            let attrs: [NSAttributedString.Key: Any] = [
                .markedClauseSegment: i
            ]
            attributed.append(NSAttributedString(string: seg.surface, attributes: attrs))

            if i < activeSegmentIndex {
                cursorOffset += seg.surface.utf16.count
            }
        }

        if activeSegmentIndex < conversionSegments.count {
            let activeLen = conversionSegments[activeSegmentIndex].surface.utf16.count
            client.setMarkedText(attributed,
                                 selectionRange: NSRange(location: cursorOffset, length: activeLen),
                                 replacementRange: NSRange(location: NSNotFound, length: 0))
        }
    }
}
