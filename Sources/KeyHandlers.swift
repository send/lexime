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
    static let f7:        UInt16 = 98
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
        if let candidates = Self.punctuationCandidates[text] {
            composePunctuation(candidates, client: client)
            return true
        }

        if isRomajiInput(text) {
            state = .composing
            appendAndConvert(text.lowercased(), client: client)
            return true
        }

        return false
    }

    // MARK: - Composing State

    func handleComposing(keyCode: UInt16, text: String, client: IMKTextInput) -> Bool {
        switch keyCode {
        case Key.enter: // Enter — commit selected prediction, or kana as-is
            hideCandidatePanel()
            if !predictionCandidates.isEmpty {
                let idx = min(selectedPredictionIndex, predictionCandidates.count - 1)
                commitText(predictionCandidates[idx], client: client)
            } else {
                flush()
                commitComposed(client: client)
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

        case Key.tab: // Tab — accept selected prediction candidate
            if !predictionCandidates.isEmpty {
                hideCandidatePanel()
                let idx = min(selectedPredictionIndex, predictionCandidates.count - 1)
                commitText(predictionCandidates[idx], client: client)
                return true
            }
            return false

        case Key.space: // Space — convert kana (or next candidate for punctuation)
            if isPunctuationComposing && predictionCandidates.count > 1 {
                selectedPredictionIndex = cyclicIndex(selectedPredictionIndex, delta: 1, count: predictionCandidates.count)
                composedKana = predictionCandidates[selectedPredictionIndex]
                updateMarkedText(composedKana, client: client)
                showCandidatePanel(client: client)
                return true
            }
            if sharedDict == nil {
                hideCandidatePanel()
                flush()
                commitComposed(client: client)
                return true
            }
            performConversion(client: client)
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
                updatePredictions()
            }
            return true

        case Key.escape: // Escape — dismiss predictions, keep kana; second Esc cancels
            if !predictionCandidates.isEmpty {
                hideCandidatePanel()
                predictionCandidates = []
                selectedPredictionIndex = 0
                updateMarkedText(client: client)
            } else {
                hideCandidatePanel()
                resetState()
                client.setMarkedText("",
                                     selectionRange: NSRange(location: 0, length: 0),
                                     replacementRange: NSRange(location: NSNotFound, length: 0))
            }
            return true

        case Key.f7: // F7 — katakana conversion
            hideCandidatePanel()
            flush()
            let katakana = composedKana.applyingTransform(.hiraganaToKatakana, reverse: false)
                ?? composedKana
            commitText(katakana, client: client)
            return true

        default:
            break
        }

        // z-sequences: composing 中、pendingRomaji + text が trie にマッチする場合は通す
        if !pendingRomaji.isEmpty {
            let candidate = pendingRomaji + text
            switch RomajiTrie.shared.lookup(candidate) {
            case .exact, .exactAndPrefix, .prefix:
                appendAndConvert(text, client: client)
                return true
            case .none:
                break
            }
        }

        if let candidates = Self.punctuationCandidates[text] {
            hideCandidatePanel()
            flush()
            commitComposed(client: client)
            composePunctuation(candidates, client: client)
            return true
        }

        if isRomajiInput(text) {
            appendAndConvert(text.lowercased(), client: client)
            return true
        }

        hideCandidatePanel()
        flush()
        commitComposed(client: client)
        return false
    }

    // MARK: - Converting State (multi-segment)

    func handleConverting(keyCode: UInt16, text: String, client: IMKTextInput) -> Bool {
        switch keyCode {
        case Key.enter: // Enter — confirm all segments
            commitConversion(client: client)
            return true

        case Key.space: // Space — next candidate for active segment
            selectSegmentCandidate(delta: 1, client: client)
            return true

        case Key.backspace, Key.escape: // Backspace or Escape — back to composing with original kana
            hideCandidatePanel()
            composedKana = originalKana
            conversionSegments = []
            activeSegmentIndex = 0
            state = .composing
            updateMarkedText(client: client)
            return true

        case Key.left: // Left arrow — previous segment
            if activeSegmentIndex > 0 {
                activeSegmentIndex -= 1
                updateConvertingMarkedText(client: client)
                showCandidatePanel(client: client)
            }
            return true

        case Key.right: // Right arrow — next segment
            if activeSegmentIndex < conversionSegments.count - 1 {
                activeSegmentIndex += 1
                updateConvertingMarkedText(client: client)
                showCandidatePanel(client: client)
            }
            return true

        case Key.up: // Up arrow — previous candidate
            selectSegmentCandidate(delta: -1, client: client)
            return true

        case Key.down: // Down arrow — next candidate
            selectSegmentCandidate(delta: 1, client: client)
            return true

        default:
            break
        }

        // U3: Number keys 1-9 for direct candidate selection
        if let num = text.first?.wholeNumberValue, num >= 1, num <= 9 {
            selectSegmentCandidate(delta: num - 1, absolute: true, client: client)
            return true
        }

        // Alphabetic: confirm all segments and start new input
        if isRomajiInput(text) {
            commitConversion(client: client)
            state = .composing
            appendAndConvert(text.lowercased(), client: client)
            return true
        }

        // Punctuation: confirm all segments, then insert
        if let candidates = Self.punctuationCandidates[text] {
            commitConversion(client: client)
            composePunctuation(candidates, client: client)
            return true
        }

        // Other: confirm and pass through
        commitConversion(client: client)
        return false
    }

    // MARK: - Segment Candidate Selection

    /// Select a candidate for the active segment and refresh the UI.
    /// `delta` is +1/-1 for next/previous; absolute index if `absolute` is true.
    func selectSegmentCandidate(delta: Int, absolute: Bool = false, client: IMKTextInput) {
        guard activeSegmentIndex < conversionSegments.count else { return }
        let seg = conversionSegments[activeSegmentIndex]
        guard !seg.candidates.isEmpty else { return }
        let newIdx: Int
        if absolute {
            guard delta >= 0 && delta < seg.candidates.count else { return }
            newIdx = delta
        } else {
            newIdx = cyclicIndex(seg.selectedIndex, delta: delta, count: seg.candidates.count)
        }
        conversionSegments[activeSegmentIndex].selectedIndex = newIdx
        conversionSegments[activeSegmentIndex].surface = seg.candidates[newIdx]
        updateConvertingMarkedText(client: client)
        showCandidatePanel(client: client)
    }

    // MARK: - Kana Conversion

    /// Flush pending romaji, run Viterbi conversion on the composed kana, build
    /// a unified candidate list (predictions, N-best, lookups, raw kana), and
    /// transition to the `.converting` state.  If conversion produces no segments
    /// the kana is committed as-is.
    func performConversion(client: IMKTextInput) {
        hideCandidatePanel()
        flush()
        let segments = convertKana(composedKana)
        if segments.isEmpty {
            commitComposed(client: client)
        } else {
            originalKana = composedKana
            viterbiSegments = segments

            // Build single-segment view: predictions, N-best Viterbi, lookups
            let viterbiSurface = segments.map { $0.surface }.joined()
            var candidates: [String] = []
            var seen = Set<String>()
            // Predictions first (skip raw kana — added at end as fallback)
            for pred in predictionCandidates where pred != composedKana && seen.insert(pred).inserted {
                candidates.append(pred)
            }
            // N-best Viterbi surfaces (includes 1-best as first element)
            let nbestSurfaces = convertKanaNbest(composedKana, n: 5)
            for surface in nbestSurfaces where seen.insert(surface).inserted {
                candidates.append(surface)
            }
            // Viterbi 1-best as fallback (in case N-best was empty)
            if seen.insert(viterbiSurface).inserted {
                candidates.append(viterbiSurface)
            }
            // Lookup candidates for full reading
            for c in lookupCandidates(composedKana) where seen.insert(c).inserted {
                candidates.append(c)
            }
            // Raw kana as last resort
            if seen.insert(composedKana).inserted {
                candidates.append(composedKana)
            }
            let surface = candidates.first ?? viterbiSurface

            conversionSegments = [ConversionSegment(
                reading: composedKana,
                surface: surface,
                candidates: candidates,
                selectedIndex: 0
            )]
            activeSegmentIndex = 0
            state = .converting
            updateConvertingMarkedText(client: client)
            showCandidatePanel(client: client)
        }
    }

    // MARK: - Segment Boundary Adjustment (U2)

    func handleSegmentBoundaryAdjust(shrink: Bool, client: IMKTextInput) -> Bool {
        // Expand from single-segment to multi-segment mode on first boundary adjustment
        if conversionSegments.count == 1
            && conversionSegments[0].reading == originalKana
            && viterbiSegments.count > 1 {
            conversionSegments = viterbiSegments
            activeSegmentIndex = 0
            updateConvertingMarkedText(client: client)
            showCandidatePanel(client: client)
            return true
        }

        guard activeSegmentIndex < conversionSegments.count else { return true }
        let activeReading = conversionSegments[activeSegmentIndex].reading

        if shrink {
            guard activeReading.count > 1 else { return true }
            let newReading = String(activeReading.dropLast())
            let movedChar = String(activeReading.suffix(1))
            var nextReading = movedChar
            if activeSegmentIndex + 1 < conversionSegments.count {
                nextReading += conversionSegments[activeSegmentIndex + 1].reading
            }
            let newActive = convertKana(newReading)
            let newNext = convertKana(nextReading)
            rebuildSegments(activeReplace: newActive, nextReplace: newNext)
        } else {
            guard activeSegmentIndex + 1 < conversionSegments.count else { return true }
            let nextReading = conversionSegments[activeSegmentIndex + 1].reading
            guard !nextReading.isEmpty else { return true }
            let expandedReading = activeReading + String(nextReading.prefix(1))
            let remainderReading = String(nextReading.dropFirst())
            let newActive = convertKana(expandedReading)
            let newNext = remainderReading.isEmpty ? [] : convertKana(remainderReading)
            rebuildSegments(activeReplace: newActive, nextReplace: newNext)
        }
        updateConvertingMarkedText(client: client)
        showCandidatePanel(client: client)
        return true
    }

    private func rebuildSegments(activeReplace: [ConversionSegment], nextReplace: [ConversionSegment]) {
        var newSegments = Array(conversionSegments[..<activeSegmentIndex])
        newSegments += activeReplace
        newSegments += nextReplace
        let skipCount = activeSegmentIndex + (activeSegmentIndex + 1 < conversionSegments.count ? 2 : 1)
        if skipCount < conversionSegments.count {
            newSegments += Array(conversionSegments[skipCount...])
        }
        conversionSegments = newSegments
    }
}
