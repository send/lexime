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
        // Tab — toggle Japanese/English submode
        if keyCode == Key.tab {
            toggleSubmode(client: client)
            return true
        }

        // English submode: add characters directly (no romaji conversion)
        if currentSubmode == .english {
            guard let scalar = text.unicodeScalars.first,
                  scalar.value >= 0x20, scalar.value < 0x7F else { return false }
            state = .composing
            didInsertBoundarySpace = false
            composedKana.append(text)
            updateMarkedText(client: client)
            return true
        }

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
            if currentSubmode == .english {
                // English-only: commit as-is without learning
                hideCandidatePanel()
                commitComposed(client: client)
            } else {
                commitCurrentState(client: client)
            }
            return true

        case Key.space: // Space
            if currentSubmode == .english {
                // English mode: insert literal space
                composedKana.append(" ")
                updateMarkedText(client: client)
                return true
            }
            if !predictionCandidates.isEmpty {
                if selectedPredictionIndex == 0 && predictionCandidates.count > 1 {
                    selectedPredictionIndex = 1
                } else {
                    selectedPredictionIndex = cyclicIndex(
                        selectedPredictionIndex, delta: 1,
                        count: predictionCandidates.count)
                }
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

        case Key.tab: // Tab — toggle Japanese/English submode
            toggleSubmode(client: client)
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
                if currentSubmode == .japanese {
                    updateCandidates()
                }
            }
            return true

        case Key.escape: // Escape — commit kana (IMKit forces commitComposition after Escape)
            hideCandidatePanel()
            flush()
            if currentSubmode == .japanese && !composedKana.isEmpty {
                recordToHistory(reading: composedKana, surface: composedKana)
            }
            predictionCandidates = []
            selectedPredictionIndex = 0
            return true

        default:
            break
        }

        // English submode: add characters directly
        if currentSubmode == .english {
            guard let scalar = text.unicodeScalars.first,
                  scalar.value >= 0x20, scalar.value < 0x7F else { return true }
            didInsertBoundarySpace = false
            composedKana.append(text)
            updateMarkedText(client: client)
            return true
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
        // Auto-commit: commit current conversion, then insert punctuation directly.
        if !isRomajiInput(text) {
            switch lookupRomaji(text) {
            case .exact, .exactAndPrefix:
                commitCurrentState(client: client)
                let result = convertRomaji(
                    composedKana: "", pendingRomaji: text, force: true)
                if !result.composedKana.isEmpty {
                    client.insertText(
                        result.composedKana,
                        replacementRange: NSRange(
                            location: NSNotFound, length: 0))
                }
                return true
            default:
                break
            }
        }

        // Unrecognized non-romaji character — add to composedKana so user can backspace.
        composedKana.append(text)
        updateMarkedText(client: client)
        updateCandidates()
        return true
    }
}
