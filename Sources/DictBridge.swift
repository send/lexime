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
        let list = lex_dict_predict(dict, kana, 5)
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
            let candidates = lookupCandidates(reading)
            var orderedCandidates = [surface]
            var seen: Set<String> = [surface]
            for c in candidates where seen.insert(c).inserted {
                orderedCandidates.append(c)
            }
            converted.append(ConversionSegment(
                reading: reading,
                surface: surface,
                candidates: orderedCandidates,
                selectedIndex: 0
            ))
        }
        return converted
    }

    /// Return N-best whole-sentence surfaces from Viterbi.
    /// Each element is the joined surface string of one Viterbi path.
    func convertKanaNbest(_ kana: String, n: Int = 5) -> [String] {
        guard let dict = sharedDict, !kana.isEmpty else { return [] }

        let list: LexConversionResultList
        if let history = sharedHistory {
            list = lex_convert_nbest_with_history(dict, sharedConn, history, kana, UInt32(n))
        } else {
            list = lex_convert_nbest(dict, sharedConn, kana, UInt32(n))
        }
        defer { lex_conversion_result_list_free(list) }

        guard list.len > 0, let results = list.results else { return [] }

        var surfaces: [String] = []
        for i in 0..<Int(list.len) {
            let result = results[i]
            guard result.len > 0, let segments = result.segments else { continue }
            var joined = ""
            for j in 0..<Int(result.len) {
                guard let surfacePtr = segments[j].surface else { continue }
                joined += String(cString: surfacePtr)
            }
            if !joined.isEmpty {
                surfaces.append(joined)
            }
        }
        return surfaces
    }
}
