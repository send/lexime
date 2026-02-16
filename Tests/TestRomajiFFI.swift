import Foundation

func testRomajiFFI() {
    print("--- Romaji FFI Tests ---")

    // Lookup: exact
    do {
        let r = romajiLookup(romaji: "ka")
        switch r {
        case .exact(let kana):
            assertEqual(kana, "か", "ka → か")
        case .exactAndPrefix(let kana):
            assertEqual(kana, "か", "ka → か (exactAndPrefix)")
        default:
            testsFailed += 1
            print("FAIL (ka): expected exact, got \(r)")
        }
    }

    // Lookup: prefix
    do {
        let r = romajiLookup(romaji: "k")
        switch r {
        case .prefix:
            testsPassed += 1
        default:
            testsFailed += 1
            print("FAIL (k): expected prefix, got \(r)")
        }
    }

    // Lookup: none
    do {
        let r = romajiLookup(romaji: "xyz")
        switch r {
        case .none:
            testsPassed += 1
        default:
            testsFailed += 1
            print("FAIL (xyz): expected none, got \(r)")
        }
    }

    // Lookup: exactAndPrefix
    do {
        let r = romajiLookup(romaji: "chi")
        switch r {
        case .exact(let kana), .exactAndPrefix(let kana):
            assertEqual(kana, "ち", "chi → ち")
        default:
            testsFailed += 1
            print("FAIL (chi): expected exact or exactAndPrefix, got \(r)")
        }
    }

    // Convert: basic
    do {
        let r = romajiConvert(kana: "", pending: "ka", force: false)
        assertEqual(r.composedKana, "か", "convert ka → か")
        assertEqual(r.pendingRomaji, "", "convert ka pending empty")
    }

    // Convert: sokuon
    do {
        let r = romajiConvert(kana: "", pending: "kka", force: false)
        assertEqual(r.composedKana, "っか", "convert kka → っか")
    }

    // Convert: force n → ん
    do {
        let r = romajiConvert(kana: "", pending: "n", force: true)
        assertEqual(r.composedKana, "ん", "convert n force → ん")
    }

    // Convert: collapse latin+kana
    do {
        let r = romajiConvert(kana: "kあ", pending: "", force: false)
        assertEqual(r.composedKana, "か", "convert kあ → か (collapse)")
    }
}
