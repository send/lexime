import Foundation

// MARK: - Romaji FFI wrappers

enum RomajiLookup {
    case none
    case prefix
    case exact(String)
    case exactAndPrefix(String)
}

func lookupRomaji(_ romaji: String) -> RomajiLookup {
    let result = lex_romaji_lookup(romaji)
    defer { lex_romaji_lookup_free(result) }
    switch result.tag {
    case 1:
        return .prefix
    case 2:
        guard let ptr = result.kana else { return .none }
        return .exact(String(cString: ptr))
    case 3:
        guard let ptr = result.kana else { return .none }
        return .exactAndPrefix(String(cString: ptr))
    default:
        return .none
    }
}

struct RomajiConvertResult {
    var composedKana: String
    var pendingRomaji: String
}

func convertRomaji(
    composedKana: String,
    pendingRomaji: String,
    force: Bool
) -> RomajiConvertResult {
    let result = lex_romaji_convert(composedKana, pendingRomaji, force ? 1 : 0)
    defer { lex_romaji_convert_free(result) }
    let kana = result.composed_kana.map { String(cString: $0) } ?? ""
    let pending = result.pending_romaji.map { String(cString: $0) } ?? ""
    return RomajiConvertResult(composedKana: kana, pendingRomaji: pending)
}

// MARK: - Unified candidate generation

extension LeximeInputController {

    func generateCandidates(_ kana: String)
        -> (surfaces: [String],
            paths: [[(reading: String, surface: String)]])
    {
        guard let dict = AppContext.shared.dict, !kana.isEmpty else { return ([], []) }

        let resp = lex_generate_candidates(
            dict,
            AppContext.shared.conn,
            AppContext.shared.history,
            kana,
            20
        )
        defer { lex_candidate_response_free(resp) }

        // Extract surfaces
        var surfaces: [String] = []
        if resp.surfaces_len > 0, let surfacePtrs = resp.surfaces {
            for i in 0..<Int(resp.surfaces_len) {
                if let ptr = surfacePtrs[i] {
                    surfaces.append(String(cString: ptr))
                }
            }
        }

        // Extract N-best paths
        var paths: [[(reading: String, surface: String)]] = []
        if resp.paths_len > 0, let results = resp.paths {
            for i in 0..<Int(resp.paths_len) {
                let result = results[i]
                guard result.len > 0, let segs = result.segments else { continue }
                var path: [(reading: String, surface: String)] = []
                for j in 0..<Int(result.len) {
                    guard let rPtr = segs[j].reading,
                          let sPtr = segs[j].surface else { continue }
                    path.append((
                        reading: String(cString: rPtr),
                        surface: String(cString: sPtr)
                    ))
                }
                paths.append(path)
            }
        }

        return (surfaces, paths)
    }
}
