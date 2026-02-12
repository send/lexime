import Foundation

func testRomajiFFI() {
    print("--- Romaji FFI Tests ---")

    // Lookup: exact
    do {
        let r = lex_romaji_lookup("ka")
        defer { lex_romaji_lookup_free(r) }
        assertEqual(r.tag, 2, "ka tag=exact")
        if let ptr = r.kana {
            assertEqual(String(cString: ptr), "か", "ka → か")
        } else {
            testsFailed += 1
            print("FAIL (ka kana): expected non-null")
        }
    }

    // Lookup: prefix
    do {
        let r = lex_romaji_lookup("k")
        defer { lex_romaji_lookup_free(r) }
        assertEqual(r.tag, 1, "k tag=prefix")
    }

    // Lookup: none
    do {
        let r = lex_romaji_lookup("xyz")
        defer { lex_romaji_lookup_free(r) }
        assertEqual(r.tag, 0, "xyz tag=none")
    }

    // Lookup: exactAndPrefix
    do {
        let r = lex_romaji_lookup("chi")
        defer { lex_romaji_lookup_free(r) }
        // chi → ち, and is prefix for cha, chu, etc.
        assertTrue(r.tag == 2 || r.tag == 3, "chi tag=exact or exactAndPrefix")
        if let ptr = r.kana {
            assertEqual(String(cString: ptr), "ち", "chi → ち")
        } else {
            testsFailed += 1
            print("FAIL (chi kana): expected non-null")
        }
    }

    // Convert: basic
    do {
        let r = lex_romaji_convert("", "ka", 0)
        defer { lex_romaji_convert_free(r) }
        if let ptr = r.composed_kana {
            assertEqual(String(cString: ptr), "か", "convert ka → か")
        }
        if let ptr = r.pending_romaji {
            assertEqual(String(cString: ptr), "", "convert ka pending empty")
        }
    }

    // Convert: sokuon
    do {
        let r = lex_romaji_convert("", "kka", 0)
        defer { lex_romaji_convert_free(r) }
        if let ptr = r.composed_kana {
            assertEqual(String(cString: ptr), "っか", "convert kka → っか")
        }
    }

    // Convert: force n → ん
    do {
        let r = lex_romaji_convert("", "n", 1)
        defer { lex_romaji_convert_free(r) }
        if let ptr = r.composed_kana {
            assertEqual(String(cString: ptr), "ん", "convert n force → ん")
        }
    }

    // Convert: collapse latin+kana
    do {
        let r = lex_romaji_convert("kあ", "", 0)
        defer { lex_romaji_convert_free(r) }
        if let ptr = r.composed_kana {
            assertEqual(String(cString: ptr), "か", "convert kあ → か (collapse)")
        }
    }
}
