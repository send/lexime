import Foundation

extension LeximeInputController {

    func lookupCandidates(_ kana: String) -> [String] {
        guard let dict = sharedDict else { return [] }
        let list = lex_dict_lookup(dict, kana)
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
        NSLog("Lexime: predict('%@') → [%@]", kana, results.joined(separator: ", "))
        return results
    }

    func convertKana(_ kana: String) -> [ConversionSegment] {
        guard let dict = sharedDict else { return [] }

        let result = lex_convert(dict, sharedConn, kana)
        defer { lex_conversion_free(result) }

        guard result.len > 0, let segments = result.segments else { return [] }

        var converted: [ConversionSegment] = []
        for i in 0..<Int(result.len) {
            let reading = String(cString: segments[i].reading)
            let surface = String(cString: segments[i].surface)
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
}
