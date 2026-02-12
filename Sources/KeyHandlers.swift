import Cocoa
import InputMethodKit

// MARK: - Key Codes (macOS virtual key codes)

enum Key {
    static let enter:     UInt16 = 36
    static let tab:       UInt16 = 48
    static let space:     UInt16 = 49
    static let backspace: UInt16 = 51
    static let escape:    UInt16 = 53
    static let yen:       UInt16 = 93
    static let eisu:      UInt16 = 102
    static let kana:      UInt16 = 104
    static let left:      UInt16 = 123
    static let right:     UInt16 = 124
    static let down:      UInt16 = 125
    static let up:        UInt16 = 126
}

/// Wrap-around index within a cyclic list.
func cyclicIndex(_ current: Int, delta: Int, count: Int) -> Int {
    guard count > 0 else { return 0 }
    return (current + delta + count) % count
}

/// Whether `text` is a romaji input character (letter or hyphen).
func isRomajiInput(_ text: String) -> Bool {
    if text == "-" { return true }
    guard let scalar = text.unicodeScalars.first else { return false }
    return CharacterSet.lowercaseLetters.contains(scalar)
        || CharacterSet.uppercaseLetters.contains(scalar)
}

extension LeximeInputController {

    // MARK: - Idle State

    func handleIdle(keyCode: UInt16, text: String, client: IMKTextInput) -> Bool {
        if isRomajiInput(text) {
            state = .composing
            appendAndConvert(text.lowercased(), client: client)
            return true
        }

        // Direct trie match for non-romaji chars (punctuation, etc.)
        switch lookupRomaji(text) {
        case .exact, .exactAndPrefix:
            state = .composing
            appendAndConvert(text, client: client)
            return true
        default:
            return false
        }
    }

    // MARK: - Composing State

    func handleComposing(keyCode: UInt16, text: String, client: IMKTextInput) -> Bool {
        switch keyCode {
        case Key.enter: // Enter — commit selected candidate with learning, or kana as-is
            commitCurrentState(client: client)
            return true

        case Key.space: // Space — next candidate
            if !predictionCandidates.isEmpty {
                selectedPredictionIndex = cyclicIndex(
                    selectedPredictionIndex, delta: 1,
                    count: predictionCandidates.count)
                updateMarkedText(
                    predictionCandidates[selectedPredictionIndex],
                    client: client)
                showCandidatePanel(client: client)
            }
            return true

        case Key.down: // Down arrow — next prediction candidate
            if !predictionCandidates.isEmpty {
                selectedPredictionIndex = cyclicIndex(selectedPredictionIndex, delta: 1, count: predictionCandidates.count)
                updateMarkedText(predictionCandidates[selectedPredictionIndex], client: client)
                showCandidatePanel(client: client)
            }
            return true

        case Key.up: // Up arrow — previous prediction candidate
            if !predictionCandidates.isEmpty {
                selectedPredictionIndex = cyclicIndex(selectedPredictionIndex, delta: -1, count: predictionCandidates.count)
                updateMarkedText(predictionCandidates[selectedPredictionIndex], client: client)
                showCandidatePanel(client: client)
            }
            return true

        case Key.tab: // Tab — katakana conversion
            hideCandidatePanel()
            flush()
            let katakana = composedKana.applyingTransform(.hiraganaToKatakana, reverse: false)
                ?? composedKana
            commitText(katakana, client: client)
            return true

        case Key.backspace: // Backspace
            if !pendingRomaji.isEmpty {
                pendingRomaji.removeLast()
            } else if !composedKana.isEmpty {
                composedKana.removeLast()
            }
            if composedKana.isEmpty && pendingRomaji.isEmpty {
                hideCandidatePanel()
                state = .idle
                client.setMarkedText("",
                                    selectionRange: NSRange(location: 0, length: 0),
                                    replacementRange: NSRange(location: NSNotFound, length: 0))
            } else {
                updateMarkedText(client: client)
                updateCandidates()
            }
            return true

        case Key.escape: // Escape — dismiss candidates, back to kana
            hideCandidatePanel()
            predictionCandidates = []
            selectedPredictionIndex = 0
            updateMarkedText(client: client)
            return true

        default:
            break
        }

        // z-sequences: composing 中、pendingRomaji + text が trie にマッチする場合は通す
        if !pendingRomaji.isEmpty {
            let candidate = pendingRomaji + text
            switch lookupRomaji(candidate) {
            case .exact, .exactAndPrefix, .prefix:
                appendAndConvert(text, client: client)
                return true
            case .none:
                break
            }
        }

        if isRomajiInput(text) {
            if selectedPredictionIndex > 0,
               selectedPredictionIndex < predictionCandidates.count {
                commitCurrentState(client: client)
                state = .composing
            }
            appendAndConvert(text.lowercased(), client: client)
            return true
        }

        // Direct trie match for non-romaji chars (punctuation, etc.)
        // Commit current composition, then start new one.
        if !isRomajiInput(text) {
            switch lookupRomaji(text) {
            case .exact, .exactAndPrefix:
                commitCurrentState(client: client)
                state = .composing
                appendAndConvert(text, client: client)
                return true
            default:
                break
            }
        }

        hideCandidatePanel()
        flush()
        commitComposed(client: client)
        return false
    }
}
