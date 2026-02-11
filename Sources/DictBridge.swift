import Foundation

extension LeximeInputController {

    func lookupCandidates(_ kana: String) -> [String] {
        guard let dict = sharedDict else { return [] }
        let list: LexCandidateList
        if let history = sharedHistory {
            list = lex_dict_lookup_with_history(dict, history, kana)
        } else {
            list = lex_dict_lookup(dict, kana)
        }
        defer { lex_candidates_free(list) }

        guard list.len > 0, let candidates = list.candidates else { return [] }
        var results: [String] = []
        for i in 0..<Int(list.len) {
            if let surface = candidates[i].surface {
                results.append(String(cString: surface))
            }
        }
        return results
    }

    func predictCandidates(_ kana: String) -> [String] {
        guard let dict = sharedDict, !kana.isEmpty else { return [] }
        let list = lex_dict_predict_ranked(dict, sharedHistory, kana, 9)
        defer { lex_candidates_free(list) }

        guard list.len > 0, let candidates = list.candidates else { return [] }
        var seen = Set<String>()
        var results: [String] = []
        for i in 0..<Int(list.len) {
            if let surface = candidates[i].surface {
                let s = String(cString: surface)
                if !s.isEmpty && seen.insert(s).inserted {
                    results.append(s)
                }
            }
        }
        return results
    }

    /// Fast Viterbi conversion — returns segments with only the 1-best surface
    /// (no per-segment dictionary lookup).  Candidates are loaded lazily via
    /// `ensureCandidatesLoaded(segmentIndex:)` when the user enters multi-segment mode.
    func convertKana(_ kana: String) -> [ConversionSegment] {
        guard let dict = sharedDict else { return [] }

        let result: LexConversionResult
        if let history = sharedHistory {
            result = lex_convert_with_history(dict, sharedConn, history, kana)
        } else {
            result = lex_convert(dict, sharedConn, kana)
        }
        defer { lex_conversion_free(result) }

        guard result.len > 0, let segments = result.segments else { return [] }

        var converted: [ConversionSegment] = []
        for i in 0..<Int(result.len) {
            guard let readingPtr = segments[i].reading,
                  let surfacePtr = segments[i].surface else { continue }
            let reading = String(cString: readingPtr)
            let surface = String(cString: surfacePtr)
            converted.append(ConversionSegment(
                reading: reading,
                surface: surface,
                candidates: [surface],
                selectedIndex: 0
            ))
        }
        return converted
    }

    /// Combined Viterbi N-best: returns 1-best segments AND N-best joined surfaces
    /// in a single FFI call, replacing separate `convertKana` + `convertKanaNbest`.
    func convertKanaCombined(_ kana: String, n: Int = 5)
        -> (segments: [ConversionSegment], nbestSurfaces: [String])
    {
        guard let dict = sharedDict, !kana.isEmpty else { return ([], []) }

        let list: LexConversionResultList
        if let history = sharedHistory {
            list = lex_convert_nbest_with_history(dict, sharedConn, history, kana, UInt32(n))
        } else {
            list = lex_convert_nbest(dict, sharedConn, kana, UInt32(n))
        }
        defer { lex_conversion_result_list_free(list) }

        guard list.len > 0, let results = list.results else { return ([], []) }

        // Extract 1-best segments (first result) without per-segment candidate lookup
        var segments: [ConversionSegment] = []
        let firstResult = results[0]
        if firstResult.len > 0, let firstSegments = firstResult.segments {
            for j in 0..<Int(firstResult.len) {
                guard let readingPtr = firstSegments[j].reading,
                      let surfacePtr = firstSegments[j].surface else { continue }
                let reading = String(cString: readingPtr)
                let surface = String(cString: surfacePtr)
                segments.append(ConversionSegment(
                    reading: reading,
                    surface: surface,
                    candidates: [surface],
                    selectedIndex: 0
                ))
            }
        }

        // Extract all N-best joined surfaces
        var surfaces: [String] = []
        for i in 0..<Int(list.len) {
            let result = results[i]
            guard result.len > 0, let segs = result.segments else { continue }
            let parts = (0..<Int(result.len)).compactMap { j -> String? in
                guard let ptr = segs[j].surface else { return nil }
                return String(cString: ptr)
            }
            let joined = parts.joined()
            if !joined.isEmpty {
                surfaces.append(joined)
            }
        }

        return (segments, surfaces)
    }

    /// Lazily load dictionary candidates for a segment that only has its
    /// Viterbi surface.  Call this before the user needs to browse candidates.
    func ensureCandidatesLoaded(segmentIndex: Int) {
        guard segmentIndex < conversionSegments.count,
              conversionSegments[segmentIndex].candidates.count <= 1 else { return }
        let seg = conversionSegments[segmentIndex]
        let looked = lookupCandidates(seg.reading)
        var ordered = [seg.surface]
        var seen: Set<String> = [seg.surface]
        for c in looked where seen.insert(c).inserted {
            ordered.append(c)
        }
        conversionSegments[segmentIndex].candidates = ordered
    }
}
